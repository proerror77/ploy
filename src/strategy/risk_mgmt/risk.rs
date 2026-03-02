use crate::config::RiskConfig;
use crate::domain::{RiskState, Round};
use crate::error::{Result, RiskError};
use crate::platform::{
    AgentRiskParams, Domain, PlatformRiskState, RiskConfig as GateRiskConfig, RiskGate,
};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

const ENGINE_AGENT_ID: &str = "strategy-engine";

/// Strategy-facing risk manager backed by the shared platform `RiskGate` runtime.
///
/// This adapter preserves the legacy `RiskManager` API used by `StrategyEngine`
/// while delegating circuit-breaker and daily PnL runtime semantics to `RiskGate`.
pub struct RiskManager {
    config: RiskConfig,
    gate: RiskGate,
    /// Last halt reason (for legacy observability/health paths)
    halt_reason: Arc<RwLock<Option<String>>>,
}

impl RiskManager {
    /// Create a new risk manager
    pub fn new(config: RiskConfig) -> Self {
        let gate = RiskGate::new(Self::to_gate_config(&config));
        Self {
            config,
            gate,
            halt_reason: Arc::new(RwLock::new(None)),
        }
    }

    fn to_gate_config(config: &RiskConfig) -> GateRiskConfig {
        let exposure_multiplier = if config.max_positions > 0 {
            Decimal::from(config.max_positions)
        } else {
            Decimal::from(10u32)
        };

        GateRiskConfig {
            max_platform_exposure: config.max_single_exposure_usd * exposure_multiplier,
            max_consecutive_failures: config.max_consecutive_failures,
            daily_loss_limit: config.daily_loss_limit_usd,
            max_drawdown_limit: None,
            max_spread_bps: 500,
            critical_bypass_exposure: false,
            // Keep behavior compatible with legacy RiskManager: no automatic unhalt.
            circuit_breaker_auto_recover: false,
            circuit_breaker_cooldown_secs: 300,
            crypto_max_exposure: None,
            sports_max_exposure: None,
            politics_max_exposure: None,
            economics_max_exposure: None,
            crypto_daily_loss_limit: None,
            sports_daily_loss_limit: None,
            politics_daily_loss_limit: None,
            economics_daily_loss_limit: None,
        }
    }

    fn engine_agent_params(&self) -> AgentRiskParams {
        let max_total_exposure = if self.config.max_positions > 0 {
            self.config.max_single_exposure_usd * Decimal::from(self.config.max_positions)
        } else {
            self.config.max_single_exposure_usd * Decimal::from(10u32)
        };

        AgentRiskParams {
            max_order_value: self.config.max_single_exposure_usd,
            max_total_exposure,
            max_unhedged_positions: self.config.max_positions_per_symbol.max(1),
            max_daily_loss: self.config.daily_loss_limit_usd,
            allow_overnight: false,
            allowed_markets: vec![],
        }
    }

    async fn ensure_engine_agent_registered(&self) {
        self.gate
            .register_agent_with_domain(
                ENGINE_AGENT_ID,
                Domain::Custom(0),
                self.engine_agent_params(),
            )
            .await;
    }

    fn map_platform_state(state: PlatformRiskState) -> RiskState {
        match state {
            PlatformRiskState::Normal => RiskState::Normal,
            PlatformRiskState::Elevated => RiskState::Elevated,
            PlatformRiskState::Halted => RiskState::Halted,
        }
    }

    async fn sync_halt_reason_from_gate(&self, fallback: &str) {
        if self.state().await != RiskState::Halted {
            return;
        }

        let reason = self
            .gate
            .circuit_breaker_events()
            .await
            .into_iter()
            .rev()
            .find(|event| event.state == PlatformRiskState::Halted)
            .map(|event| event.reason)
            .unwrap_or_else(|| fallback.to_string());

        *self.halt_reason.write().await = Some(reason);
    }

    /// Get current risk state
    pub async fn state(&self) -> RiskState {
        Self::map_platform_state(self.gate.state().await)
    }

