use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::domain::RiskState;
use crate::platform::{PlatformRiskState, RiskGate};
use crate::strategy::risk::RiskManager;

/// Minimal risk observability interface shared by health/metrics surfaces.
#[async_trait]
pub trait RiskView: Send + Sync {
    /// Current normalized risk state.
    async fn state(&self) -> RiskState;
    /// Legacy-compatible daily stats tuple: (daily_pnl, cycle_count, leg2_completions).
    async fn daily_stats(&self) -> (Decimal, u32, u32);
    /// Global consecutive-failure count.
    fn consecutive_failures(&self) -> u32;
}

#[async_trait]
impl RiskView for RiskManager {
    async fn state(&self) -> RiskState {
        RiskManager::state(self).await
    }

    async fn daily_stats(&self) -> (Decimal, u32, u32) {
        RiskManager::daily_stats(self).await
    }

    fn consecutive_failures(&self) -> u32 {
        RiskManager::consecutive_failures(self)
    }
}

#[async_trait]
impl RiskView for RiskGate {
    async fn state(&self) -> RiskState {
        match RiskGate::state(self).await {
            PlatformRiskState::Normal => RiskState::Normal,
            PlatformRiskState::Elevated => RiskState::Elevated,
            PlatformRiskState::Halted => RiskState::Halted,
        }
    }

    async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let (pnl, success_count, failure_count) = RiskGate::daily_stats(self).await;
        let cycle_count = success_count + failure_count;
        let leg2_completions = success_count;
        (pnl, cycle_count, leg2_completions)
    }

    fn consecutive_failures(&self) -> u32 {
        RiskGate::consecutive_failures(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskConfig;
    use rust_decimal_macros::dec;

    fn strategy_risk_config() -> RiskConfig {
        RiskConfig {
            max_single_exposure_usd: dec!(100),
            min_remaining_seconds: 30,
            max_consecutive_failures: 1,
            daily_loss_limit_usd: dec!(500),
            leg2_force_close_seconds: 20,
            max_positions: 0,
            max_positions_per_symbol: 1,
            position_size_pct: None,
            fixed_amount_usd: None,
            min_balance_usd: dec!(0),
        }
    }

    #[tokio::test]
    async fn risk_gate_impl_maps_state_and_daily_stats() {
        let mut cfg = crate::platform::RiskConfig::default();
        cfg.max_consecutive_failures = 1;
        let gate = RiskGate::new(cfg);

        gate.record_success("agent1", dec!(12)).await;
        let (pnl, cycles, leg2) = RiskView::daily_stats(&gate).await;
        assert_eq!(pnl, dec!(12));
        assert_eq!(cycles, 1);
        assert_eq!(leg2, 1);
        assert_eq!(RiskView::state(&gate).await, RiskState::Normal);

        gate.record_failure("agent1", "boom").await;
        let (_, cycles_after, leg2_after) = RiskView::daily_stats(&gate).await;
        assert_eq!(cycles_after, 2);
        assert_eq!(leg2_after, 1);
        assert_eq!(RiskView::state(&gate).await, RiskState::Halted);
        assert_eq!(RiskView::consecutive_failures(&gate), 1);
    }

    #[tokio::test]
    async fn risk_manager_impl_supports_trait_object_usage() {
        let risk_manager = RiskManager::new(strategy_risk_config());
        let view: &dyn RiskView = &risk_manager;

        risk_manager.record_success(dec!(3)).await;
        let (pnl, cycles, leg2) = view.daily_stats().await;
        assert_eq!(pnl, dec!(3));
        assert_eq!(cycles, 1);
        assert_eq!(leg2, 1);
        assert_eq!(view.state().await, RiskState::Normal);
    }
}
