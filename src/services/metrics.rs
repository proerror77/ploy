use crate::domain::StrategyState;
use crate::strategy::RiskManager;
use chrono::Utc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use tracing::info;

/// Metrics collector for observability
pub struct Metrics {
    /// Total quote updates processed
    pub quote_updates: AtomicU64,
    /// Total orders submitted
    pub orders_submitted: AtomicU64,
    /// Total orders filled
    pub orders_filled: AtomicU64,
    /// WebSocket reconnections
    pub ws_reconnections: AtomicU64,
    /// Current state
    current_state: RwLock<String>,
    /// Last update timestamp
    last_update: RwLock<i64>,
}

impl Metrics {
    /// Create a new metrics instance
    pub fn new() -> Self {
        Self {
            quote_updates: AtomicU64::new(0),
            orders_submitted: AtomicU64::new(0),
            orders_filled: AtomicU64::new(0),
            ws_reconnections: AtomicU64::new(0),
            current_state: RwLock::new("IDLE".to_string()),
            last_update: RwLock::new(Utc::now().timestamp()),
        }
    }

    /// Increment quote updates
    pub fn inc_quote_updates(&self) {
        self.quote_updates.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment orders submitted
    pub fn inc_orders_submitted(&self) {
        self.orders_submitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment orders filled
    pub fn inc_orders_filled(&self) {
        self.orders_filled.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment reconnections
    pub fn inc_reconnections(&self) {
        self.ws_reconnections.fetch_add(1, Ordering::Relaxed);
    }

    /// Update current state
    pub async fn set_state(&self, state: StrategyState) {
        *self.current_state.write().await = state.to_string();
        *self.last_update.write().await = Utc::now().timestamp();
    }

    /// Get current metrics as a formatted string
    pub async fn summary(&self, risk_manager: &RiskManager) -> String {
        let (daily_pnl, cycle_count, leg2_completions) = risk_manager.daily_stats().await;
        let completion_rate = if cycle_count > 0 {
            (leg2_completions as f64 / cycle_count as f64) * 100.0
        } else {
            0.0
        };

        let state = self.current_state.read().await;
        let risk_state = risk_manager.state().await;

        format!(
            r#"
=== PLOY TRADING BOT STATUS ===
State: {} | Risk: {}
Daily PnL: ${:.2} | Cycles: {} | Leg2 Rate: {:.1}%
Consecutive Failures: {}
Quote Updates: {} | Orders: {}/{}
WS Reconnections: {}
================================
"#,
            state,
            risk_state,
            daily_pnl,
            cycle_count,
            completion_rate,
            risk_manager.consecutive_failures(),
            self.quote_updates.load(Ordering::Relaxed),
            self.orders_filled.load(Ordering::Relaxed),
            self.orders_submitted.load(Ordering::Relaxed),
            self.ws_reconnections.load(Ordering::Relaxed),
        )
    }

    /// Export metrics in Prometheus format
    pub async fn prometheus(&self, risk_manager: &RiskManager) -> String {
        let (daily_pnl, cycle_count, leg2_completions) = risk_manager.daily_stats().await;

        format!(
            r#"# HELP ploy_quote_updates_total Total quote updates processed
# TYPE ploy_quote_updates_total counter
ploy_quote_updates_total {}

# HELP ploy_orders_submitted_total Total orders submitted
# TYPE ploy_orders_submitted_total counter
ploy_orders_submitted_total {}

# HELP ploy_orders_filled_total Total orders filled
# TYPE ploy_orders_filled_total counter
ploy_orders_filled_total {}

# HELP ploy_ws_reconnections_total WebSocket reconnections
# TYPE ploy_ws_reconnections_total counter
ploy_ws_reconnections_total {}

# HELP ploy_daily_pnl_usd Daily profit/loss in USD
# TYPE ploy_daily_pnl_usd gauge
ploy_daily_pnl_usd {}

# HELP ploy_daily_cycles_total Daily cycle count
# TYPE ploy_daily_cycles_total counter
ploy_daily_cycles_total {}

# HELP ploy_daily_leg2_completions_total Daily Leg2 completions
# TYPE ploy_daily_leg2_completions_total counter
ploy_daily_leg2_completions_total {}

# HELP ploy_consecutive_failures Current consecutive failures
# TYPE ploy_consecutive_failures gauge
ploy_consecutive_failures {}
"#,
            self.quote_updates.load(Ordering::Relaxed),
            self.orders_submitted.load(Ordering::Relaxed),
            self.orders_filled.load(Ordering::Relaxed),
            self.ws_reconnections.load(Ordering::Relaxed),
            daily_pnl,
            cycle_count,
            leg2_completions,
            risk_manager.consecutive_failures(),
        )
    }

    /// Log periodic status
    pub async fn log_status(&self, risk_manager: &RiskManager) {
        info!("{}", self.summary(risk_manager).await);
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
