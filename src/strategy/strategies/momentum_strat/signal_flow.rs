use chrono::Duration;
use rust_decimal_macros::dec;
use tracing::{debug, info};

use super::{
    ActivePosition, DateTime, Decimal, Domain, EntrySignal, EventContext, ExitReason, HashMap,
    MomentumStrategy, OrderType, PendingOrder, Side, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyOrderIntent, TimeInForce, Utc,
};

impl MomentumStrategy {
    /// Calculate momentum from price history
    pub(super) fn calculate_momentum(&self, symbol: &str) -> Option<Decimal> {
        let history = self.price_history.get(symbol)?;
        if history.len() < 2 {
            return None;
        }

        let now = Utc::now();
        let lookback = Duration::seconds(self.config.detector_config.long_window_secs);
        let cutoff = now - lookback;

        let old_price = history
            .iter()
            .rev()
            .find(|(ts, _)| *ts < cutoff)
            .or_else(|| history.first())?
            .1;
        let current_price = history.last()?.1;

        if old_price.is_zero() {
            return None;
        }

        Some((current_price - old_price) / old_price)
    }

    /// Estimate fair value based on momentum (piecewise sigmoid-like scaling)
    pub(super) fn estimate_fair_value(&self, momentum: Decimal) -> Decimal {
        let base_prob = dec!(0.50);
        let abs_momentum = momentum.abs();
        let momentum_factor = if abs_momentum < dec!(0.001) {
            abs_momentum * dec!(50)
        } else if abs_momentum < dec!(0.005) {
            dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40)
        } else {
            dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30)
        };

        (base_prob + momentum_factor).min(dec!(0.90))
    }

    /// Check exit conditions for a position
    pub(super) fn check_exit(
        &self,
        pos: &ActivePosition,
        current_bid: Decimal,
    ) -> Option<ExitReason> {
        let pnl_pct = pos.pnl_pct(current_bid);

        if pnl_pct >= self.config.take_profit_pct {
            return Some(ExitReason::TakeProfit);
        }

        if pnl_pct <= -self.config.stop_loss_pct {
            return Some(ExitReason::StopLoss);
        }

        if pos.highest_price > pos.entry_price && current_bid < pos.highest_price {
            let drop = (pos.highest_price - current_bid) / pos.highest_price;
            if drop >= self.config.trailing_stop_pct {
                return Some(ExitReason::TrailingStop);
            }
        }

        if pos.time_remaining() < self.config.exit_before_resolution_secs as i64 {
            return Some(ExitReason::TimeExit);
        }

        None
    }

    /// Process Binance price update
    pub(super) fn on_binance_price(
        &mut self,
        symbol: &str,
        price: Decimal,
        timestamp: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        if !self.config.symbols.contains(&symbol.to_string()) {
            return actions;
        }

        let history = self.price_history.entry(symbol.to_string()).or_default();
        history.push((timestamp, price));

        let cutoff = timestamp - Duration::seconds(300);
        history.retain(|(ts, _)| *ts > cutoff);

        self.last_binance_prices
            .insert(symbol.to_string(), (price, timestamp));

        if let Some(momentum) = self.calculate_momentum(symbol) {
            if momentum.abs() >= self.config.min_move_pct {
                if let Some(event) = self.find_event_for_symbol(symbol) {
                    let side = if momentum > Decimal::ZERO {
                        Side::Up
                    } else {
                        Side::Down
                    };

                    if self.can_enter(symbol, side, event) {
                        let fair_value = self.estimate_fair_value(momentum);

                        let signal = EntrySignal {
                            symbol: symbol.to_string(),
                            side,
                            cex_move_pct: momentum,
                            pm_price: dec!(0.50),
                            edge: fair_value - dec!(0.50),
                            event_end_time: event.end_time,
                            token_id: match side {
                                Side::Up => event.up_token_id.clone(),
                                Side::Down => event.down_token_id.clone(),
                            },
                        };

                        let _ = signal;
                        actions.push(StrategyAction::LogEvent {
                            event: StrategyEvent::new(
                                StrategyEventType::SignalDetected,
                                format!(
                                    "Momentum signal: {} {:?} ({:.2}%)",
                                    symbol,
                                    side,
                                    momentum * dec!(100)
                                ),
                            ),
                        });
                    }
                }
            }
        }

        actions
    }

    /// Find event for a symbol
    pub(super) fn find_event_for_symbol(&self, symbol: &str) -> Option<&EventContext> {
        self.active_events
            .values()
            .find(|event| event.symbol == symbol)
    }

    /// Check if we can enter a position
    pub(super) fn can_enter(&self, symbol: &str, _side: Side, _event: &EventContext) -> bool {
        if self.positions.len() >= self.config.max_positions {
            return false;
        }

        if self
            .positions
            .values()
            .any(|position| position.symbol == symbol)
        {
            return false;
        }

        if self.in_cooldown(symbol) {
            return false;
        }

        true
    }

    /// Create entry order
    pub(super) fn create_entry_order(&mut self, signal: EntrySignal) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        if signal.pm_price > self.config.max_entry_price {
            debug!(
                "PM price {:.2}¢ > max {:.2}¢, skipping",
                signal.pm_price * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
            return actions;
        }

        if signal.edge < self.config.min_edge {
            debug!(
                "Edge {:.2}% < min {:.2}%, skipping",
                signal.edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return actions;
        }

        let client_order_id = format!("{}-entry-{}", self.config.id, Utc::now().timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                symbol: signal.symbol.clone(),
                side: signal.side,
                is_entry: true,
                signal: Some(signal.clone()),
            },
        );

        info!(
            "ENTRY: {} {:?} @ {:.2}¢ (CEX: {:.2}%, edge: {:.2}%)",
            signal.symbol,
            signal.side,
            signal.pm_price * dec!(100),
            signal.cex_move_pct * dec!(100),
            signal.edge * dec!(100)
        );

        actions.push(StrategyAction::SubmitIntent {
            intent: self.submit_intent(
                client_order_id,
                self.market_slug_for_token(&signal.token_id, &signal.symbol),
                signal.token_id,
                signal.side,
                true,
                self.config.shares_per_trade,
                signal.pm_price,
                5,
            ),
        });

        actions
    }

    /// Create exit order
    pub(super) fn create_exit_order(
        &mut self,
        symbol: &str,
        price: Decimal,
        reason: ExitReason,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let Some(pos) = self.positions.get(symbol).cloned() else {
            return actions;
        };

        let pnl_pct = pos.pnl_pct(price);

        info!(
            "EXIT: {} {:?} @ {:.2}¢ - {} (P&L: {:.2}%)",
            symbol,
            pos.side,
            price * dec!(100),
            reason,
            pnl_pct * dec!(100)
        );

        let client_order_id = format!("{}-exit-{}", self.config.id, Utc::now().timestamp_millis());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                client_order_id: client_order_id.clone(),
                symbol: symbol.to_string(),
                side: pos.side,
                is_entry: false,
                signal: None,
            },
        );

        actions.push(StrategyAction::SubmitIntent {
            intent: self.submit_intent(
                client_order_id,
                self.market_slug_for_token(&pos.token_id, symbol),
                pos.token_id.clone(),
                pos.side,
                false,
                pos.shares,
                price,
                10,
            ),
        });

        actions
    }

    pub(super) fn market_slug_for_token(&self, token_id: &str, fallback: &str) -> String {
        self.active_events
            .values()
            .find(|event| event.up_token_id == token_id || event.down_token_id == token_id)
            .map(|event| event.event_id.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    pub(super) fn submit_intent(
        &self,
        client_order_id: String,
        market_slug: String,
        token_id: String,
        side: Side,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
        priority: u8,
    ) -> StrategyOrderIntent {
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
}
