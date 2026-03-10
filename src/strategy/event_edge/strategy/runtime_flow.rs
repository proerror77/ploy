use super::{
    sanitize_component, DateTime, Decimal, Domain, EventEdgeScan, EventEdgeStrategy, HashMap,
    OrderStatus, OrderType, PendingEventEdgeOrder, PositionInfo, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyOrderIntent, TimeInForce, TradeDecision, Utc, EVENT_EDGE_PRIORITY,
    EVENT_EDGE_STRATEGY_NAME,
};

impl EventEdgeStrategy {
    pub(super) fn has_live_interest_in_token(&self, token_id: &str) -> bool {
        self.positions.contains_key(token_id)
            || self
                .pending_orders
                .values()
                .any(|pending| pending.token_id == token_id)
    }

    pub(super) fn allowed_notional_with_reservations(&self, amount: Decimal) -> bool {
        self.core.state.daily_spend_usd + self.reserved_notional_usd + amount
            <= self.core.cfg.max_daily_spend_usd
    }

    pub(super) fn reserve_pending_order(
        &mut self,
        decision: &TradeDecision,
        client_order_id: String,
    ) -> PendingEventEdgeOrder {
        let reserved_notional_usd = decision.limit_price * Decimal::from(decision.shares);
        let pending = PendingEventEdgeOrder {
            client_order_id: client_order_id.clone(),
            event_id: decision.event_id.clone(),
            outcome: decision.outcome.clone(),
            token_id: decision.token_id.clone(),
            condition_id: decision.condition_id.clone(),
            market_slug: decision.market_slug.clone(),
            side: decision.side,
            shares: decision.shares,
            limit_price: decision.limit_price,
            reserved_notional_usd,
        };
        self.reserved_notional_usd += reserved_notional_usd;
        self.pending_orders.insert(client_order_id, pending.clone());
        pending
    }

    pub(super) fn release_pending_order(
        &mut self,
        client_order_id: &str,
    ) -> Option<PendingEventEdgeOrder> {
        let pending = self.pending_orders.remove(client_order_id)?;
        self.reserved_notional_usd =
            (self.reserved_notional_usd - pending.reserved_notional_usd).max(Decimal::ZERO);
        Some(pending)
    }

