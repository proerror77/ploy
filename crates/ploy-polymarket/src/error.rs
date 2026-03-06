//! Error types for the ploy-polymarket crate.

use thiserror::Error;

/// Polymarket-specific error type.
///
/// Covers signing, HMAC auth, order construction, and nonce management.
/// The main app's `PloyError` can wrap this via `#[from]` for seamless conversion.
#[derive(Error, Debug)]
pub enum PolymarketError {
    #[error("Order submission error: {0}")]
    OrderSubmission(String),

    #[error("Signature error: {0}")]
    Signature(String),

    #[error("Wallet error: {0}")]
    Wallet(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Result type alias for [`PolymarketError`].
pub type Result<T> = std::result::Result<T, PolymarketError>;
