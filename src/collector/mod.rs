//! Data collection module for lag analysis
//!
//! Collects synchronized Binance LOB and Polymarket price data
//! for analyzing the lag between CEX price moves and prediction market reactions.

mod binance_depth;
mod binance_klines;
mod sync_collector;

pub use binance_depth::*;
pub use binance_klines::*;
pub use sync_collector::*;
