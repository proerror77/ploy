//! Ploy Core — shared types and traits for the ploy trading system.
//!
//! This crate contains domain types, error definitions, and strategy traits
//! that are shared across all ploy workspace crates.

pub mod error;

pub use error::{CoreError, CoreResult};
