use super::*;
use crate::domain::{OrderStatus, Side};
use crate::strategy::nba_comeback::core::NbaComebackState;
use crate::strategy::traits::{AlertLevel, StrategyEvent, StrategyEventType};
use std::collections::HashMap;

impl NbaComebackStrategy {
    pub(super) fn handle_order_update(
        &mut self,
        update: &OrderUpdate,
    ) -> Result<Vec<StrategyAction>> {
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(pending) = self.pending_orders.get_mut(client_order_id) else {
            return Ok(Vec::new());
        };

        let mut actions = Vec::new();
        if update.filled_qty > pending.accounted_filled_shares {
            let delta = update.filled_qty - pending.accounted_filled_shares;
            pending.accounted_filled_shares = update.filled_qty;
            let fill_price = update.avg_fill_price.unwrap_or(pending.limit_price);
            let strategy_id = self.id.clone();
            let token_id = pending.token_id.clone();

            self.core.record_initial_entry_submission(
                &pending.game_id,
                &pending.token_id,
                fill_price * Decimal::from(delta),
            );
            self.core.record_position_entry_with_market_and_team(
                &pending.game_id,
                &pending.trailing_abbrev,
                &pending.market_slug,
                &pending.token_id,
                fill_price,
                delta,
                0.0,
            );

            let new_position = build_position_info(&strategy_id, pending, fill_price);
            let fill_event = log_fill_event(pending, delta, fill_price);
            let position = self.positions.entry(token_id).or_insert(new_position);
            let total_cost = position.entry_price * Decimal::from(position.shares)
                + fill_price * Decimal::from(delta);
            position.shares += delta;
            if position.shares > 0 {
                position.entry_price = total_cost / Decimal::from(position.shares);
            }
            position.current_price = Some(fill_price);

            actions.push(fill_event);
        }

        if matches!(
            update.status,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed
        ) {
            self.release_pending_order(client_order_id);
        }

        Ok(actions)
    }

    pub(super) fn build_state_info(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert(
            "tracked_markets".to_string(),
            self.market_registrations.len().to_string(),
        );
        metrics.insert(
            "reserved_notional_usd".to_string(),
            self.reserved_notional_usd.to_string(),
        );
        metrics.insert("stats_loaded".to_string(), self.stats_loaded.to_string());
        if let Some(last_scan_at) = self.last_scan_at {
            metrics.insert("last_scan_at".to_string(), last_scan_at.to_rfc3339());
        }
        if let Some(last_error) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), last_error.clone());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled {
                "running".to_string()
            } else {
                "disabled".to_string()
            },
            enabled: self.enabled,
            active: self.runtime_active(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self
                .positions
                .values()
                .map(|position| position.entry_price * Decimal::from(position.shares))
                .sum(),
            unrealized_pnl: self
                .positions
                .values()
                .map(|position| position.unrealized_pnl)
                .sum(),
            realized_pnl_today: self.core.state.daily_realized_pnl_usd,
            last_update: self.last_scan_at.unwrap_or_else(Utc::now),
            metrics,
        }
    }

    pub(super) fn tracked_positions(&self) -> Vec<PositionInfo> {
        self.positions.values().cloned().collect()
    }

    pub(super) fn runtime_active(&self) -> bool {
        self.enabled && (!self.positions.is_empty() || !self.pending_orders.is_empty())
    }

    pub(super) fn shutdown_actions(&mut self) -> Vec<StrategyAction> {
        self.enabled = false;
        vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (dry_run={})", self.id, self.dry_run),
        }]
    }

    pub(super) fn reset_runtime_state(&mut self) {
        self.pending_orders.clear();
        self.positions.clear();
        self.reserved_notional_usd = Decimal::ZERO;
        self.last_scan_at = None;
        self.last_error = None;
        self.stats_loaded = false;
        self.core.state = NbaComebackState::default();
    }
}

fn build_position_info(
    strategy_id: &str,
    pending: &PendingNbaComebackOrder,
    fill_price: Decimal,
) -> PositionInfo {
    let mut info = PositionInfo::new(
        pending.token_id.clone(),
        Side::Up,
        0,
        fill_price,
        strategy_id.to_string(),
    );
    info.metadata
        .insert("game_id".to_string(), pending.game_id.clone());
    info.metadata
        .insert("trailing_team".to_string(), pending.trailing_abbrev.clone());
    info.metadata
        .insert("market_slug".to_string(), pending.market_slug.clone());
    if let Some(condition_id) = pending.condition_id.clone() {
        info.metadata
            .insert("condition_id".to_string(), condition_id);
    }
    info
}

fn log_fill_event(
    pending: &PendingNbaComebackOrder,
    delta: u64,
    fill_price: Decimal,
) -> StrategyAction {
    StrategyAction::LogEvent {
        event: StrategyEvent::new(
            StrategyEventType::OrderFilled,
            format!(
                "nba_comeback fill game={} token={} shares={} price={}",
                pending.game_id, pending.token_id, delta, fill_price
            ),
        )
        .with_data("game_id", &pending.game_id)
        .with_data("token_id", &pending.token_id)
        .with_data("filled_qty", delta.to_string())
        .with_data("fill_price", fill_price.to_string()),
    }
}
