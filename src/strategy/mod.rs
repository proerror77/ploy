//! Strategy module
//!
//! Contains trading strategies and supporting infrastructure.
//!
//! ## Architecture
//!
//! - `traits/adapters/feeds/manager` define the orchestration contract layer.
//! - `execution` contains order execution pipeline and state machine runtime.
//! - `risk` is the canonical risk-domain facade (`risk_mgmt` internals).
//! - `impls` groups concrete strategy implementations and strategy utilities.

// =============================================================================
// Core contract surface
// =============================================================================

pub mod adapters;
pub mod event_models;
pub mod feeds;
pub mod manager;
pub mod traits;

pub use traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, RiskLevel, Strategy,
    StrategyAction, StrategyConfig, StrategyEvent, StrategyEventType, StrategyStateInfo,
};

pub use adapters::{MomentumStrategyAdapter, SplitArbStrategyAdapter};
pub use feeds::{DataFeedBuilder, DataFeedManager};
pub use manager::{StrategyFactory, StrategyInfo, StrategyManager, StrategyStatus};

// =============================================================================
// Subdomain modules
// =============================================================================

pub mod core;
pub mod execution;
pub mod risk;
pub mod impls;

// =============================================================================
// Strategy implementations and runtime modules
// =============================================================================

pub mod backtest;
pub mod backtest_feed;
pub mod backtest_recorder;
pub mod backtest_report;
pub mod calculations;
#[cfg(feature = "claimer_daemon")]
pub mod claimer;
pub mod crypto;
pub mod deribit_probability_arb;
pub mod directional_backtest;
pub mod event_edge;
pub mod execution_sim;
pub mod fee_model;
pub mod integrity;
pub mod momentum;
pub mod momentum_backtest;
pub mod multi_event;
pub mod multi_outcome;
pub mod nba_comeback;
pub mod paper_runner;
#[cfg(feature = "analysis")]
pub mod parquet_analysis;
pub mod pattern_memory;
pub mod position_manager;
pub mod probability;
pub mod reconciliation;
pub mod registry;
pub mod reverse_engineered;
pub mod risk_mgmt;
pub mod signal;
pub mod split_arb;
pub mod sports;
pub mod staggered_arb_backtest;
pub mod staggered_arb_live;
pub mod trade_logger;
pub mod trading_costs;
pub mod volatility;
pub mod volatility_arb;
