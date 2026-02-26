//! Strategy module
//!
//! Contains trading strategies and supporting infrastructure.
//!
//! ## Architecture
//!
//! Strategies are organized by market type:
//! - `core/` - Shared abstractions and generic split arbitrage engine
//! - `crypto/` - Crypto UP/DOWN markets (BTC, ETH, SOL)
//! - `sports/` - Sports betting markets (NBA, NFL, etc.)
//!
//! ## Usage
//!
//! ```bash
//! # Crypto markets
//! ploy crypto split-arb --coins BTC,ETH,SOL
//!
//! # Sports markets
//! ploy sports split-arb --leagues NBA,NFL
//! ```

// =============================================================================
// Strategy trait and core types
// =============================================================================

pub mod adapters;
pub mod event_edge;
pub mod event_models;
pub mod feeds;
pub mod manager;
pub mod registry;
pub mod traits;

pub use traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, RiskLevel, Strategy,
    StrategyAction, StrategyConfig, StrategyEvent, StrategyEventType, StrategyStateInfo,
};

pub use adapters::{MomentumStrategyAdapter, SplitArbStrategyAdapter};
pub use feeds::{DataFeedBuilder, DataFeedManager};
pub use manager::{StrategyFactory, StrategyInfo, StrategyManager, StrategyStatus};

// =============================================================================
// New modular architecture
// =============================================================================

pub mod core;
pub mod crypto;
pub mod nba_comeback;
pub mod pattern_memory;
pub mod sports;

// =============================================================================
// Operational modules used by current runtime
// =============================================================================

pub mod backtest;
pub mod backtest_feed;
pub mod calculations;
pub mod claimer;
pub mod dump_hedge;
pub mod execution;
pub mod execution_sim;
pub mod integrity;
pub mod momentum;
pub mod momentum_backtest;
pub mod multi_event;
pub mod multi_outcome;
pub mod paper_runner;
#[cfg(feature = "analysis")]
pub mod parquet_analysis;
pub mod position_manager;
pub mod reconciliation;
pub mod risk_mgmt;
pub mod signal;
pub mod split_arb;
pub mod trade_logger;
pub mod trading_costs;
pub mod volatility;
pub mod volatility_arb;
pub mod directional_backtest;
pub mod fee_model;
pub mod probability;

// Runtime re-exports
pub use claimer::{AutoClaimer, ClaimResult, ClaimerConfig, RedeemablePosition};
pub use execution::engine::StrategyEngine;
pub use execution::engine_store;
pub use execution::executor::OrderExecutor;
pub use execution::fund_manager::{FundManager, FundStatus, PositionSizeResult};
pub use execution::idempotency::{IdempotencyManager, IdempotencyResult};

// Backward-compat module aliases (external code uses crate::strategy::executor::X)
pub use execution::engine;
pub use execution::executor;
pub use execution::fund_manager;
pub use execution::idempotency;
pub use multi_event::{ArbitrageOpportunity, EventSummary, EventTracker, MultiEventMonitor};
pub use multi_outcome::{
    analyze_market_making_opportunity,
    analyze_near_settlement,
    detect_split_merge_opportunity,
    // Core types
    fetch_multi_outcome_event,
    generate_ev_table,
    ArbitrageType,
    // EV analysis
    ExpectedValue,
    MarketMakingAction,
    // Market making
    MarketMakingConfig,
    MarketMakingOpportunity,
    MultiOutcomeArbitrage,
    MultiOutcomeMonitor,
    // Near-settlement analysis
    NearSettlementAnalysis,
    Outcome,
    OutcomeDirection,
    OutcomeSummary,
    // Split/Merge arbitrage
    SplitMergeOpportunity,
    SplitMergeType,
    POLYMARKET_FEE_RATE,
};
pub use trade_logger::{
    BucketStats, SymbolStats, TradeContext, TradeLogger, TradeOutcome, TradeRecord, TradingStats,
};

