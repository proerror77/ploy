use std::sync::atomic::Ordering;

use rust_decimal::Decimal;

use super::{DrawdownSnapshot, PlatformRiskState, RiskGate};

impl RiskGate {
    /// 當前平台狀態
    pub async fn state(&self) -> PlatformRiskState {
        *self.state.read().await
    }

    /// 是否可以交易
    pub async fn can_trade(&self) -> bool {
        self.state.read().await.can_trade()
    }

    /// 當前平台總暴露
    pub async fn total_exposure(&self) -> Decimal {
        *self.total_exposure.read().await
    }

    /// Agent 統計
    pub async fn agent_stats(&self, agent_id: &str) -> Option<(Decimal, Decimal, usize, u32)> {
        let stats_map = self.agent_stats.read().await;
        stats_map.get(agent_id).map(|stats| {
            (
                stats.exposure,
                stats.realized_pnl,
                stats.position_count,
                stats.consecutive_failures,
            )
        })
    }

    /// 每日統計
    pub async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let daily = self.daily_stats.read().await;
        (daily.total_pnl, daily.success_count, daily.failure_count)
    }

    /// Daily loss limit (USD)
    pub fn daily_loss_limit(&self) -> Decimal {
        self.config.daily_loss_limit
    }

    /// Optional max drawdown limit (USD)
    pub fn max_drawdown_limit(&self) -> Option<Decimal> {
        self.config.max_drawdown_limit
    }

    /// Current drawdown + max observed drawdown (USD)
    pub async fn drawdown_stats(&self) -> (Decimal, Decimal) {
        let drawdown = self.drawdown_stats.read().await;
        (drawdown.current_drawdown, drawdown.max_drawdown_observed)
    }

    /// Full drawdown snapshot for persistence/recovery.
    pub async fn drawdown_snapshot(&self) -> DrawdownSnapshot {
        let drawdown = self.drawdown_stats.read().await;
        DrawdownSnapshot {
            current_equity: drawdown.current_equity,
            equity_peak: drawdown.equity_peak,
            current_drawdown: drawdown.current_drawdown,
            max_drawdown_observed: drawdown.max_drawdown_observed,
        }
    }

    /// Circuit breaker event history
    pub async fn circuit_breaker_events(&self) -> Vec<super::CircuitBreakerEvent> {
        self.circuit_events.read().await.clone()
    }

    /// 連續失敗數
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }
}
