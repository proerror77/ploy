//! Trading Circuit Breaker
//!
//! Implements circuit breaker pattern for trading operations to prevent
//! cascading failures and protect against adverse market conditions.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - all trades allowed
    Closed,
    /// Failure threshold exceeded - trades blocked
    Open,
    /// Recovery period - limited trades allowed for testing
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half-open"),
        }
    }
}

/// Configuration for trading circuit breaker
#[derive(Debug, Clone)]
pub struct TradingCircuitBreakerConfig {
    /// Number of consecutive failures to trip the circuit
    pub failure_threshold: u32,
    /// Daily loss limit in USD to trip the circuit
    pub daily_loss_limit_usd: Decimal,
    /// Time to wait before transitioning from Open to HalfOpen (seconds)
    pub recovery_timeout_secs: u64,
    /// Maximum quote staleness before tripping (seconds)
    pub quote_staleness_secs: u64,
    /// WebSocket disconnection time to trip circuit (seconds)
    pub ws_disconnect_threshold_secs: u64,
    /// Number of successful trades in HalfOpen to close circuit
    pub half_open_success_threshold: u32,
    /// Maximum number of trade attempts allowed while in HalfOpen (0 = unlimited)
    pub half_open_max_trades: u32,
    /// Maximum cumulative exposure allowed while in HalfOpen (0 = unlimited)
    pub half_open_max_exposure_usd: Decimal,
}

impl Default for TradingCircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            daily_loss_limit_usd: Decimal::from(100),
            recovery_timeout_secs: 300, // 5 minutes
            quote_staleness_secs: 30,
            ws_disconnect_threshold_secs: 120, // 2 minutes
            half_open_success_threshold: 1,
            half_open_max_trades: 1,
            half_open_max_exposure_usd: Decimal::from(25),
        }
    }
}

/// Trip reasons for the circuit breaker
#[derive(Debug, Clone)]
pub enum TripReason {
    ConsecutiveFailures(u32),
    DailyLossLimit(Decimal),
    QuoteStaleness(u64),
    WebSocketDisconnect(u64),
    ManualTrip(String),
}

impl std::fmt::Display for TripReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TripReason::ConsecutiveFailures(n) => write!(f, "{} consecutive failures", n),
            TripReason::DailyLossLimit(loss) => write!(f, "daily loss limit ${}", loss),
            TripReason::QuoteStaleness(secs) => write!(f, "quote staleness {}s", secs),
            TripReason::WebSocketDisconnect(secs) => write!(f, "WebSocket disconnected {}s", secs),
            TripReason::ManualTrip(reason) => write!(f, "manual: {}", reason),
        }
    }
}

/// Circuit breaker for trading operations
pub struct TradingCircuitBreaker {
    config: TradingCircuitBreakerConfig,
    state: Arc<RwLock<CircuitState>>,
    consecutive_failures: AtomicU32,
    daily_pnl: Arc<RwLock<Decimal>>,
    last_success: Arc<RwLock<Option<DateTime<Utc>>>>,
    last_failure: Arc<RwLock<Option<DateTime<Utc>>>>,
    last_trip_reason: Arc<RwLock<Option<TripReason>>>,
    opened_at: Arc<RwLock<Option<DateTime<Utc>>>>,
    half_open_successes: AtomicU32,
    half_open_trade_count: AtomicU32,
    half_open_exposure_usd: Arc<RwLock<Decimal>>,
    total_trips: AtomicU64,
}

