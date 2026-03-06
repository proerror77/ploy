//! Strategy implementations for backtesting.
//!
//! Concrete strategy implementations will be added here as they are
//! migrated from the main application crate.

pub mod directional;
pub mod momentum;
pub mod staggered_arb;

pub use directional::{
    adjust_fair_value_for_price_to_beat, build_results as build_directional_results,
    calculate_sharpe as calculate_directional_sharpe, estimate_fair_value,
    DirectionalBacktestConfig, DirectionalClosedTrade,
};
pub use momentum::{
    build_results as build_momentum_results, calculate_sharpe as calculate_momentum_sharpe,
    MomentumClosedTrade,
};
pub use staggered_arb::{
    build_results as build_staggered_arb_results,
    calculate_sharpe as calculate_staggered_arb_sharpe, StaggeredArbClosedTrade,
};
