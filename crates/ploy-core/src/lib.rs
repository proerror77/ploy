//! Ploy Core — shared types and traits for the ploy trading system.
//!
//! This crate contains domain types, error definitions, and strategy traits
//! that are shared across all ploy workspace crates.

pub mod domain;
pub mod error;
pub mod strategy;

pub use domain::{
    Domain, OrderSide, OrderStatus, OrderType, RiskState, Side, StrategyState, TimeInForce,
    Timeframe,
};
pub use error::{CoreError, CoreResult};
