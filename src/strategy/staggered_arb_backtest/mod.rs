//! Staggered Arbitrage Backtest Engine — 時間差套利回測
//!
//! Polymarket binary options have `up_ask + down_ask > 1` at any point (market maker spread).
//! Buying both sides simultaneously always loses. But by using volatility prediction to time
//! entries — buying the side about to get expensive first, then buying the other side after
//! price movement — the total cost of both legs can be < $1, yielding risk-free arbitrage.
//!
//! When both legs are filled, they are immediately merged (redeemed) for $1.00 per share,
//! without waiting for settlement. This dramatically improves capital turnover.
//!
//! Usage:
//!   ploy strategy backtest staggered-arb --symbols BTCUSDT --save --json

mod config;
mod engine;
mod state;

pub use config::StaggeredArbBacktestConfig;
pub use engine::StaggeredArbBacktestEngine;
pub use ploy_backtest::strategies::StaggeredArbClosedTrade;
pub use state::ArbPositionState;
