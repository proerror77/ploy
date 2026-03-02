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
