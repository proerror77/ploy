//! Ploy Data — read-only data source layer.
//!
//! This crate provides data feeds from external sources:
//! - Binance (klines, depth, WebSocket streams)
//! - Deribit (IV surface) — planned
//! - ESPN (sports event data) — planned
//! - Chainlink (on-chain prices) — planned
//!
//! All data sources are READ-ONLY — no order execution.

pub mod binance;
pub mod error;
pub mod freshness;
