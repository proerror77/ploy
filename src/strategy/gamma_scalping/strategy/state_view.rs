use chrono::Utc;
use rust_decimal::Decimal;

use crate::domain::Side;
use crate::strategy::traits::{PositionInfo, StrategyStateInfo};

use super::super::rebalancer::Straddle;
use super::GammaScalpingStrategy;

impl GammaScalpingStrategy {
    pub(super) fn state_info(&self) -> StrategyStateInfo {
        let total_exposure: Decimal = self.straddles.values().map(|s| s.cost_basis).sum();

        let unrealized: Decimal = self
            .straddles
            .values()
            .map(|s| {
                let up_val = self
                    .quote_cache
                    .get(&s.up_token_id)
                    .and_then(|q| q.best_bid)
                    .unwrap_or(s.up_entry_price)
                    * Decimal::from(s.up_shares);
                let down_val = self
                    .quote_cache
                    .get(&s.down_token_id)
                    .and_then(|q| q.best_bid)
                    .unwrap_or(s.down_entry_price)
                    * Decimal::from(s.down_shares);
                up_val + down_val - s.cost_basis + s.realized_pnl
            })
            .sum();

        let mut metrics = std::collections::HashMap::new();
        metrics.insert("straddles".to_string(), self.straddles.len().to_string());
        metrics.insert("trade_count".to_string(), self.trade_count.to_string());
        metrics.insert("daily_loss".to_string(), self.daily_loss.to_string());
        metrics.insert("dry_run".to_string(), self.config.dry_run.to_string());

        StrategyStateInfo {
            strategy_id: self.config.id.clone(),
            phase: if self.straddles.is_empty() {
                "scanning".to_string()
            } else {
                "active".to_string()
            },
            enabled: self.config.enabled,
            active: self.active,
            position_count: self.straddles.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure,
            unrealized_pnl: unrealized,
            realized_pnl_today: self.realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    pub(super) fn position_snapshots(&self) -> Vec<PositionInfo> {
        self.straddles
            .values()
            .flat_map(|straddle| {
                [
                    self.position_for_leg(straddle, Side::Up),
                    self.position_for_leg(straddle, Side::Down),
                ]
                .into_iter()
                .flatten()
            })
            .collect()
    }

    fn position_for_leg(&self, straddle: &Straddle, side: Side) -> Option<PositionInfo> {
        let (token_id, shares, entry_price, leg) = match side {
            Side::Up if straddle.up_shares > 0 => (
                straddle.up_token_id.clone(),
                straddle.up_shares,
                straddle.up_entry_price,
                "straddle_up",
            ),
            Side::Down if straddle.down_shares > 0 => (
                straddle.down_token_id.clone(),
                straddle.down_shares,
                straddle.down_entry_price,
                "straddle_down",
            ),
            _ => return None,
        };

        let mut position = PositionInfo::new(
            token_id.clone(),
            side,
            shares,
            entry_price,
            self.config.id.clone(),
        );
        if let Some(bid) = self
            .quote_cache
            .get(&token_id)
            .and_then(|quote| quote.best_bid)
        {
            position.update_price(bid);
        }
        position
            .metadata
            .insert("event_id".to_string(), straddle.event_id.clone());
        position.metadata.insert("leg".to_string(), leg.to_string());
        Some(position)
    }
}