    /// Check if trading is allowed
    pub async fn can_trade(&self) -> bool {
        self.gate.can_trade().await
    }

    /// Check if we can open a new cycle
    pub async fn can_open_cycle(&self) -> bool {
        self.state().await.can_open_new_cycle()
    }

    // ==================== Pre-Trade Checks ====================

    /// Check if we can enter Leg1
    pub async fn check_leg1_entry(&self, shares: u64, price: Decimal, round: &Round) -> Result<()> {
        // Check risk state
        if !self.can_trade().await {
            let reason = self
                .halt_reason()
                .await
                .unwrap_or_else(|| "Trading is halted".to_string());
            return Err(RiskError::TradingHalted { reason }.into());
        }

        // Check exposure limit
        let exposure = Decimal::from(shares) * price;
        if exposure > self.config.max_single_exposure_usd {
            return Err(RiskError::MaxExposureExceeded {
                limit: self.config.max_single_exposure_usd,
                requested: exposure,
            }
            .into());
        }

        // Check time remaining (keep signed semantics; never cast negative to huge u64).
        let remaining_secs_i64 = round.seconds_remaining();
        if remaining_secs_i64 < self.config.min_remaining_seconds as i64 {
            return Err(RiskError::InsufficientTime {
                remaining_secs: remaining_secs_i64.max(0) as u64,
                min_secs: self.config.min_remaining_seconds,
            }
            .into());
        }

        Ok(())
    }

    /// Check spread for anti-fake-dump
    pub fn check_spread(&self, spread_bps: u32, max_spread_bps: u32) -> Result<()> {
        if spread_bps > max_spread_bps {
            return Err(RiskError::SpreadTooWide {
                spread_bps,
                max_bps: max_spread_bps,
            }
            .into());
        }
        Ok(())
    }

    /// Check if Leg2 must be forced (approaching round end)
    pub fn must_force_leg2(&self, round: &Round) -> bool {
        let remaining_secs_i64 = round.seconds_remaining();
        remaining_secs_i64 <= self.config.leg2_force_close_seconds as i64
    }

    // ==================== Post-Trade Updates ====================

    /// Record a successful cycle completion
    pub async fn record_success(&self, pnl: Decimal) {
        self.ensure_engine_agent_registered().await;

        self.gate.record_success(ENGINE_AGENT_ID, pnl).await;

        if self.state().await == RiskState::Halted {
            self.sync_halt_reason_from_gate("Risk gate halted after success")
                .await;
            return;
        }

        // Clear stale halt reason once we are no longer halted.
        *self.halt_reason.write().await = None;

        info!("Cycle completed successfully. PnL: {}", pnl);
    }

    /// Record a cycle failure/abort
    pub async fn record_failure(&self, reason: &str) {
        self.ensure_engine_agent_registered().await;

        warn!("Cycle failed: {}", reason);
        self.gate.record_failure(ENGINE_AGENT_ID, reason).await;

        if self.state().await == RiskState::Halted {
            self.sync_halt_reason_from_gate("Too many consecutive failures")
                .await;
        }
    }

    /// Record a loss (for daily limit tracking)
    pub async fn record_loss(&self, loss: Decimal) {
        self.ensure_engine_agent_registered().await;

        self.gate.record_loss(ENGINE_AGENT_ID, loss.abs()).await;

        if self.state().await == RiskState::Halted {
            self.sync_halt_reason_from_gate("Daily loss limit exceeded")
                .await;
        }
    }

    /// Trigger circuit breaker
    pub async fn trigger_circuit_breaker(&self, reason: &str) {
        self.ensure_engine_agent_registered().await;

        error!("CIRCUIT BREAKER TRIGGERED: {}", reason);
        self.gate.trigger_circuit_breaker(reason).await;
        *self.halt_reason.write().await = Some(reason.to_string());
    }