impl TradingCircuitBreaker {
    /// Create a new trading circuit breaker
    pub fn new(config: TradingCircuitBreakerConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(CircuitState::Closed)),
            consecutive_failures: AtomicU32::new(0),
            daily_pnl: Arc::new(RwLock::new(Decimal::ZERO)),
            last_success: Arc::new(RwLock::new(None)),
            last_failure: Arc::new(RwLock::new(None)),
            last_trip_reason: Arc::new(RwLock::new(None)),
            opened_at: Arc::new(RwLock::new(None)),
            half_open_successes: AtomicU32::new(0),
            half_open_trade_count: AtomicU32::new(0),
            half_open_exposure_usd: Arc::new(RwLock::new(Decimal::ZERO)),
            total_trips: AtomicU64::new(0),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(TradingCircuitBreakerConfig::default())
    }

    /// Get current state
    pub async fn state(&self) -> CircuitState {
        *self.state.read().await
    }

    /// Check if trading is allowed
    pub async fn should_allow(&self) -> bool {
        let state = self.state().await;

        // Check for auto-recovery from Open to HalfOpen
        if state == CircuitState::Open {
            if let Some(opened_at) = *self.opened_at.read().await {
                let elapsed = Utc::now().signed_duration_since(opened_at).num_seconds() as u64;
                if elapsed >= self.config.recovery_timeout_secs {
                    self.transition_to_half_open().await;
                    return true;
                }
            }
            return false;
        }

        state != CircuitState::Open
    }

    /// Check if a trade should be allowed, enforcing HalfOpen limits.
    ///
    /// Note: this method is intended to be called once per trade attempt and will
    /// increment HalfOpen counters when it returns `Ok(true)`.
    pub async fn should_allow_trade(&self, proposed_exposure_usd: Decimal) -> Result<bool, String> {
        let state = self.state().await;

        match state {
            CircuitState::Closed => Ok(true),
            CircuitState::Open => {
                if self.should_transition_to_half_open().await {
                    self.transition_to_half_open().await;
                    self.allow_half_open_trade(proposed_exposure_usd).await
                } else {
                    Err(format!(
                        "Circuit open, {} seconds until recovery",
                        self.time_until_recovery().await
                    ))
                }
            }
            CircuitState::HalfOpen => self.allow_half_open_trade(proposed_exposure_usd).await,
        }
    }

    async fn allow_half_open_trade(&self, proposed_exposure_usd: Decimal) -> Result<bool, String> {
        if self.config.half_open_max_trades > 0 {
            let trades = self.half_open_trade_count.load(Ordering::SeqCst);
            if trades >= self.config.half_open_max_trades {
                return Err("HalfOpen trade limit reached".to_string());
            }
        }

        if self.config.half_open_max_exposure_usd > Decimal::ZERO
            && proposed_exposure_usd > Decimal::ZERO
        {
            let current = *self.half_open_exposure_usd.read().await;
            if current + proposed_exposure_usd > self.config.half_open_max_exposure_usd {
                return Err("HalfOpen exposure limit reached".to_string());
            }
        }

        self.half_open_trade_count.fetch_add(1, Ordering::SeqCst);
        if proposed_exposure_usd > Decimal::ZERO {
            let mut exposure = self.half_open_exposure_usd.write().await;
            *exposure += proposed_exposure_usd;
        }

        Ok(true)
    }

    /// Record a successful trade
    pub async fn record_success(&self, pnl: Decimal) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.last_success.write().await = Some(Utc::now());

        // Update daily PnL
        {
            let mut daily = self.daily_pnl.write().await;
            *daily += pnl;
        }

        let state = self.state().await;
        if state == CircuitState::HalfOpen {
            let successes = self.half_open_successes.fetch_add(1, Ordering::SeqCst) + 1;
            if successes >= self.config.half_open_success_threshold {
                self.close().await;
            }
        }

        debug!("Trade success recorded, PnL: {}", pnl);
    }

    /// Record a failed trade
    pub async fn record_failure(&self, reason: &str) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_failure.write().await = Some(Utc::now());

        warn!("Trade failure #{}: {}", failures, reason);

        // Check if we should trip
        if failures >= self.config.failure_threshold {
            self.trip(TripReason::ConsecutiveFailures(failures)).await;
        }

        // Reset half-open progress on failure
        if self.state().await == CircuitState::HalfOpen {
            self.trip(TripReason::ConsecutiveFailures(failures)).await;
        }
    }

    /// Record daily PnL (can be called to update from external source)
    pub async fn update_daily_pnl(&self, pnl: Decimal) {
        let mut daily = self.daily_pnl.write().await;
        *daily = pnl;

        // Check daily loss limit
        if pnl < -self.config.daily_loss_limit_usd {
            drop(daily);
            self.trip(TripReason::DailyLossLimit(pnl.abs())).await;
        }
    }

    /// Check quote staleness and trip if exceeded
    pub async fn check_quote_staleness(&self, last_quote_time: DateTime<Utc>) -> bool {
        let elapsed = Utc::now()
            .signed_duration_since(last_quote_time)
            .num_seconds() as u64;

        if elapsed > self.config.quote_staleness_secs {
            self.trip(TripReason::QuoteStaleness(elapsed)).await;
            false
        } else {
            true
        }
    }

    /// Check WebSocket connection status
    pub async fn check_websocket_status(&self, disconnect_duration_secs: u64) -> bool {
        if disconnect_duration_secs > self.config.ws_disconnect_threshold_secs {
            self.trip(TripReason::WebSocketDisconnect(disconnect_duration_secs))
                .await;
            false
        } else {
            true
        }
    }

    /// Trip the circuit breaker
    pub async fn trip(&self, reason: TripReason) {
        let mut state = self.state.write().await;
        if *state != CircuitState::Open {
            *state = CircuitState::Open;
            *self.opened_at.write().await = Some(Utc::now());
            *self.last_trip_reason.write().await = Some(reason.clone());
            self.half_open_successes.store(0, Ordering::SeqCst);
            self.half_open_trade_count.store(0, Ordering::SeqCst);
            *self.half_open_exposure_usd.write().await = Decimal::ZERO;
            self.total_trips.fetch_add(1, Ordering::SeqCst);

            warn!("Circuit breaker TRIPPED: {}", reason);
        }
    }

    /// Manually trip the circuit
    pub async fn manual_trip(&self, reason: &str) {
        self.trip(TripReason::ManualTrip(reason.to_string())).await;
    }

    /// Transition to half-open state
    async fn transition_to_half_open(&self) {
        let mut state = self.state.write().await;
        if *state == CircuitState::Open {
            *state = CircuitState::HalfOpen;
            self.half_open_successes.store(0, Ordering::SeqCst);
            self.half_open_trade_count.store(0, Ordering::SeqCst);
            *self.half_open_exposure_usd.write().await = Decimal::ZERO;
            info!("Circuit breaker transitioning to HALF-OPEN");
        }
    }

    /// Close the circuit (resume normal operation)
    pub async fn close(&self) {
        let mut state = self.state.write().await;
        *state = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.opened_at.write().await = None;
        self.half_open_successes.store(0, Ordering::SeqCst);
        self.half_open_trade_count.store(0, Ordering::SeqCst);
        *self.half_open_exposure_usd.write().await = Decimal::ZERO;

        info!("Circuit breaker CLOSED - normal operation resumed");
    }

    async fn should_transition_to_half_open(&self) -> bool {
        if let Some(opened_at) = *self.opened_at.read().await {
            let elapsed = Utc::now().signed_duration_since(opened_at).num_seconds() as u64;
            elapsed >= self.config.recovery_timeout_secs
        } else {
            false
        }
    }

    async fn time_until_recovery(&self) -> u64 {
        if let Some(opened_at) = *self.opened_at.read().await {
            let elapsed = Utc::now().signed_duration_since(opened_at).num_seconds() as u64;
            self.config.recovery_timeout_secs.saturating_sub(elapsed)
        } else {
            self.config.recovery_timeout_secs
        }
    }

    /// Force close the circuit (manual reset)
    pub async fn force_close(&self) {
        self.close().await;
        self.half_open_successes.store(0, Ordering::SeqCst);
        *self.last_trip_reason.write().await = None;
        warn!("Circuit breaker force-closed");
    }

    /// Reset daily PnL (call at start of new trading day)
    pub async fn reset_daily(&self) {
        *self.daily_pnl.write().await = Decimal::ZERO;
        debug!("Daily PnL reset");
    }

    /// Get circuit breaker statistics
    pub async fn get_stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            state: self.state().await,
            consecutive_failures: self.consecutive_failures.load(Ordering::SeqCst),
            daily_pnl: *self.daily_pnl.read().await,
            last_success: *self.last_success.read().await,
            last_failure: *self.last_failure.read().await,
            last_trip_reason: self.last_trip_reason.read().await.clone(),
            half_open_trade_count: self.half_open_trade_count.load(Ordering::SeqCst),
            half_open_exposure_usd: *self.half_open_exposure_usd.read().await,
            total_trips: self.total_trips.load(Ordering::SeqCst),
        }
    }
}

