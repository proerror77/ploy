//! Strategy implementations for backtesting.
//!
//! Concrete strategy implementations will be added here as they are
//! migrated from the main application crate.

pub mod momentum;

pub use momentum::{
    build_results as build_momentum_results, calculate_sharpe as calculate_momentum_sharpe,
    MomentumClosedTrade,
};