    /// Reset circuit breaker (manual intervention)
    pub async fn reset_circuit_breaker(&self) {
        info!("Circuit breaker reset");
        self.gate.reset_circuit_breaker().await;
        *self.halt_reason.write().await = None;
    }

    /// Get the last halt reason (if any)
    pub async fn halt_reason(&self) -> Option<String> {
        self.halt_reason.read().await.clone()
    }

    // ==================== Metrics ====================

    /// Get consecutive failures count
    pub fn consecutive_failures(&self) -> u32 {
        self.gate.consecutive_failures()
    }

    /// Get daily stats
    ///
    /// Returns `(daily_pnl, cycle_count, leg2_completions)` for backward compatibility.
    pub async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let (daily_pnl, success_count, failure_count) = self.gate.daily_stats().await;
        let cycle_count = success_count + failure_count;
        let leg2_completions = success_count;
        (daily_pnl, cycle_count, leg2_completions)
    }

    /// Calculate Leg2 completion rate
    pub async fn leg2_completion_rate(&self) -> f64 {
        let (_, cycle_count, leg2_completions) = self.daily_stats().await;
        if cycle_count == 0 {
            return 0.0;
        }
        leg2_completions as f64 / cycle_count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;

    fn test_config() -> RiskConfig {
        RiskConfig {
            max_single_exposure_usd: dec!(100),
            min_remaining_seconds: 30,
            max_consecutive_failures: 3,
            daily_loss_limit_usd: dec!(500),
            leg2_force_close_seconds: 20,
            max_positions: 0,
            max_positions_per_symbol: 1,
            position_size_pct: None,
            fixed_amount_usd: None,
            min_balance_usd: dec!(0),
        }
    }

    fn test_round(remaining_secs: i64) -> Round {
        let now = Utc::now();
        Round {
            id: Some(1),
            slug: "test".to_string(),
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
            start_time: now - Duration::minutes(10),
            end_time: now + Duration::seconds(remaining_secs),
            outcome: None,
        }
    }

    #[tokio::test]
    async fn test_exposure_limit() {
        let risk = RiskManager::new(test_config());
        let round = test_round(60);

        // Within limit
        let result = risk.check_leg1_entry(100, dec!(0.50), &round).await;
        assert!(result.is_ok());

        // Over limit (200 shares * $0.60 = $120 > $100)
        let result = risk.check_leg1_entry(200, dec!(0.60), &round).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_time_remaining() {
        let risk = RiskManager::new(test_config());

        // Enough time
        let round = test_round(60);
        let result = risk.check_leg1_entry(50, dec!(0.50), &round).await;
        assert!(result.is_ok());

        // Not enough time
        let round = test_round(20);
        let result = risk.check_leg1_entry(50, dec!(0.50), &round).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_time_remaining_already_expired_round() {
        let risk = RiskManager::new(test_config());

        // Expired round should not underflow to huge positive seconds.
        let round = test_round(-5);
        let result = risk.check_leg1_entry(50, dec!(0.50), &round).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_circuit_breaker() {
        let risk = RiskManager::new(test_config());

        // Record failures
        for i in 0..3 {
            risk.record_failure(&format!("Test failure {}", i)).await;
        }

        // Should be halted
        assert_eq!(risk.state().await, RiskState::Halted);
        assert!(!risk.can_trade().await);
        assert_eq!(
            risk.halt_reason().await.as_deref(),
            Some("Too many consecutive failures")
        );

        // Reset
        risk.reset_circuit_breaker().await;
        assert_eq!(risk.state().await, RiskState::Normal);
        assert!(risk.can_trade().await);
        assert_eq!(risk.halt_reason().await, None);
    }

    #[tokio::test]
    async fn test_force_leg2() {
        let risk = RiskManager::new(test_config());

        // Don't force with lots of time
        let round = test_round(60);
        assert!(!risk.must_force_leg2(&round));

        // Force when time is running out
        let round = test_round(15);
        assert!(risk.must_force_leg2(&round));
    }
}
