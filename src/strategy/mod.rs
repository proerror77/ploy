//! Strategy module
//!
//! Contains trading strategies and supporting infrastructure.

// =============================================================================
// Core modules
// =============================================================================

pub mod engine;
pub mod executor;
pub mod momentum;
pub mod multi_event;
pub mod multi_outcome;
pub mod risk;
pub mod signal;
pub mod split_arb;

// Legacy re-exports
pub use engine::StrategyEngine;
pub use executor::OrderExecutor;
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
pub use split_arb::{
    run_split_arb, ArbSide, ArbStats, HedgedPosition, PartialPosition,
    PositionStatus, SplitArbConfig, SplitArbEngine,
};
