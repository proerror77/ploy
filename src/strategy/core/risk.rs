//! Risk management for all strategies
//!
//! Centralized risk controls including exposure limits, circuit breakers,
//! and per-strategy risk tracking.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Risk configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum single position exposure (USD)
    pub max_single_exposure: Decimal,
    /// Maximum total exposure across all positions (USD)
    pub max_total_exposure: Decimal,
    /// Maximum exposure per strategy (USD)
    pub max_strategy_exposure: Decimal,
    /// Maximum consecutive failures before circuit break
    pub max_consecutive_failures: u32,
    /// Daily loss limit (USD)
    pub daily_loss_limit: Decimal,
    /// Minimum seconds remaining before blocking new positions
    pub min_time_remaining_secs: u64,
    /// Seconds before deadline to force close positions
    pub force_close_secs: u64,
    /// Maximum spread in basis points
    pub max_spread_bps: u32,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_single_exposure: Decimal::from(100),
            max_total_exposure: Decimal::from(1000),
            max_strategy_exposure: Decimal::from(500),
            max_consecutive_failures: 3,
            daily_loss_limit: Decimal::from(500),
            min_time_remaining_secs: 30,
            force_close_secs: 20,
            max_spread_bps: 500, // 5%
        }
    }
}

/// Risk state levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskState {
    /// Normal operation
    Normal,
    /// Elevated risk - more cautious
    Elevated,
    /// Trading halted
    Halted,
}

impl RiskState {
    /// Check if trading is allowed
    pub fn can_trade(&self) -> bool {
        matches!(self, RiskState::Normal | RiskState::Elevated)
    }

    /// Check if new positions can be opened
    pub fn can_open_position(&self) -> bool {
        matches!(self, RiskState::Normal)
    }
}

impl Default for RiskState {
    fn default() -> Self {
        RiskState::Normal
    }
}

/// Result of a risk check
#[derive(Debug, Clone)]
pub struct RiskCheck {
    /// Whether the check passed
    pub passed: bool,
    /// Risk level after check
    pub risk_level: RiskState,
    /// Reason if check failed
    pub reason: Option<String>,
    /// Suggested action
    pub action: Option<RiskAction>,
}

impl RiskCheck {
    pub fn pass() -> Self {
        Self {
            passed: true,
            risk_level: RiskState::Normal,
            reason: None,
            action: None,
        }
    }

    pub fn fail(reason: impl Into<String>) -> Self {
        Self {
            passed: false,
            risk_level: RiskState::Normal,
            reason: Some(reason.into()),
            action: None,
        }
    }

    pub fn with_action(mut self, action: RiskAction) -> Self {
        self.action = Some(action);
        self
    }

    pub fn with_level(mut self, level: RiskState) -> Self {
        self.risk_level = level;
        self
    }
}

/// Suggested risk action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskAction {
    /// No action needed
    None,
    /// Reduce position size
    ReduceSize { max_shares: u64 },
    /// Close position immediately
    ClosePosition { position_id: String },
    /// Cancel pending orders
    CancelOrders { strategy_id: String },
    /// Halt trading
    HaltTrading { reason: String },
}

/// Per-strategy risk metrics
#[derive(Debug, Clone, Default)]
struct StrategyRiskMetrics {
    /// Current exposure
    exposure: Decimal,
    /// Unrealized P&L
    unrealized_pnl: Decimal,
    /// Realized P&L today
    realized_pnl: Decimal,
    /// Open position count
    position_count: usize,
    /// Consecutive failures
    consecutive_failures: u32,
    /// Last update time
    last_update: Option<DateTime<Utc>>,
}

/// Daily P&L tracking
#[derive(Debug, Clone, Default)]
struct DailyPnL {
    date: Option<NaiveDate>,
    total_pnl: Decimal,
    cycle_count: u32,
    success_count: u32,
}