    pub(super) fn build_signal_event(
        &self,
        decision: &TradeDecision,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> StrategyEvent {
        StrategyEvent::new(
            StrategyEventType::SignalDetected,
            format!(
                "event_edge signal event={} outcome={} ask={:.4} edge={:.4} p_true={:.4}",
                decision.event_id,
                decision.outcome,
                decision.limit_price,
                decision.edge,
                decision.p_true
            ),
        )
        .with_data("event_id", &decision.event_id)
        .with_data("event_title", &scan.event_title)
        .with_data("outcome", &decision.outcome)
        .with_data("token_id", &decision.token_id)
        .with_data("market_slug", &decision.market_slug)
        .with_data("limit_price", decision.limit_price.to_string())
        .with_data("edge", decision.edge.to_string())
        .with_data("p_true", decision.p_true.to_string())
        .with_data("net_ev", decision.net_ev.to_string())
        .with_data("detected_at", now.to_rfc3339())
    }

    pub(super) fn client_order_id(&self, decision: &TradeDecision, now: DateTime<Utc>) -> String {
        format!(
            "intent:event_edge:{}:{}:{}",
            sanitize_component(&decision.event_id),
            sanitize_component(&decision.token_id),
            now.timestamp_millis()
        )
    }

    pub(super) fn build_actions_for_scan(
        &mut self,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        if !self.enabled || !self.core.cfg.trade || self.core.daily_cap_reached() {
            return Vec::new();
        }

        let Some(decision) = self.core.pick_best_trade(scan) else {
            return Vec::new();
        };

        if self.has_live_interest_in_token(&decision.token_id) {
            return Vec::new();
        }

        let notional = decision.limit_price * Decimal::from(decision.shares);
        if !self.allowed_notional_with_reservations(notional) {
            return Vec::new();
        }

        let client_order_id = self.client_order_id(&decision, now);

        self.reserve_pending_order(&decision, client_order_id.clone());
        self.last_signal_event_id = Some(decision.event_id.clone());

        vec![
            StrategyAction::LogEvent {
                event: self.build_signal_event(&decision, scan, now),
            },
            StrategyAction::SubmitIntent {
                intent: StrategyOrderIntent {
                    client_order_id,
                    domain: Domain::Politics,
                    market_slug: decision.market_slug.clone(),
                    token_id: decision.token_id.clone(),
                    side: decision.side,
                    is_buy: true,
                    shares: decision.shares,
                    limit_price: decision.limit_price,
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    priority: EVENT_EDGE_PRIORITY,
                    metadata: HashMap::from([
                        ("event_id".to_string(), decision.event_id.clone()),
                        ("strategy".to_string(), EVENT_EDGE_STRATEGY_NAME.to_string()),
                    ]),
                },
            },
        ]
    }

    pub(super) fn update_position_from_fill(
        &mut self,
        pending: &PendingEventEdgeOrder,
        shares: u64,
        fill_price: Decimal,
        now: DateTime<Utc>,
    ) {
        let strategy_id = self.id.clone();
        let position = self
            .positions
            .entry(pending.token_id.clone())
            .or_insert_with(|| {
                PositionInfo::new(
                    pending.token_id.clone(),
                    pending.side,
                    shares,
                    fill_price,
                    strategy_id.clone(),
                )
            });
        position.side = pending.side;
        position.shares = shares;
        position.entry_price = fill_price;
        position.opened_at = now;
        position.current_price = Some(fill_price);
        position.unrealized_pnl = Decimal::ZERO;
        position
            .metadata
            .insert("event_id".to_string(), pending.event_id.clone());
        position
            .metadata
            .insert("outcome".to_string(), pending.outcome.clone());
        position
            .metadata
            .insert("market_slug".to_string(), pending.market_slug.clone());
        position.metadata.insert(
            "client_order_id".to_string(),
            pending.client_order_id.clone(),
        );
        if let Some(condition_id) = pending.condition_id.as_ref() {
            position
                .metadata
                .insert("condition_id".to_string(), condition_id.clone());
        }
    }

    pub(super) fn state_metrics(&self) -> HashMap<String, String> {
        let mut metrics = HashMap::new();
        metrics.insert(
            "tracked_events".to_string(),
            self.discovered_events.len().to_string(),
        );
        metrics.insert(
            "resolved_events".to_string(),
            self.core.state.resolved_event_ids.len().to_string(),
        );
        metrics.insert(
            "reserved_notional_usd".to_string(),
            self.reserved_notional_usd.to_string(),
        );
        metrics.insert(
            "daily_spend_usd".to_string(),
            self.core.state.daily_spend_usd.to_string(),
        );
        metrics.insert("dry_run".to_string(), self.dry_run.to_string());
        metrics.insert("trade".to_string(), self.core.cfg.trade.to_string());
        metrics.insert(
            "interval_secs".to_string(),
            self.core.cfg.interval_secs.to_string(),
        );
        if let Some(last_scan_at) = self.last_scan_at {
            metrics.insert("last_scan_at".to_string(), last_scan_at.to_rfc3339());
        }
        if let Some(event_id) = self.last_signal_event_id.as_ref() {
            metrics.insert("last_signal_event_id".to_string(), event_id.clone());
        }
        if let Some(last_error) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), last_error.clone());
        }
        metrics
    }

    pub(super) fn apply_order_update_flow(&mut self, update: &super::OrderUpdate) {
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return;
        };

        match update.status {
            OrderStatus::Filled => {
                if let Some(pending) = self.release_pending_order(client_order_id) {
                    let filled_shares = if update.filled_qty > 0 {
                        update.filled_qty.min(pending.shares)
                    } else {
                        pending.shares
                    };
                    let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);
                    self.core.record_trade_at(
                        &pending.token_id,
                        fill_price * Decimal::from(filled_shares),
                        update.timestamp,
                    );
                    self.update_position_from_fill(
                        &pending,
                        filled_shares,
                        fill_price,
                        update.timestamp,
                    );
                }
            }
            OrderStatus::PartiallyFilled => {
                if let Some(pending) = self.pending_orders.get(client_order_id).cloned() {
                    let filled_shares = if update.filled_qty > 0 {
                        update.filled_qty.min(pending.shares)
                    } else {
                        pending.shares
                    };
                    let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);
                    self.update_position_from_fill(
                        &pending,
                        filled_shares,
                        fill_price,
                        update.timestamp,
                    );
                }
            }
            OrderStatus::Rejected
            | OrderStatus::Cancelled
            | OrderStatus::Expired
            | OrderStatus::Failed => {
                self.release_pending_order(client_order_id);
                self.last_error = update.error.clone();
            }
            OrderStatus::Pending | OrderStatus::Submitted => {}
        }
    }

    #[cfg(test)]
    pub(super) fn build_actions_for_scan_for_test(
        &mut self,
        scan: &EventEdgeScan,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        self.build_actions_for_scan(scan, now)
    }
}
