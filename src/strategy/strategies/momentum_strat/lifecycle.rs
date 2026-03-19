use rust_decimal_macros::dec;
use tracing::{error, info};

use super::{
    ActivePosition, AlertLevel, DateTime, Decimal, EntrySignal, ExitReason, HashMap,
    MomentumStrategy, OrderStatus, OrderUpdate, PositionInfo, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo, Utc,
};

impl MomentumStrategy {
    pub(super) fn handle_order_update(&mut self, update: &OrderUpdate) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let pending = self
            .pending_orders
            .iter()
            .find(|(_, p)| p.client_order_id == update.client_order_id.as_deref().unwrap_or(""))
            .map(|(k, v)| (k.clone(), v.clone()));

        let Some((client_id, pending)) = pending else {
            return actions;
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(Decimal::ZERO);

                if pending.is_entry {
                    self.handle_entry_fill(
                        &mut actions,
                        &client_id,
                        &pending.symbol,
                        pending.side,
                        update.filled_qty,
                        fill_price,
                        pending.signal.as_ref(),
                    );
                } else {
                    self.handle_exit_fill(&mut actions, &pending.symbol, update.filled_qty, fill_price);
                }

                self.pending_orders.remove(&client_id);
            }
            OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                if !pending.is_entry {
                    error!(
                        "Exit order failed for {}: {:?}",
                        pending.symbol, update.status
                    );
                    actions.push(StrategyAction::Alert {
                        level: AlertLevel::Critical,
                        message: format!("Exit order failed for {}", pending.symbol),
                    });
                }
                self.pending_orders.remove(&client_id);
            }
            _ => {}
        }

        actions
    }

    pub(super) fn handle_tick(&mut self) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        let positions_to_exit: Vec<(String, Decimal)> = self
            .positions
            .iter()
            .filter_map(|(symbol, pos)| {
                if pos.time_remaining() < self.config.exit_before_resolution_secs as i64 {
                    Some((symbol.clone(), pos.highest_price))
                } else {
                    None
                }
            })
            .collect();

        for (symbol, price) in positions_to_exit {
            actions.extend(self.create_exit_order(&symbol, price, ExitReason::TimeExit));
        }

        actions
    }

    pub(super) fn build_state(&self) -> StrategyStateInfo {
        let exposure = self
            .positions
            .values()
            .map(|p| p.entry_price * Decimal::from(p.shares))
            .sum();

        let unrealized_pnl = self
            .positions
            .values()
            .map(|p| {
                let current = p.highest_price;
                (current - p.entry_price) * Decimal::from(p.shares)
            })
            .sum();

        let mut metrics = HashMap::new();
        metrics.insert(
            "active_events".to_string(),
            self.active_events.len().to_string(),
        );
        metrics.insert("symbols".to_string(), self.config.symbols.join(","));

        StrategyStateInfo {
            strategy_id: self.config.id.clone(),
            phase: if self.positions.is_empty() {
                "waiting"
            } else {
                "in_position"
            }
            .to_string(),
            enabled: self.config.enabled,
            active: self.runtime_is_active(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: exposure,
            unrealized_pnl,
            realized_pnl_today: self.realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    pub(super) fn build_positions(&self) -> Vec<PositionInfo> {
        self.positions
            .values()
            .map(|p| {
                let mut info = PositionInfo::new(
                    p.token_id.clone(),
                    p.side,
                    p.shares,
                    p.entry_price,
                    self.config.id.clone(),
                );
                info.current_price = Some(p.highest_price);
                info.unrealized_pnl = (p.highest_price - p.entry_price) * Decimal::from(p.shares);
                info.metadata.insert("symbol".to_string(), p.symbol.clone());
                info
            })
            .collect()
    }

    pub(super) fn runtime_is_active(&self) -> bool {
        !self.positions.is_empty() || !self.pending_orders.is_empty()
    }

    pub(super) fn shutdown_actions(&mut self) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        for client_id in self.pending_orders.keys() {
            actions.push(StrategyAction::CancelOrder {
                order_id: client_id.clone(),
            });
        }

        let positions_to_exit: Vec<(String, Decimal)> = self
            .positions
            .iter()
            .map(|(symbol, pos)| (symbol.clone(), pos.highest_price))
            .collect();

        for (symbol, price) in positions_to_exit {
            actions.extend(self.create_exit_order(&symbol, price, ExitReason::Manual));
        }

        actions
    }

    pub(super) fn reset_runtime(&mut self) {
        self.positions.clear();
        self.pending_orders.clear();
        self.last_trade_time.clear();
        self.last_binance_prices.clear();
        self.price_history.clear();
        self.active_events.clear();
        self.realized_pnl = Decimal::ZERO;
        self.detector.reset();
    }

    fn handle_entry_fill(
        &mut self,
        actions: &mut Vec<StrategyAction>,
        client_id: &str,
        symbol: &str,
        side: crate::domain::Side,
        filled_qty: u64,
        fill_price: Decimal,
        signal: Option<&EntrySignal>,
    ) {
        let Some(signal) = signal else {
            return;
        };

        let position = ActivePosition {
            token_id: signal.token_id.clone(),
            symbol: symbol.to_string(),
            side,
            entry_price: fill_price,
            shares: filled_qty,
            entry_time: Utc::now(),
            highest_price: fill_price,
            event_end_time: signal.event_end_time,
            client_order_id: client_id.to_string(),
        };

        self.positions.insert(symbol.to_string(), position);
        self.last_trade_time.insert(symbol.to_string(), Utc::now());

        info!(
            "Entry filled: {} {:?} {} shares @ {:.2}¢",
            symbol,
            side,
            filled_qty,
            fill_price * dec!(100)
        );

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(StrategyEventType::OrderFilled, "Entry filled")
                .with_data("symbol", symbol.to_string())
                .with_data("price", fill_price.to_string()),
        });
    }

    fn handle_exit_fill(
        &mut self,
        actions: &mut Vec<StrategyAction>,
        symbol: &str,
        filled_qty: u64,
        fill_price: Decimal,
    ) {
        if let Some(pos) = self.positions.remove(symbol) {
            let pnl = (fill_price - pos.entry_price) * Decimal::from(pos.shares);
            self.realized_pnl += pnl;

            info!(
                "Exit filled: {} {} shares @ {:.2}¢ (P&L: ${:.2})",
                symbol,
                filled_qty,
                fill_price * dec!(100),
                pnl
            );

            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(StrategyEventType::ExitTriggered, "Exit filled")
                    .with_data("symbol", symbol.to_string())
                    .with_data("pnl", pnl.to_string()),
            });
        }
    }
}