/// Centralized risk manager
pub struct RiskManager {
    config: RiskConfig,
    /// Global risk state
    state: Arc<RwLock<RiskState>>,
    /// Per-strategy metrics
    strategy_metrics: Arc<RwLock<HashMap<String, StrategyRiskMetrics>>>,
    /// Global consecutive failures
    consecutive_failures: AtomicU32,
    /// Daily P&L tracker
    daily_pnl: Arc<RwLock<DailyPnL>>,
    /// Total exposure across all strategies
    total_exposure: Arc<RwLock<Decimal>>,
}

impl Default for RiskManager {
    fn default() -> Self {
        Self::new(RiskConfig::default())
    }
}

impl RiskManager {
    /// Create a new risk manager
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(RiskState::Normal)),
            strategy_metrics: Arc::new(RwLock::new(HashMap::new())),
            consecutive_failures: AtomicU32::new(0),
            daily_pnl: Arc::new(RwLock::new(DailyPnL::default())),
            total_exposure: Arc::new(RwLock::new(Decimal::ZERO)),
        }
    }

    // ==================== State Queries ====================

    /// Get current global risk state
    pub async fn state(&self) -> RiskState {
        *self.state.read().await
    }

    /// Check if trading is allowed
    pub async fn can_trade(&self) -> bool {
        self.state.read().await.can_trade()
    }

    /// Check if new positions can be opened
    pub async fn can_open_position(&self) -> bool {
        self.state.read().await.can_open_position()
    }

    /// Get current total exposure
    pub async fn total_exposure(&self) -> Decimal {
        *self.total_exposure.read().await
    }

    // ==================== Pre-Trade Checks ====================

    /// Check if a new position can be opened
    pub async fn check_new_position(
        &self,
        strategy_id: &str,
        shares: u64,
        price: Decimal,
        time_remaining_secs: Option<u64>,
    ) -> RiskCheck {
        // Check global state
        if !self.can_trade().await {
            return RiskCheck::fail("Trading is halted")
                .with_level(RiskState::Halted);
        }

        if !self.can_open_position().await {
            return RiskCheck::fail("New positions blocked - elevated risk")
                .with_level(RiskState::Elevated);
        }

        // Check single exposure
        let exposure = Decimal::from(shares) * price;
        if exposure > self.config.max_single_exposure {
            let max_shares = (self.config.max_single_exposure / price)
                .to_u64()
                .unwrap_or(0);
            return RiskCheck::fail(format!(
                "Exposure ${} exceeds limit ${}",
                exposure, self.config.max_single_exposure
            ))
            .with_action(RiskAction::ReduceSize { max_shares });
        }

        // Check total exposure
        let current_total = *self.total_exposure.read().await;
        if current_total + exposure > self.config.max_total_exposure {
            return RiskCheck::fail(format!(
                "Total exposure would be ${}, exceeds limit ${}",
                current_total + exposure,
                self.config.max_total_exposure
            ));
        }

        // Check strategy exposure
        let strategy_metrics = self.strategy_metrics.read().await;
        if let Some(metrics) = strategy_metrics.get(strategy_id) {
            if metrics.exposure + exposure > self.config.max_strategy_exposure {
                return RiskCheck::fail(format!(
                    "Strategy exposure would be ${}, exceeds limit ${}",
                    metrics.exposure + exposure,
                    self.config.max_strategy_exposure
                ));
            }
        }

        // Check time remaining
        if let Some(remaining) = time_remaining_secs {
            if remaining < self.config.min_time_remaining_secs {
                return RiskCheck::fail(format!(
                    "Only {}s remaining, minimum is {}s",
                    remaining, self.config.min_time_remaining_secs
                ));
            }
        }

        RiskCheck::pass()
    }

    /// Check spread for anti-fake signal
    pub fn check_spread(&self, spread_bps: u32) -> RiskCheck {
        if spread_bps > self.config.max_spread_bps {
            return RiskCheck::fail(format!(
                "Spread {} bps exceeds max {} bps",
                spread_bps, self.config.max_spread_bps
            ));
        }
        RiskCheck::pass()
    }

    /// Check if position must be force closed
    pub fn must_force_close(&self, time_remaining_secs: u64) -> bool {
        time_remaining_secs <= self.config.force_close_secs
    }

    // ==================== State Updates ====================

    /// Update strategy exposure
    pub async fn update_exposure(
        &self,
        strategy_id: &str,
        exposure: Decimal,
        unrealized_pnl: Decimal,
        position_count: usize,
    ) {
        let mut metrics_map = self.strategy_metrics.write().await;
        let metrics = metrics_map.entry(strategy_id.to_string()).or_default();

        // Update total exposure
        let old_exposure = metrics.exposure;
        *self.total_exposure.write().await += exposure - old_exposure;

        metrics.exposure = exposure;
        metrics.unrealized_pnl = unrealized_pnl;
        metrics.position_count = position_count;
        metrics.last_update = Some(Utc::now());
    }

    /// Record a successful trade/cycle
    pub async fn record_success(&self, strategy_id: &str, pnl: Decimal) {
        // Reset consecutive failures
        self.consecutive_failures.store(0, Ordering::SeqCst);

        // Update strategy metrics
        {
            let mut metrics_map = self.strategy_metrics.write().await;
            let metrics = metrics_map.entry(strategy_id.to_string()).or_default();
            metrics.consecutive_failures = 0;
            metrics.realized_pnl += pnl;
        }

        // Update daily P&L
        {
            let mut daily = self.daily_pnl.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl += pnl;
            daily.cycle_count += 1;
            daily.success_count += 1;
        }

        info!(
            "Strategy {} recorded success. PnL: {}",
            strategy_id, pnl
        );

        // Check if we should normalize risk state
        if *self.state.read().await == RiskState::Elevated {
            *self.state.write().await = RiskState::Normal;
            info!("Risk state normalized after successful trade");
        }
    }

    /// Record a failed trade/cycle
    pub async fn record_failure(&self, strategy_id: &str, reason: &str) {
        let global_failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;

        // Update strategy metrics
        let strategy_failures = {
            let mut metrics_map = self.strategy_metrics.write().await;
            let metrics = metrics_map.entry(strategy_id.to_string()).or_default();
            metrics.consecutive_failures += 1;
            metrics.consecutive_failures
        };

        // Update daily stats
        {
            let mut daily = self.daily_pnl.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.cycle_count += 1;
        }

        warn!(
            "Strategy {} failed: {}. Failures: strategy={}, global={}",
            strategy_id, reason, strategy_failures, global_failures
        );

        // Check for circuit breaker
        if global_failures >= self.config.max_consecutive_failures {
            self.trigger_circuit_breaker("Too many consecutive failures").await;
        } else if global_failures >= self.config.max_consecutive_failures / 2 {
            *self.state.write().await = RiskState::Elevated;
            warn!("Risk state elevated due to failures");
        }
    }

    /// Record a loss
    pub async fn record_loss(&self, strategy_id: &str, loss: Decimal) {
        // Update strategy metrics
        {
            let mut metrics_map = self.strategy_metrics.write().await;
            let metrics = metrics_map.entry(strategy_id.to_string()).or_default();
            metrics.realized_pnl -= loss.abs();
        }

        // Update daily P&L
        let should_halt = {
            let mut daily = self.daily_pnl.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl -= loss.abs();
            daily.total_pnl.abs() >= self.config.daily_loss_limit
        };

        if should_halt {
            self.trigger_circuit_breaker("Daily loss limit exceeded").await;
        }
    }

    /// Trigger circuit breaker
    pub async fn trigger_circuit_breaker(&self, reason: &str) {
        error!("CIRCUIT BREAKER TRIGGERED: {}", reason);
        *self.state.write().await = RiskState::Halted;
    }

    /// Reset circuit breaker (manual intervention)
    pub async fn reset_circuit_breaker(&self) {
        info!("Circuit breaker reset");
        *self.state.write().await = RiskState::Normal;
        self.consecutive_failures.store(0, Ordering::SeqCst);

        // Reset all strategy failure counts
        let mut metrics_map = self.strategy_metrics.write().await;
        for metrics in metrics_map.values_mut() {
            metrics.consecutive_failures = 0;
        }
    }

    // ==================== Reporting ====================

    /// Get consecutive failures count
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    /// Get daily stats
    pub async fn daily_stats(&self) -> (Decimal, u32, u32) {
        let daily = self.daily_pnl.read().await;
        (daily.total_pnl, daily.cycle_count, daily.success_count)
    }

    /// Get strategy metrics
    pub async fn strategy_stats(&self, strategy_id: &str) -> Option<(Decimal, Decimal, usize, u32)> {
        let metrics_map = self.strategy_metrics.read().await;
        metrics_map.get(strategy_id).map(|m| {
            (m.exposure, m.realized_pnl, m.position_count, m.consecutive_failures)
        })
    }

    /// Calculate success rate
    pub async fn success_rate(&self) -> f64 {
        let daily = self.daily_pnl.read().await;
        if daily.cycle_count == 0 {
            return 0.0;
        }
        daily.success_count as f64 / daily.cycle_count as f64
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

    /// Clear all metrics (for testing/reset)
    pub async fn clear(&self) {
        *self.state.write().await = RiskState::Normal;
        self.strategy_metrics.write().await.clear();
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.daily_pnl.write().await = DailyPnL::default();
        *self.total_exposure.write().await = Decimal::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> RiskConfig {
        RiskConfig {
            max_single_exposure: dec!(100),
            max_total_exposure: dec!(500),
            max_strategy_exposure: dec!(200),
            max_consecutive_failures: 3,
            daily_loss_limit: dec!(500),
            min_time_remaining_secs: 30,
            force_close_secs: 20,
            max_spread_bps: 500,
        }
    }

    #[tokio::test]
    async fn test_exposure_limits() {
        let risk = RiskManager::new(test_config());

        // Within limit
        let check = risk.check_new_position("test", 100, dec!(0.50), None).await;
        assert!(check.passed);

        // Over single exposure limit (200 * 0.60 = 120 > 100)
        let check = risk.check_new_position("test", 200, dec!(0.60), None).await;
        assert!(!check.passed);
        assert!(check.reason.unwrap().contains("exceeds limit"));
    }

    #[tokio::test]
    async fn test_time_remaining() {
        let risk = RiskManager::new(test_config());

        // Enough time
        let check = risk.check_new_position("test", 50, dec!(0.50), Some(60)).await;
        assert!(check.passed);

        // Not enough time
        let check = risk.check_new_position("test", 50, dec!(0.50), Some(20)).await;
        assert!(!check.passed);
        assert!(check.reason.unwrap().contains("remaining"));
    }

    #[tokio::test]
    async fn test_circuit_breaker() {
        let risk = RiskManager::new(test_config());

        // Record failures
        for i in 0..3 {
            risk.record_failure("test", &format!("Test failure {}", i)).await;
        }

        // Should be halted
        assert_eq!(risk.state().await, RiskState::Halted);
        assert!(!risk.can_trade().await);

        // Reset
        risk.reset_circuit_breaker().await;
        assert_eq!(risk.state().await, RiskState::Normal);
        assert!(risk.can_trade().await);
    }

    #[tokio::test]
    async fn test_spread_check() {
        let risk = RiskManager::new(test_config());

        // Within limit
        let check = risk.check_spread(300);
        assert!(check.passed);

        // Over limit
        let check = risk.check_spread(600);
        assert!(!check.passed);
    }

    #[tokio::test]
    async fn test_force_close() {
        let risk = RiskManager::new(test_config());

        // Don't force with lots of time
        assert!(!risk.must_force_close(60));

        // Force when time is running out
        assert!(risk.must_force_close(15));
    }

    #[tokio::test]
    async fn test_success_tracking() {
        let risk = RiskManager::new(test_config());

        // Record some successes
        risk.record_success("test", dec!(10)).await;
        risk.record_success("test", dec!(5)).await;

        let (pnl, cycles, successes) = risk.daily_stats().await;
        assert_eq!(pnl, dec!(15));
        assert_eq!(cycles, 2);
        assert_eq!(successes, 2);
    }
}
