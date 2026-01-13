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

pub mod traits;
pub mod manager;
pub mod adapters;
pub mod feeds;

pub use traits::{
    Strategy, DataFeed, MarketUpdate, OrderUpdate, StrategyAction,
    StrategyStateInfo, PositionInfo, RiskLevel, AlertLevel,
    StrategyEvent, StrategyEventType, StrategyConfig,
};

pub use manager::{StrategyManager, StrategyStatus, StrategyFactory, StrategyInfo};
pub use adapters::{MomentumStrategyAdapter, SplitArbStrategyAdapter};
pub use feeds::{DataFeedManager, DataFeedBuilder};

// =============================================================================
// New modular architecture
// =============================================================================

pub mod core;
pub mod crypto;
pub mod sports;

// =============================================================================
// Legacy modules (to be phased out)
// =============================================================================

pub mod calculations;
pub mod claimer;
pub mod engine;
pub mod executor;
pub mod fund_manager;
pub mod idempotency;
pub mod momentum;
pub mod multi_event;
pub mod multi_outcome;
pub mod risk;
pub mod signal;
pub mod split_arb;
pub mod trade_logger;
pub mod validation;
pub mod volatility;
pub mod dump_hedge;
pub mod volatility_arb;
pub mod backtest;
pub mod paper_runner;
pub mod position_manager;
pub mod reconciliation;
pub mod trading_costs;
pub mod slippage;
pub mod execution_sim;
pub mod nba_winprob;
pub mod nba_filters;
pub mod nba_entry;
pub mod nba_exit;
pub mod nba_state_machine;
pub mod nba_data_collector;

// Legacy re-exports
pub use engine::StrategyEngine;
pub use executor::OrderExecutor;
pub use idempotency::{IdempotencyManager, IdempotencyResult};
pub use claimer::{AutoClaimer, ClaimerConfig, ClaimResult, RedeemablePosition};
pub use fund_manager::{FundManager, FundStatus, PositionSizeResult};
pub use trade_logger::{
    TradeLogger, TradeRecord, TradeOutcome, TradingStats, SymbolStats,
    TradeContext, BucketStats,
};
pub use multi_event::{ArbitrageOpportunity, EventSummary, EventTracker, MultiEventMonitor};
pub use multi_outcome::{
    // Core types
    fetch_multi_outcome_event, ArbitrageType, MultiOutcomeArbitrage, MultiOutcomeMonitor,
    Outcome, OutcomeDirection, OutcomeSummary,
    // EV analysis
    ExpectedValue, POLYMARKET_FEE_RATE,
    // Split/Merge arbitrage
    SplitMergeOpportunity, SplitMergeType, detect_split_merge_opportunity,
    // Near-settlement analysis
    NearSettlementAnalysis, RiskLevel as LegacyRiskLevel, analyze_near_settlement,
    // Market making
    MarketMakingConfig, MarketMakingOpportunity, MarketMakingAction,
    analyze_market_making_opportunity, generate_ev_table,
};
pub use momentum::{
    Direction, EventInfo, EventMatcher, ExitConfig, ExitManager, ExitReason,
    MomentumConfig, MomentumDetector as LegacyMomentumDetector, MomentumEngine,
    MomentumSignal as LegacyMomentumSignal, Position,
};
pub use risk::RiskManager;
pub use signal::SignalDetector;
pub use volatility::{
    VolatilityConfig, VolatilityDetector, VolatilitySignal,
    EventTracker as VolatilityEventTracker, ActiveEvent, EventRecord,
};
pub use dump_hedge::{
    DumpHedgeConfig, DumpHedgeEngine, DumpHedgeStats,
    EnhancedDumpSignal, ProgressiveHedgeSignal, HedgeResult,
    PendingHedge, StopLossSignal, StopLossReason,
};
pub use volatility_arb::{
    VolatilityArbConfig, VolatilityArbEngine, VolArbSignal,
    VolArbStats, VolArbTrade, VolatilityEstimate, MarketPricing,
    calculate_fair_yes_price, calculate_implied_volatility, calculate_kelly_fraction,
};
pub use backtest::{
    BacktestEngine, BacktestResults, BacktestTrade,
    PaperTrader, PaperSignal, PaperTradingStats,
    KlineRecord, PMPriceRecord, MarketSnapshot,
    load_klines_from_csv, load_pm_prices_from_csv, calculate_kline_volatility,
};
pub use paper_runner::{
    PaperTradingConfig, PaperTradingRunner, TrackedMarket,
    run_paper_trading,
};
pub use position_manager::{
    PositionManager, Position as PersistedPosition, PositionStatus as PersistedPositionStatus, PositionSummary,
};
pub use reconciliation::{
    ReconciliationService, ReconciliationConfig, ReconciliationResult,
    PositionDiscrepancy, DiscrepancySeverity,
};
pub use trading_costs::{
    TradingCostCalculator, TradingCostConfig, TradingCostBreakdown, OrderType,
};
pub use slippage::{
    SlippageProtection, SlippageConfig, SlippageCheck, MarketDepth,
};
pub use execution_sim::{
    ExecutionSimulator, ExecutionSimConfig, ExecutionResult,
};
pub use nba_winprob::{
    LiveWinProbModel, WinProbCoefficients, ModelMetadata,
    GameFeatures, WinProbPrediction,
};
pub use nba_filters::{
    MarketFilters, FilterConfig, MarketContext, FilterResult,
};
pub use nba_entry::{
    EntryLogic, EntryConfig, EntrySignal, EntryDecision, PartialSignal,
};
pub use nba_exit::{
    ExitLogic, ExitDecision, ExitUrgency, PositionState,
    ExitConfig as NbaExitConfig,
};
pub use nba_state_machine::{
    StateMachine as NbaStateMachine, StrategyState as NbaStrategyState, StateEvent as NbaStateEvent,
};
pub use nba_data_collector::{
    DataCollector as NbaDataCollector, CollectorConfig as NbaCollectorConfig,
    MarketSnapshot as NbaMarketSnapshot, OrderbookData, GameState as NbaGameState, TeamStats,
};
pub use split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition,
    PositionStatus, SplitArbConfig, SplitArbEngine,
};

// New consolidated modules
pub use calculations::{
    TradingCalculator,
    POLYMARKET_FEE_RATE as CALC_FEE_RATE,
    DEFAULT_SLIPPAGE, MIN_PROFIT_TARGET,
    effective_sum_target, check_leg2_condition, calculate_cycle_pnl,
};
pub use validation::{
    ValidationChain, ValidationContext, ValidationError, Validator,
    TimeRemainingValidator, ExposureValidator, SpreadValidator,
    RiskStateValidator, SumTargetValidator,
    leg1_entry_chain, leg2_entry_chain,
};

// =============================================================================
// New architecture re-exports
// =============================================================================

// Core types
pub use core::{
    BinaryMarket, MarketDiscovery, MarketType,
    PriceCache,
    ArbSide as CoreArbSide, ArbStats as CoreArbStats,
    HedgedPosition as CoreHedgedPosition, PartialPosition as CorePartialPosition,
    PositionStatus as CorePositionStatus,
    SplitArbConfig as CoreSplitArbConfig, SplitArbEngine as CoreSplitArbEngine,
};

// Crypto strategies
pub use crypto::{
    CryptoMarketDiscovery, CryptoSplitArbConfig,
    run_crypto_split_arb,
};

// Sports strategies
pub use sports::{
    SportsMarketDiscovery, SportsLeague, SportsSplitArbConfig,
    run_sports_split_arb,
};
