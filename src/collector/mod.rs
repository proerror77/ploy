//! Data collection module for lag analysis
//!
//! Collects synchronized Binance LOB and Polymarket price data
//! for analyzing the lag between CEX price moves and prediction market reactions.

mod binance_depth;
mod binance_klines;
mod sync_collector;
pub mod backtest_collector;

pub use binance_depth::*;
pub use binance_klines::*;
pub use sync_collector::*;
pub use backtest_collector::{
    BacktestCollector, CollectorConfig, CollectorStats, ActiveMarket,
    collect_historical_klines, print_collector_status,
};
