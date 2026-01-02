//! Core shared components for all strategies
//!
//! This module contains the infrastructure that all strategies share:
//! - Order execution
//! - Risk management
//! - Position tracking

pub mod executor;
pub mod position;
pub mod risk;

pub use executor::{OrderExecutor, ExecutionResult, ExecutionConfig};
pub use position::{PositionManager, Position, PositionUpdate};
pub use risk::{RiskManager, RiskConfig, RiskState, RiskCheck};
