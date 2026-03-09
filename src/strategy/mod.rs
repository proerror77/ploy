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
mod research_facade;
mod runtime_facade;
pub mod runtime_order;
pub mod runtime_specs;
pub mod traits;

pub use runtime_facade::{
    engine, engine_store, executor, fund_manager, idempotency, order_request_from_intent,
    AlertLevel, DataFeed, DataFeedBuilder, DataFeedManager, FundManager, FundStatus,
    IdempotencyManager, IdempotencyResult, MarketUpdate, MomentumStrategyAdapter, OrderExecutor,
    OrderUpdate, PositionInfo, PositionSizeResult, RiskLevel, SplitArbStrategyAdapter,
    StaggeredArbAdapter, Strategy, StrategyAction, StrategyConfig, StrategyEngine, StrategyEvent,
    StrategyEventType, StrategyFactory, StrategyInfo, StrategyManager, StrategyStateInfo,
    StrategyStatus,
};

// =============================================================================
// New modular architecture
// =============================================================================

pub mod core;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod nba_comeback;
pub mod pattern_memory;
pub mod sports;

// =============================================================================
// Operational modules used by current runtime
// =============================================================================

pub mod backtest;
pub mod backtest_feed;
pub mod backtest_recorder;
pub mod backtest_report;
pub mod calculations;
#[cfg(feature = "claimer_daemon")]
pub mod claimer;
pub mod deribit_probability_arb;
pub mod directional_backtest;
pub mod dump_hedge;
pub mod execution;
pub mod execution_sim;
pub mod fee_model;
pub mod gamma_scalping;
pub mod garch_probability_backtest;
pub mod integrity;
pub mod liquidity_vacuum_backtest;
pub mod momentum;
pub mod momentum_backtest;
pub mod momentum_runtime_config;
pub mod multi_event;
pub mod multi_outcome;
pub mod paper_runner;
#[cfg(feature = "analysis")]
pub mod parquet_analysis;
pub mod position_manager;
pub mod probability;
pub mod reconciliation;
pub mod reverse_engineered;
pub mod risk_mgmt;
pub mod signal;
pub mod split_arb;
pub mod staggered_arb_backtest;
pub mod staggered_arb_live;
pub mod trade_logger;
pub mod trading_costs;
pub mod volatility;
pub mod volatility_arb;

// Runtime re-exports
#[cfg(feature = "claimer_daemon")]
pub use claimer::{
    ensure_account_claimer_daemon, AutoClaimer, ClaimResult, ClaimerConfig, RedeemablePosition,
};
pub use dump_hedge::{
    DumpHedgeConfig, DumpHedgeEngine, DumpHedgeStats, EnhancedDumpSignal, HedgeResult,
    PendingHedge, ProgressiveHedgeSignal, StopLossReason, StopLossSignal,
};
pub use event_edge::core::{EventEdgeCore, EventEdgeState, TradeDecision};
pub use fee_model::{AllInCost, FeeModel, FeeRateCache};
pub use gamma_scalping::{
    BinaryGreeks, GammaScalpingConfig, GammaScalpingStrategy, RebalanceAction, Rebalancer, Straddle,
};
pub use momentum::{
    Direction, EventInfo, EventMatcher, ExitConfig, ExitManager, ExitReason, MomentumConfig,
    MomentumDetector, MomentumEngine, MomentumSignal, Position,
};
pub use momentum_runtime_config::{CryptoEntryMode, CryptoTradingConfig};
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
pub use position_manager::{
    Position as PersistedPosition, PositionManager, PositionStatus as PersistedPositionStatus,
    PositionSummary,
};
pub use probability::{estimate_probability, full_estimate, Features, ProbabilityEstimate};
pub use reconciliation::{
    DiscrepancySeverity, PositionDiscrepancy, ReconciliationConfig, ReconciliationResult,
    ReconciliationService,
};
pub use registry::{EventFilter, EventStatus, EventUpsertRequest, RegisteredEvent};
pub use research_facade::{
    binary_call_prob_forward, calculate_kline_volatility, extract_profile_snapshot,
    infer_strategy_params, interpolate_iv_linear, load_klines_from_csv, load_pm_prices_from_csv,
    load_report, net_edge, norm_cdf, parse_polymarket_question, run_deribit_probability_arb,
    run_paper_trading, run_reverse_engineered_profile_paper, BacktestEngine, BacktestRecorder,
    BacktestReport, BacktestResults, BacktestSignal, BacktestTrade, DeribitProbabilityArbConfig,
    DirectionalBacktestConfig, DirectionalBacktestEngine, DirectionalClosedTrade, ExecutionResult,
    ExecutionSimConfig, ExecutionSimulator, GarchProbabilityBacktestConfig,
    GarchProbabilityBacktestEngine, GarchProbabilityClosedTrade, KlineRecord,
    LiquidityVacuumBacktestConfig, LiquidityVacuumBacktestEngine, LiquidityVacuumClosedTrade,
    MarketSnapshot, NullRecorder, PMPriceRecord, PaperSignal, PaperTrader, PaperTradingConfig,
    PaperTradingRunner, PaperTradingStats, ParsedPolymarketQuestion, PendingTrade,
    PgBacktestRecorder, ReverseDryRunResult, ReverseEngineeredConfig, ReverseProfileSnapshot,
    ReverseStrategyParams, ReverseTradeEvent, SignalType, StaggeredArbBacktestConfig,
    StaggeredArbBacktestEngine, StaggeredArbClosedTrade, Suggestion, SuggestionPriority,
    SurfacePoint, TrackedMarket, VolSurfaceSnapshot, REVERSE_PROFILE_STRATEGY_NAME,
    REVERSE_PROFILE_STRATEGY_SLUG,
};
pub use risk_mgmt::risk::RiskManager;
pub use risk_mgmt::slippage::{MarketDepth, SlippageCheck, SlippageConfig, SlippageProtection};
pub use signal::SignalDetector;
pub use split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition, PositionStatus,
    SplitArbConfig, SplitArbEngine,
};
pub use trade_logger::{
    BucketStats, SymbolStats, TradeContext, TradeLogger, TradeOutcome, TradeRecord, TradingStats,
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
pub use sports::{SportsLeague, SportsMarketDiscovery};
