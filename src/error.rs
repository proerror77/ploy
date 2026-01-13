use thiserror::Error;

/// Main error type for the trading bot
#[derive(Error, Debug)]
pub enum PloyError {
    // Configuration errors
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    // Database errors
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    // Network errors
    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    // Serialization errors
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    // Market data errors
    #[error("Market data unavailable: {0}")]
    MarketDataUnavailable(String),

    #[error("Invalid market data: {0}")]
    InvalidMarketData(String),

    #[error("Round not found: {0}")]
    RoundNotFound(String),

    // Order execution errors
    #[error("Order submission failed: {0}")]
    OrderSubmission(String),

    #[error("Order timeout: {0}")]
    OrderTimeout(String),

    #[error("Order rejected: {0}")]
    OrderRejected(String),

    #[error("Insufficient liquidity: {0}")]
    InsufficientLiquidity(String),

    // State machine errors
    #[error("Invalid state transition: from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },

    #[error("Unexpected state: {0}")]
    UnexpectedState(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    // Data availability errors (for 24/7 reliability)
    #[error("Quote unavailable for token: {token_id}")]
    QuoteUnavailable { token_id: String },

    #[error("Address parsing error: {0}")]
    AddressParsing(String),

    #[error("Component failure: {component} - {reason}")]
    ComponentFailure { component: String, reason: String },

    #[error("Stale data: {0}")]
    StaleData(String),

    // Risk management errors
    #[error("Risk limit exceeded: {0}")]
    RiskLimitExceeded(String),

    #[error("Circuit breaker triggered: {0}")]
    CircuitBreakerTriggered(String),

    #[error("Daily loss limit reached: {0}")]
    DailyLossLimit(String),

    // Validation errors
    #[error("Validation failed: {0}")]
    Validation(String),

    // Crypto/signing errors
    #[error("Wallet error: {0}")]
    Wallet(String),

    #[error("Signature error: {0}")]
    Signature(String),

    // Authentication errors
    #[error("Authentication error: {0}")]
    Auth(String),

    // IO errors
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    // Generic errors
    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Result type alias for PloyError
pub type Result<T> = std::result::Result<T, PloyError>;

/// Specific error types for order execution
#[derive(Error, Debug, Clone)]
pub enum OrderError {
    #[error("Order not found: {order_id}")]
    NotFound { order_id: String },

    #[error("Order already filled")]
    AlreadyFilled,

    #[error("Order already cancelled")]
    AlreadyCancelled,

    #[error("Partial fill: requested {requested}, filled {filled}")]
    PartialFill { requested: u64, filled: u64 },

    #[error("Price slippage exceeded: limit {limit}, actual {actual}")]
    SlippageExceeded {
        limit: rust_decimal::Decimal,
        actual: rust_decimal::Decimal,
    },

    #[error("Timeout after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },

    #[error("Max retries exceeded: {attempts}")]
    MaxRetriesExceeded { attempts: u8 },
}

/// Specific error types for risk management
#[derive(Error, Debug, Clone)]
pub enum RiskError {
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

impl From<OrderError> for PloyError {
    fn from(err: OrderError) -> Self {
        PloyError::OrderSubmission(err.to_string())
    }
}

impl From<RiskError> for PloyError {
    fn from(err: RiskError) -> Self {
        PloyError::RiskLimitExceeded(err.to_string())
    }
}
