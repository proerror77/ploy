//! Shared configuration types used across ploy workspace crates.
//!
//! These are **copies** of the canonical types in the main `src/config.rs`.
//! They exist here so that downstream crates (`ploy-risk`, `ploy-backtest`, etc.)
//! can depend on `ploy-core` without pulling in the full application config.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ── DryRunConfig ────────────────────────────────────────────────────────────

/// Simple flag to enable/disable dry-run mode (no real orders).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DryRunConfig {
    /// Enable dry run mode (no real orders)
    pub enabled: bool,
}

impl Default for DryRunConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

// ── DatabaseConfig ──────────────────────────────────────────────────────────

/// Database connection parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL
    pub url: String,
    /// Maximum connections in pool
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    5
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://localhost/ploy".to_string(),
            max_connections: default_max_connections(),
        }
    }
}

// ── LoggingConfig ───────────────────────────────────────────────────────────

/// Logging output configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Enable JSON formatted logs
    #[serde(default)]
    pub json: bool,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            json: false,
        }
    }
}

// ── RiskConfig ──────────────────────────────────────────────────────────────

/// Risk management limits shared across strategies and the risk gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum single position exposure in USD
    pub max_single_exposure_usd: Decimal,
    /// Minimum seconds remaining to allow new Leg1
    pub min_remaining_seconds: u64,
    /// Number of consecutive failures before circuit breaker
    pub max_consecutive_failures: u32,
    /// Daily loss limit in USD (absolute value)
    pub daily_loss_limit_usd: Decimal,
    /// Seconds before round end to force Leg2 action
    pub leg2_force_close_seconds: u64,

    // === Fund Management ===
    /// Maximum concurrent positions (0 = unlimited)
    #[serde(default)]
    pub max_positions: u32,
    /// Maximum positions per symbol (default: 1)
    #[serde(default = "default_max_positions_per_symbol")]
    pub max_positions_per_symbol: u32,
    /// Percentage of available balance per trade (e.g., 0.10 = 10%)
    #[serde(default)]
    pub position_size_pct: Option<Decimal>,
    /// Fixed USD amount per trade (overrides position_size_pct if set)
    #[serde(default)]
    pub fixed_amount_usd: Option<Decimal>,
    /// Minimum balance to maintain (won't trade if balance below this)
    #[serde(default)]
    pub min_balance_usd: Decimal,
}

fn default_max_positions_per_symbol() -> u32 {
    1
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_single_exposure_usd: Decimal::new(100, 0),
            min_remaining_seconds: 30,
            max_consecutive_failures: 3,
            daily_loss_limit_usd: Decimal::new(500, 0),
            leg2_force_close_seconds: 20,
            max_positions: 3,
            max_positions_per_symbol: default_max_positions_per_symbol(),
            position_size_pct: None,
            fixed_amount_usd: Some(Decimal::ONE),
            min_balance_usd: Decimal::new(2, 0),
        }
    }
}

// ── ExecutionConfig ─────────────────────────────────────────────────────────

/// Order execution parameters (timeouts, retries, spread limits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Exchange backend (`polymarket` or `kalshi`)
    #[serde(default = "default_execution_exchange")]
    pub exchange: String,
    /// Order timeout in milliseconds
    pub order_timeout_ms: u64,
    /// Maximum retry attempts for order submission
    pub max_retries: u8,
    /// Maximum spread in basis points to accept
    pub max_spread_bps: u32,
    /// Polling interval for order status in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    /// Best-effort post-submit fill confirmation via polling
    #[serde(default)]
    pub confirm_fills: bool,
    /// Maximum time to wait for a terminal order status (ms)
    #[serde(default = "default_confirm_fill_timeout_ms")]
    pub confirm_fill_timeout_ms: u64,
    /// Maximum quote age in seconds before rejecting trade (default: 5s)
    #[serde(default = "default_max_quote_age")]
    pub max_quote_age_secs: u64,
}

fn default_execution_exchange() -> String {
    "polymarket".to_string()
}

fn default_poll_interval() -> u64 {
    500
}

fn default_confirm_fill_timeout_ms() -> u64 {
    2000
}

fn default_max_quote_age() -> u64 {
    5
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            exchange: default_execution_exchange(),
            order_timeout_ms: 5000,
            max_retries: 3,
            max_spread_bps: 500,
            poll_interval_ms: default_poll_interval(),
            confirm_fills: false,
            confirm_fill_timeout_ms: default_confirm_fill_timeout_ms(),
            max_quote_age_secs: default_max_quote_age(),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_defaults_to_enabled() {
        let cfg = DryRunConfig::default();
        assert!(cfg.enabled);
    }

    #[test]
    fn database_defaults_are_sensible() {
        let cfg = DatabaseConfig::default();
        assert_eq!(cfg.max_connections, 5);
        assert!(!cfg.url.is_empty());
    }

    #[test]
    fn logging_defaults_to_info() {
        let cfg = LoggingConfig::default();
        assert_eq!(cfg.level, "info");
        assert!(!cfg.json);
    }

    #[test]
    fn risk_defaults_are_sensible() {
        let cfg = RiskConfig::default();
        assert!(cfg.max_single_exposure_usd > Decimal::ZERO);
        assert!(cfg.daily_loss_limit_usd > Decimal::ZERO);
        assert_eq!(cfg.max_positions_per_symbol, 1);
    }

    #[test]
    fn execution_defaults_to_polymarket() {
        let cfg = ExecutionConfig::default();
        assert_eq!(cfg.exchange, "polymarket");
        assert_eq!(cfg.order_timeout_ms, 5000);
        assert_eq!(cfg.max_retries, 3);
    }
}
