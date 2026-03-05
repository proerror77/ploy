//! Strategy implementations
//!
//! Contains concrete strategy implementations:
//! - TwoLegStrategy: Two-leg arbitrage on prediction markets
//! - MomentumStrategy: Momentum-based trading
//! - GammaScalpingStrategy: Gamma scalping on crypto binary options

pub mod two_leg;
pub mod momentum_strat;

pub use two_leg::TwoLegStrategy;
pub use momentum_strat::MomentumStrategy;
