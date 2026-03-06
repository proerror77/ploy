//! Data-layer error types.
//!
//! Lightweight error enum covering network (WebSocket, HTTP) and internal
//! failures. Converts into `ploy_core::CoreError` for upstream consumers.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DataError {
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, DataError>;

impl From<DataError> for ploy_core::CoreError {
    fn from(e: DataError) -> Self {
        ploy_core::CoreError::Other(e.to_string())
    }
}
