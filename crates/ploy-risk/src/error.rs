//! Risk error types for the ploy-risk crate.

use thiserror::Error;

/// Error type for risk management operations.
#[derive(Error, Debug, Clone)]
pub enum RiskError {
    #[error("Validation failed: {0}")]
    Validation(String),

    #[error("Max exposure exceeded: limit ${limit}, requested ${requested}")]
    MaxExposureExceeded {
        limit: rust_decimal::Decimal,
        requested: rust_decimal::Decimal,
    },

    #[error("Consecutive failures: {count} >= {threshold}")]
    ConsecutiveFailures { count: u32, threshold: u32 },

    #[error("Daily loss limit: current ${current}, limit ${limit}")]
    DailyLossLimit {
        current: rust_decimal::Decimal,
        limit: rust_decimal::Decimal,
    },

    #[error("Insufficient time remaining: {remaining_secs}s < {min_secs}s")]
    InsufficientTime { remaining_secs: u64, min_secs: u64 },

    #[error("Spread too wide: {spread_bps} bps > {max_bps} bps")]
    SpreadTooWide { spread_bps: u32, max_bps: u32 },

    #[error("Trading halted: {reason}")]
    TradingHalted { reason: String },
}

/// Result type alias using [`RiskError`].
pub type RiskResult<T> = std::result::Result<T, RiskError>;
