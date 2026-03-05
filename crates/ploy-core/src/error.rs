//! Core error types for the ploy trading system.
//!
//! [`CoreError`] contains only dependency-free error variants that can be shared
//! across all workspace crates without pulling in heavy dependencies like sqlx,
//! reqwest, or ethers.  The main app's [`PloyError`] wraps `CoreError` via
//! `#[from]` for seamless conversion.

use thiserror::Error;

/// Lightweight, dependency-free error type shared across the ploy workspace.
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation failed: {0}")]
    Validation(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Invalid state transition: from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },

    #[error("{0}")]
    Other(String),
}

/// Result type alias using [`CoreError`].
pub type CoreResult<T> = std::result::Result<T, CoreError>;
