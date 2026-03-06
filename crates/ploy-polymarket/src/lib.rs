//! Ploy Polymarket — Polymarket execution layer.
//!
//! This crate handles all Polymarket-specific functionality:
//! - CLOB REST API (order placement, cancellation, queries)
//! - WebSocket connections (orderbook, trades)
//! - Wallet signing, nonce management, authentication
//! - CTF contract interaction
//! - Market discovery and search

pub mod error;
pub mod signing;

pub use signing::Wallet;