pub use backtest::{
    calculate_kline_volatility, load_klines_from_csv, load_pm_prices_from_csv, BacktestEngine,
    BacktestResults, BacktestTrade, KlineRecord, MarketSnapshot, PMPriceRecord, PaperSignal,
    PaperTrader, PaperTradingStats,
};
pub use dump_hedge::{
    DumpHedgeConfig, DumpHedgeEngine, DumpHedgeStats, EnhancedDumpSignal, HedgeResult,
    PendingHedge, ProgressiveHedgeSignal, StopLossReason, StopLossSignal,
};
pub use event_edge::core::{EventEdgeCore, EventEdgeState, TradeDecision};
pub use event_edge::{run_event_edge, EventEdgeConfig};
pub use execution_sim::{ExecutionResult, ExecutionSimConfig, ExecutionSimulator};
pub use momentum::{
    Direction, EventInfo, EventMatcher, ExitConfig, ExitManager, ExitReason, MomentumConfig,
    MomentumDetector, MomentumEngine, MomentumSignal, Position,
};
pub use nba_comeback::nba_data_collector::{
    CollectorConfig as NbaCollectorConfig, DataCollector as NbaDataCollector,
    GameState as NbaGameState, MarketSnapshot as NbaMarketSnapshot, OrderbookData, TeamStats,
};
pub use nba_comeback::nba_entry::{
    EntryConfig, EntryDecision, EntryLogic, EntrySignal, PartialSignal,
};
pub use nba_comeback::nba_exit::{
    ExitConfig as NbaExitConfig, ExitDecision, ExitLogic, ExitUrgency, PositionState,
};
pub use nba_comeback::nba_filters::{FilterConfig, FilterResult, MarketContext, MarketFilters};
pub use nba_comeback::nba_state_machine::{
    StateEvent as NbaStateEvent, StateMachine as NbaStateMachine, StrategyState as NbaStrategyState,
};
pub use nba_comeback::nba_winprob::{
    GameFeatures, LiveWinProbModel, ModelMetadata, WinProbCoefficients, WinProbPrediction,
};
pub use paper_runner::{run_paper_trading, PaperTradingConfig, PaperTradingRunner, TrackedMarket};
pub use position_manager::{
    Position as PersistedPosition, PositionManager, PositionStatus as PersistedPositionStatus,
    PositionSummary,
};
pub use reconciliation::{
    DiscrepancySeverity, PositionDiscrepancy, ReconciliationConfig, ReconciliationResult,
    ReconciliationService,
};
pub use registry::{EventFilter, EventStatus, EventUpsertRequest, RegisteredEvent};
pub use risk_mgmt::risk::RiskManager;
pub use risk_mgmt::slippage::{MarketDepth, SlippageCheck, SlippageConfig, SlippageProtection};
pub use signal::SignalDetector;
pub use split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition, PositionStatus,
    SplitArbConfig, SplitArbEngine,
};
pub use trading_costs::{
    OrderType, TradingCostBreakdown, TradingCostCalculator, TradingCostConfig,
};
pub use volatility::{
    ActiveEvent, EventRecord, EventTracker as VolatilityEventTracker, VolatilityConfig,
    VolatilityDetector, VolatilitySignal,
};
pub use volatility_arb::{
    calculate_fair_yes_price, calculate_implied_volatility, calculate_kelly_fraction,
    MarketPricing, VolArbSignal, VolArbStats, VolArbTrade, VolatilityArbConfig,
    VolatilityArbEngine, VolatilityEstimate,
};
pub use probability::{estimate_probability, full_estimate, Features, ProbabilityEstimate};
pub use fee_model::{AllInCost, FeeModel, FeeRateCache};
pub use directional_backtest::{DirectionalBacktestConfig, DirectionalBacktestEngine, DirectionalClosedTrade};

// New consolidated modules
pub use calculations::{
    calculate_cycle_pnl, check_leg2_condition, effective_sum_target, TradingCalculator,
    DEFAULT_SLIPPAGE, MIN_PROFIT_TARGET, POLYMARKET_FEE_RATE as CALC_FEE_RATE,
};
pub use risk_mgmt::validation::{
    leg1_entry_chain, leg2_entry_chain, ExposureValidator, RiskStateValidator, SpreadValidator,
    SumTargetValidator, TimeRemainingValidator, ValidationChain, ValidationContext,
    ValidationError, Validator,
};

// Backward-compat module aliases for risk/slippage/validation
pub use risk_mgmt::risk;
pub use risk_mgmt::slippage;
pub use risk_mgmt::validation;

// =============================================================================
// New architecture re-exports
// =============================================================================

// Core types
pub use core::{
    ArbSide as CoreArbSide, ArbStats as CoreArbStats, BinaryMarket,
    HedgedPosition as CoreHedgedPosition, MarketDiscovery, MarketType,
    PartialPosition as CorePartialPosition, PositionStatus as CorePositionStatus, PriceCache,
    SplitArbConfig as CoreSplitArbConfig, SplitArbEngine as CoreSplitArbEngine,
};

// Crypto strategies
pub use crypto::{run_crypto_split_arb, CryptoMarketDiscovery, CryptoSplitArbConfig};

// Sports strategies
pub use sports::{run_sports_split_arb, SportsLeague, SportsMarketDiscovery, SportsSplitArbConfig};