/// Statistics for monitoring
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub daily_pnl: Decimal,
    pub last_success: Option<DateTime<Utc>>,
    pub last_failure: Option<DateTime<Utc>>,
    pub last_trip_reason: Option<TripReason>,
    pub half_open_trade_count: u32,
    pub half_open_exposure_usd: Decimal,
    pub total_trips: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_circuit_breaker_initial_state() {
        let cb = TradingCircuitBreaker::with_defaults();
        assert_eq!(cb.state().await, CircuitState::Closed);
        assert!(cb.should_allow().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_trip_on_failures() {
        let config = TradingCircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = TradingCircuitBreaker::new(config);

        cb.record_failure("error 1").await;
        cb.record_failure("error 2").await;
        assert_eq!(cb.state().await, CircuitState::Closed);

        cb.record_failure("error 3").await;
        assert_eq!(cb.state().await, CircuitState::Open);
        assert!(!cb.should_allow().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_success_resets_failures() {
        let config = TradingCircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = TradingCircuitBreaker::new(config);

        cb.record_failure("error 1").await;
        cb.record_failure("error 2").await;
        cb.record_success(dec!(10)).await;

        // Failures should be reset
        cb.record_failure("error 1").await;
        cb.record_failure("error 2").await;
        assert_eq!(cb.state().await, CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_daily_loss_limit() {
        let config = TradingCircuitBreakerConfig {
            daily_loss_limit_usd: dec!(50),
            ..Default::default()
        };
        let cb = TradingCircuitBreaker::new(config);

        cb.update_daily_pnl(dec!(-30)).await;
        assert_eq!(cb.state().await, CircuitState::Closed);

        cb.update_daily_pnl(dec!(-60)).await;
        assert_eq!(cb.state().await, CircuitState::Open);
    }

    #[tokio::test]
    async fn test_circuit_breaker_manual_close() {
        let cb = TradingCircuitBreaker::with_defaults();

        cb.manual_trip("test").await;
        assert_eq!(cb.state().await, CircuitState::Open);

        cb.force_close().await;
        assert_eq!(cb.state().await, CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_half_open_trade_limit_enforced() {
        let config = TradingCircuitBreakerConfig {
            recovery_timeout_secs: 0,
            half_open_max_trades: 1,
            ..Default::default()
        };
        let cb = TradingCircuitBreaker::new(config);

        cb.manual_trip("test").await;
        assert_eq!(cb.state().await, CircuitState::Open);

        // First trade transitions to HalfOpen and is allowed.
        assert!(cb.should_allow_trade(dec!(5)).await.unwrap());
        assert_eq!(cb.state().await, CircuitState::HalfOpen);

        // Second trade should be blocked by trade limit.
        assert!(cb.should_allow_trade(dec!(5)).await.is_err());
    }
}
