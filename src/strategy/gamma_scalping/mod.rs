//! Gamma scalping strategy for Polymarket crypto binary options.
//!
//! Profits from realized volatility exceeding implied volatility by maintaining
//! delta-neutral straddle positions (long UP + long DOWN tokens) and dynamically
//! rebalancing as the underlying price moves.

pub mod config;
pub mod greeks;
pub mod rebalancer;
pub mod strategy;

pub use config::GammaScalpingConfig;
pub use greeks::{binary_greeks, realized_vol_from_closes, BinaryGreeks};
pub use rebalancer::{RebalanceAction, Rebalancer, Straddle};
pub use strategy::GammaScalpingStrategy;
