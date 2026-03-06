//! Ploy Backtest — unified backtesting framework for the ploy trading system.
//!
//! This crate provides:
//! - Historical data loading from CSV/JSON/DB
//! - Unified backtest engine with pluggable strategies
//! - Realistic execution simulation (spread, partial fills, market impact)
//! - Trade recording and signal persistence
//! - Report generation with calibration analysis
//!
//! ## Architecture
//!
//! - `engine` — Core backtest types (snapshots, trades, results) and data loading
//! - `signal` — `BacktestStrategy` trait and signal types
//! - `feed` — `MarketFeed` trait and `HistoricalFeed` (DB + CSV loaders)
//! - `recorder` — `BacktestRecorder` trait with null and Postgres implementations
//! - `report` — DB-backed report generation with calibration and profitability analysis
//! - `execution_sim` — Realistic fill simulation (spread, depth, impact, delay)
//! - `fee_model` — Polymarket parabolic fee curve
//! - `strategies` — Pluggable strategy implementations

pub mod engine;
pub mod execution_sim;
pub mod fee_model;
pub mod feed;
pub mod recorder;
pub mod report;
pub mod signal;
pub mod strategies;

// Re-export key types for convenience
pub use engine::{
    calculate_kline_volatility, load_klines_from_csv, load_pm_prices_from_csv, BacktestResults,
    BacktestTrade, KlineRecord, MarketSnapshot, PMPriceRecord, PaperSignal, PaperTradingStats,
    SymbolStats,
};
pub use execution_sim::{ExecutionResult, ExecutionSimConfig, ExecutionSimulator};
pub use feed::{HistoricalFeed, MarketFeed, MarketUpdate, UpdateType};
#[cfg(feature = "persistence")]
pub use recorder::PgBacktestRecorder;
pub use recorder::{BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType};
pub use signal::{BacktestStrategy, ExitReason, OpenTrade, Signal, SignalDirection};
