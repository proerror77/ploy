//! Data collection module for lag analysis
//!
//! Collects synchronized Binance LOB and Polymarket price data
//! for analyzing the lag between CEX price moves and prediction market reactions.

pub mod backtest_collector;
mod polymarket_orderbook_history;
mod sync_collector;
mod token_targets;

pub use backtest_collector::{
    collect_historical_klines, print_collector_status, ActiveMarket, BacktestCollector,
    CollectorConfig, CollectorStats,
};
// Re-export Binance types from ploy-data
pub use ploy_data::binance::{
    BinanceDepthStream, DepthUpdate, LobCache, LobSnapshot, LobUpdate, OrderBookState,
    BinanceKlineClient, Kline, VolatilityStats,
};
pub use polymarket_orderbook_history::*;
pub use sync_collector::*;
pub use token_targets::*;
