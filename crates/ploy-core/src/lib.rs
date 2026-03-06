//! Ploy Core — shared types and traits for the ploy trading system.
//!
//! This crate contains domain types, error definitions, strategy traits,
//! and shared configuration types used across all ploy workspace crates.

pub mod config;
pub mod domain;
pub mod error;
pub mod strategy;

pub use config::{DatabaseConfig, DryRunConfig, ExecutionConfig, LoggingConfig, RiskConfig};
pub use domain::{
    Domain, OrderSide, OrderStatus, OrderType, RiskState, Side, StrategyState, TimeInForce,
    Timeframe,
};
pub use error::{CoreError, CoreResult};
