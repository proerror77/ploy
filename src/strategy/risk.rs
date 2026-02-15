use crate::config::RiskConfig;
use crate::domain::{RiskState, Round};
use crate::error::{Result, RiskError};
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Risk manager for enforcing trading limits
pub struct RiskManager {
    config: RiskConfig,
    /// Current risk state
    state: Arc<RwLock<RiskState>>,
    /// Last halt reason (when circuit breaker is triggered)
    halt_reason: Arc<RwLock<Option<String>>>,
    /// Consecutive failures counter
    consecutive_failures: AtomicU32,
    /// Daily PnL tracker
    daily_pnl: Arc<RwLock<DailyPnL>>,
}

#[derive(Debug, Clone, Default)]
struct DailyPnL {
    date: Option<NaiveDate>,
    total_pnl: Decimal,
    cycle_count: u32,
    leg2_completions: u32,
}

impl RiskManager {
    /// Create a new risk manager
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(RiskState::Normal)),
            halt_reason: Arc::new(RwLock::new(None)),
            consecutive_failures: AtomicU32::new(0),
            daily_pnl: Arc::new(RwLock::new(DailyPnL::default())),
        }
    }

    /// Get current risk state
    pub async fn state(&self) -> RiskState {
        *self.state.read().await
    }

    /// Check if trading is allowed
    pub async fn can_trade(&self) -> bool {
        self.state.read().await.can_trade()
    }

    /// Check if we can open a new cycle
    pub async fn can_open_cycle(&self) -> bool {
        self.state.read().await.can_open_new_cycle()
    }

    // ==================== Pre-Trade Checks ====================

    /// Check if we can enter Leg1
    pub async fn check_leg1_entry(&self, shares: u64, price: Decimal, round: &Round) -> Result<()> {
        // Check risk state
        if !self.can_trade().await {
            return Err(RiskError::TradingHalted {
                reason: "Trading is halted".to_string(),
            }
            .into());
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

        // Check time remaining
        let remaining = round.seconds_remaining() as u64;
        if remaining < self.config.min_remaining_seconds {
            return Err(RiskError::InsufficientTime {
                remaining_secs: remaining,
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
        let remaining = round.seconds_remaining() as u64;
        remaining <= self.config.leg2_force_close_seconds
    }

    // ==================== Post-Trade Updates ====================

    /// Record a successful cycle completion
    pub async fn record_success(&self, pnl: Decimal) {
        // Reset consecutive failures
        self.consecutive_failures.store(0, Ordering::SeqCst);

        // Update daily PnL
        let mut daily = self.daily_pnl.write().await;
        self.ensure_daily_reset(&mut daily);
        daily.total_pnl += pnl;
        daily.cycle_count += 1;
        daily.leg2_completions += 1;

        info!(
            "Cycle completed successfully. PnL: {}, Daily total: {}",
            pnl, daily.total_pnl
        );

        // Enforce daily loss limit on net PnL (only triggers on losses).
        if daily.total_pnl <= Decimal::ZERO - self.config.daily_loss_limit_usd {
            drop(daily); // Release lock before triggering
            self.trigger_circuit_breaker("Daily loss limit exceeded")
                .await;
        }

        // Check if we should reduce risk state
        if *self.state.read().await == RiskState::Elevated {
            *self.state.write().await = RiskState::Normal;
            info!("Risk state normalized after successful cycle");
        }
    }

    /// Record a cycle failure/abort
    pub async fn record_failure(&self, reason: &str) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;

        // Update daily stats
        let mut daily = self.daily_pnl.write().await;
        self.ensure_daily_reset(&mut daily);
        daily.cycle_count += 1;

        warn!(
            "Cycle failed: {}. Consecutive failures: {}/{}",
            reason, failures, self.config.max_consecutive_failures
        );

        // Check for circuit breaker
        if failures >= self.config.max_consecutive_failures {
            self.trigger_circuit_breaker("Too many consecutive failures")
                .await;
        } else if failures >= self.config.max_consecutive_failures / 2 {
            // Elevate risk state
            *self.state.write().await = RiskState::Elevated;
            warn!("Risk state elevated due to failures");
        }
    }

    /// Record a loss (for daily limit tracking)
    pub async fn record_loss(&self, loss: Decimal) {
        let mut daily = self.daily_pnl.write().await;
        self.ensure_daily_reset(&mut daily);
        daily.total_pnl -= loss.abs();

        // Check daily loss limit
        if daily.total_pnl <= Decimal::ZERO - self.config.daily_loss_limit_usd {
            drop(daily); // Release lock before triggering
            self.trigger_circuit_breaker("Daily loss limit exceeded")
                .await;
        }
    }

    /// Trigger circuit breaker
    pub async fn trigger_circuit_breaker(&self, reason: &str) {
        error!("CIRCUIT BREAKER TRIGGERED: {}", reason);
        *self.state.write().await = RiskState::Halted;
        *self.halt_reason.write().await = Some(reason.to_string());
    }

    /// Reset circuit breaker (manual intervention)
    pub async fn reset_circuit_breaker(&self) {
        info!("Circuit breaker reset");
        *self.state.write().await = RiskState::Normal;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.halt_reason.write().await = None;
    }

    /// Get the last halt reason (if any)
    pub async fn halt_reason(&self) -> Option<String> {
        self.halt_reason.read().await.clone()
    }

    // ==================== Helpers ====================

    /// Ensure daily stats are reset on date change
    fn ensure_daily_reset(&self, daily: &mut DailyPnL) {
        let today = Utc::now().date_naive();
        if daily.date != Some(today) {
            *daily = DailyPnL {
                date: Some(today),
                ..Default::default()
            };
        }
    }

    /// Get consecutive failures count
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    /// Get daily stats
    pub async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let daily = self.daily_pnl.read().await;
        (daily.total_pnl, daily.cycle_count, daily.leg2_completions)
    }

    /// Calculate Leg2 completion rate
    pub async fn leg2_completion_rate(&self) -> f64 {
        let daily = self.daily_pnl.read().await;
        if daily.cycle_count == 0 {
            return 0.0;
        }
        daily.leg2_completions as f64 / daily.cycle_count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
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
