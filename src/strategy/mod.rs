//! Strategy module
//!
//! Contains trading strategies and supporting infrastructure.
//!
//! ## Architecture
//!
//! Canonical live strategy ownership:
//! - New live strategy implementations belong on the `Strategy` contract in
//!   [`crate::strategy::traits`].
//! - New live strategy runtime work should plug into the canonical
//!   coordinator-managed strategy runtime path.
//! - `TradingAgent` / `DomainAgent` are retired historical paths and are not
//!   available entry points for new live strategies.
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
// Core contract surface
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
mod sports_facade;
pub mod traits;

pub use runtime_facade::{
    engine, engine_store, executor, fund_manager, idempotency, order_request_from_intent,
    AlertLevel, DataFeed, DataFeedBuilder, DataFeedManager, FundManager, FundStatus,
    IdempotencyManager, IdempotencyResult, MarketUpdate, MomentumStrategyAdapter, OrderExecutor,
    OrderPurpose, OrderUpdate, PositionInfo, PositionSizeResult, RiskLevel, SplitArbStrategyAdapter,
    StaggeredArbAdapter, Strategy, StrategyAction, StrategyConfig, StrategyEngine, StrategyEvent,
    StrategyEventType, StrategyFactory, StrategyInfo, StrategyManager, StrategyStateInfo,
    StrategyStatus,
};

// =============================================================================
// Subdomain modules
// =============================================================================

pub mod core;
pub mod crypto;
pub mod crypto_lob_ml;
pub mod crypto_rl_policy;
pub mod impls;
pub mod nba_comeback;
pub mod pattern_memory;
pub mod pm_5m_directional;
pub mod pm_5m_directional_backtest;
pub mod risk;
pub mod sports;

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
pub mod composable_crypto;
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
pub mod multi_outcome;
pub mod paper_runner;
pub mod position_manager;
pub mod probability;
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
pub use composable_crypto::ComposableCryptoStrategy;
pub use dump_hedge::{
    DumpHedgeConfig, DumpHedgeEngine, DumpHedgeStats, EnhancedDumpSignal, HedgeResult,
    PendingHedge, ProgressiveHedgeSignal, StopLossReason, StopLossSignal,
};
pub use event_edge::core::{EventEdgeCore, EventEdgeState, TradeDecision};
pub use event_edge::{scan_event_edge_once, EventEdgeScan, EdgeRow};
pub use fee_model::{AllInCost, FeeModel, FeeRateCache};
pub use gamma_scalping::{
    BinaryGreeks, GammaScalpingConfig, GammaScalpingStrategy, RebalanceAction, Rebalancer, Straddle,
};
pub use momentum::{
    Direction, EventInfo, EventMatcher, ExitConfig, ExitManager, ExitReason, MomentumConfig,
    MomentumDetector, MomentumEngine, MomentumSignal, Position,
};
pub use momentum_runtime_config::{CryptoEntryMode, CryptoTradingConfig};
pub use multi_outcome::{
    analyze_market_making_opportunity,
    analyze_near_settlement,
    detect_split_merge_opportunity,
    fetch_multi_outcome_event,
    generate_ev_table,
    ArbitrageType,
    ExpectedValue,
    MarketMakingAction,
    MarketMakingConfig,
    MarketMakingOpportunity,
    MultiOutcomeArbitrage,
    MultiOutcomeMonitor,
    NearSettlementAnalysis,
    Outcome,
    OutcomeDirection,
    OutcomeSummary,
    SplitMergeOpportunity,
    SplitMergeType,
    POLYMARKET_FEE_RATE,
};
pub use pm_5m_directional::Pm5mDirectionalStrategy;
pub use position_manager::{
    Position as PersistedPosition, PositionManager, PositionStatus as PersistedPositionStatus,
    PositionSummary,
};
pub use probability::{estimate_probability, full_estimate, Features, ProbabilityEstimate};
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
pub use risk_mgmt::validation::{
    leg1_entry_chain, leg2_entry_chain, ExposureValidator, RiskStateValidator, SpreadValidator,
    SumTargetValidator, TimeRemainingValidator, ValidationChain, ValidationContext,
    ValidationError, Validator,
};
pub use signal::SignalDetector;
pub use split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition, PositionStatus,
    SplitArbConfig, SplitArbEngine,
};
pub use sports_facade::{
    EntryConfig, EntryDecision, EntryLogic, EntrySignal, ExitDecision, ExitLogic, ExitUrgency,
    FilterConfig, FilterResult, GameFeatures, LiveWinProbModel, MarketContext, MarketFilters,
    ModelMetadata, NbaCollectorConfig, NbaDataCollector, NbaExitConfig, NbaGameState,
    NbaMarketSnapshot, NbaStateEvent, NbaStateMachine, NbaStrategyState, OrderbookData,
    PartialSignal, PositionState, SportsLeague, SportsMarketDiscovery, TeamStats,
    WinProbCoefficients, WinProbPrediction,
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

// Backward-compat module aliases for slippage/validation
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
pub use sports::{SportsLeague as SportsSplitLeague, SportsMarketDiscovery as SportsSplitDiscovery};
