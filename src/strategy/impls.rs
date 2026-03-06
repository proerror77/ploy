//! Strategy implementation exports grouped by behavior.
//!
//! Use this module instead of pulling strategy implementation types from the
//! `strategy` root module.

pub use super::core::{
    ArbSide as CoreArbSide, ArbStats as CoreArbStats, BinaryMarket,
    HedgedPosition as CoreHedgedPosition, MarketDiscovery, MarketType,
    PartialPosition as CorePartialPosition, PositionStatus as CorePositionStatus, PriceCache,
    SplitArbConfig as CoreSplitArbConfig, SplitArbEngine as CoreSplitArbEngine,
};
pub use super::crypto::{run_crypto_split_arb, CryptoMarketDiscovery, CryptoSplitArbConfig};
pub use super::sports::{
    run_sports_split_arb, SportsLeague, SportsMarketDiscovery, SportsSplitArbConfig,
};
#[cfg(feature = "claimer_daemon")]
pub use crate::account::claimer::{AutoClaimer, ClaimResult, ClaimerConfig, RedeemablePosition};

pub use super::deribit_probability_arb::{
    binary_call_prob_forward, interpolate_iv_linear, net_edge, norm_cdf, parse_polymarket_question,
    run_deribit_probability_arb, DeribitProbabilityArbConfig, ParsedPolymarketQuestion,
    SurfacePoint, VolSurfaceSnapshot,
};
pub use super::directional_backtest::{
    DirectionalBacktestConfig, DirectionalBacktestEngine, DirectionalClosedTrade,
};
pub use super::event_edge::core::{EventEdgeCore, EventEdgeState, TradeDecision};
pub use super::event_edge::{run_event_edge, EventEdgeConfig};
pub use super::fee_model::{AllInCost, FeeModel, FeeRateCache};
pub use super::momentum::{
    Direction, EventInfo, EventMatcher, ExitConfig, ExitManager, ExitReason, MomentumConfig,
    MomentumDetector, MomentumEngine, MomentumSignal, Position,
};
pub use super::multi_event::{ArbitrageOpportunity, EventSummary, EventTracker, MultiEventMonitor};
pub use super::multi_outcome::{
    analyze_market_making_opportunity, analyze_near_settlement, detect_split_merge_opportunity,
    fetch_multi_outcome_event, generate_ev_table, ArbitrageType, ExpectedValue, MarketMakingAction,
    MarketMakingConfig, MarketMakingOpportunity, MultiOutcomeArbitrage, MultiOutcomeMonitor,
    NearSettlementAnalysis, Outcome, OutcomeDirection, OutcomeSummary, SplitMergeOpportunity,
    SplitMergeType, POLYMARKET_FEE_RATE,
};
pub use super::nba_comeback::nba_data_collector::{
    CollectorConfig as NbaCollectorConfig, DataCollector as NbaDataCollector,
    GameState as NbaGameState, MarketSnapshot as NbaMarketSnapshot, OrderbookData, TeamStats,
};
pub use super::nba_comeback::nba_entry::{
    EntryConfig, EntryDecision, EntryLogic, EntrySignal, PartialSignal,
};
pub use super::nba_comeback::nba_exit::{
    ExitConfig as NbaExitConfig, ExitDecision, ExitLogic, ExitUrgency, PositionState,
};
pub use super::nba_comeback::nba_filters::{
    FilterConfig, FilterResult, MarketContext, MarketFilters,
};
pub use super::nba_comeback::nba_state_machine::{
    StateEvent as NbaStateEvent, StateMachine as NbaStateMachine, StrategyState as NbaStrategyState,
};
pub use super::nba_comeback::nba_winprob::{
    GameFeatures, LiveWinProbModel, ModelMetadata, WinProbCoefficients, WinProbPrediction,
};
pub use super::paper_runner::{
    run_paper_trading, PaperTradingConfig, PaperTradingRunner, TrackedMarket,
};
pub use super::position_manager::{
    Position as PersistedPosition, PositionManager, PositionStatus as PersistedPositionStatus,
    PositionSummary,
};
pub use super::probability::{estimate_probability, full_estimate, Features, ProbabilityEstimate};
pub use super::reconciliation::{
    DiscrepancySeverity, PositionDiscrepancy, ReconciliationConfig, ReconciliationResult,
    ReconciliationService,
};
pub use super::reverse_engineered::{
    extract_profile_snapshot, infer_strategy_params, run_reverse_engineered_profile_paper,
    ProfileSnapshot as ReverseProfileSnapshot, ReverseDryRunResult, ReverseEngineeredConfig,
    ReverseTradeEvent, StrategyParams as ReverseStrategyParams, REVERSE_PROFILE_STRATEGY_NAME,
    REVERSE_PROFILE_STRATEGY_SLUG,
};
pub use super::split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition, PositionStatus,
    SplitArbConfig, SplitArbEngine,
};
pub use super::staggered_arb_backtest::{
    StaggeredArbBacktestConfig, StaggeredArbBacktestEngine, StaggeredArbClosedTrade,
};
pub use super::staggered_arb_live::StaggeredArbAdapter;
pub use super::trade_logger::{
    BucketStats, SymbolStats, TradeContext, TradeLogger, TradeOutcome, TradeRecord, TradingStats,
};
pub use super::trading_costs::{
    OrderType, TradingCostBreakdown, TradingCostCalculator, TradingCostConfig,
};
pub use super::volatility::{
    ActiveEvent, EventRecord, EventTracker as VolatilityEventTracker, VolatilityConfig,
    VolatilityDetector, VolatilitySignal,
};
pub use super::volatility_arb::{
    calculate_fair_yes_price, calculate_implied_volatility, calculate_kelly_fraction,
    MarketPricing, VolArbSignal, VolArbStats, VolArbTrade, VolatilityArbConfig,
    VolatilityArbEngine, VolatilityEstimate,
};
