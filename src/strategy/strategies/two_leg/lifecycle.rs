use tracing::{debug, info, warn};

use super::{
    AlertLevel, Decimal, Domain, DumpSignal, HashMap, OrderType, PendingOrder, Side,
    StrategyAction, StrategyEvent, StrategyEventType, StrategyOrderIntent, TimeInForce,
    TwoLegState, TwoLegStrategy, Utc,
};

impl TwoLegStrategy {
    /// Process dump signal and create the first leg entry.
    pub(super) fn handle_dump_signal(&mut self, signal: DumpSignal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        if !signal.spread_ok(self.config.dump_config.max_spread_bps) {
            debug!(
                "Signal rejected: spread {} > max {}",
                signal.spread_bps, self.config.dump_config.max_spread_bps
            );
            return actions;
        }

        if let Some(remaining) = self.seconds_remaining() {
            if remaining < self.config.min_time_remaining_secs as i64 {
                debug!("Signal rejected: only {}s remaining", remaining);
                return actions;
            }
        }

        let Some(token_id) = self.token_id(signal.side) else {
            return actions;
        };

        let client_order_id = format!("{}-leg1-{}", self.config.id, Utc::now().timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                order_id: None,
                side: signal.side,
                is_leg1: true,
            },
        );

        self.state = TwoLegState::Leg1Pending;

        info!(
            "Entering Leg1: {} {} shares @ {}",
            signal.side, self.config.shares, signal.trigger_price
        );

        actions.push(StrategyAction::SubmitIntent {
            intent: self.submit_intent(
                client_order_id,
                token_id.to_string(),
                signal.side,
                true,
                self.config.shares,
                signal.trigger_price,
                10,
            ),
        });

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(StrategyEventType::EntryTriggered, "Leg1 entry triggered")
                .with_data("side", signal.side.to_string())
                .with_data("price", signal.trigger_price.to_string())
                .with_data(
                    "drop_pct",
                    (signal.drop_pct * Decimal::from(100)).to_string(),
                ),
        });

        actions
    }

    /// Check whether the opposite side can complete the arbitrage cycle.
    pub(super) fn check_leg2_opportunity(&self) -> Option<(Side, Decimal)> {
        let ctx = self.current_cycle.as_ref()?;
        let opposite_side = ctx.leg1_side.opposite();
        let opposite_quote = match opposite_side {
            Side::Up => self.last_up_quote.as_ref(),
            Side::Down => self.last_down_quote.as_ref(),
        };
        let ask = opposite_quote?.best_ask?;

        self.detector
            .check_leg2_condition(ctx.leg1_price, ask)
            .then_some((opposite_side, ask))
    }

    /// Enter the second leg once the opposite side becomes attractive enough.
    pub(super) fn enter_leg2(&mut self, side: Side, price: Decimal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let Some(ctx) = &self.current_cycle else {
            return actions;
        };

        let Some(token_id) = self.token_id(side) else {
            return actions;
        };

        let client_order_id = format!("{}-leg2-{}", self.config.id, Utc::now().timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                order_id: None,
                side,
                is_leg1: false,
            },
        );

        self.state = TwoLegState::Leg2Pending;

        info!(
            "Entering Leg2: {} {} shares @ {}",
            side, ctx.leg1_shares, price
        );

        actions.push(StrategyAction::SubmitIntent {
            intent: self.submit_intent(
                client_order_id,
                token_id.to_string(),
                side,
                true,
                ctx.leg1_shares,
                price,
                10,
            ),
        });

        actions
    }

    pub(super) fn submit_intent(
        &self,
        client_order_id: String,
        token_id: String,
        side: Side,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
        priority: u8,
    ) -> StrategyOrderIntent {
        let market_slug = self
            .current_event
            .as_ref()
            .map(|event| event.event_id.clone())
            .unwrap_or_else(|| token_id.clone());

        StrategyOrderIntent {
            client_order_id,
            domain: Domain::Crypto,
            market_slug,
            token_id,
            side,
            is_buy,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority,
            metadata: HashMap::new(),
        }
    }

    /// Force the second leg near event expiry or abort if the opposite side is unavailable.
    pub(super) fn force_leg2_or_abort(&mut self) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let Some(ctx) = &self.current_cycle else {
            return actions;
        };

        let opposite_side = ctx.leg1_side.opposite();
        let opposite_quote = match opposite_side {
            Side::Up => self.last_up_quote.as_ref(),
            Side::Down => self.last_down_quote.as_ref(),
        };

        if let Some(quote) = opposite_quote {
            if let Some(ask) = quote.best_ask {
                let forced_price = ask * (Decimal::ONE + self.config.dump_config.slippage_buffer);
                warn!("Forcing Leg2 at {}", forced_price);
                return self.enter_leg2(opposite_side, forced_price);
            }
        }

        self.abort_cycle("No quote for forced Leg2");
        actions.push(StrategyAction::Alert {
            level: AlertLevel::Warning,
            message: "Cycle aborted: No quote for forced Leg2".to_string(),
        });

        actions
    }

    /// Abort the active arbitrage cycle and clear open cycle state.
    pub(super) fn abort_cycle(&mut self, reason: &str) {
        warn!("Aborting cycle: {}", reason);

        self.state = TwoLegState::Abort;
        self.current_cycle = None;
        self.positions.clear();
    }

    /// Return the strategy to its idle monitoring state.
    pub(super) fn transition_to_idle(&mut self) {
        self.state = TwoLegState::Idle;
        self.current_cycle = None;
        self.current_event = None;
        self.positions.clear();
        self.detector.reset(None);
        debug!("Transitioned to IDLE");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    use super::super::{EventContext, TwoLegConfig};

    #[test]
    fn handle_dump_signal_emits_submit_intent() {
        let mut strategy = TwoLegStrategy::new(TwoLegConfig::default());
        strategy.current_event = Some(EventContext {
            event_id: "event-1".to_string(),
            up_token_id: "token-up".to_string(),
            down_token_id: "token-down".to_string(),
            end_time: Utc::now() + chrono::Duration::seconds(120),
            start_time: Utc::now(),
        });

        let actions = strategy.handle_dump_signal(DumpSignal {
            event_id: Some("event-1".to_string()),
            side: Side::Up,
            trigger_price: dec!(0.42),
            reference_price: dec!(0.48),
            drop_pct: dec!(0.12),
            spread_bps: 0,
            timestamp: Utc::now(),
        });

        match actions.first() {
            Some(StrategyAction::SubmitIntent { intent }) => {
                assert_eq!(intent.domain, Domain::Crypto);
                assert_eq!(intent.market_slug, "event-1");
                assert_eq!(intent.token_id, "token-up");
                assert!(intent.is_buy);
                assert_eq!(intent.shares, strategy.config.shares);
            }
            other => panic!("expected submit intent, got {other:?}"),
        }
    }
}
