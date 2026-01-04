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
pub mod engine;
pub mod executor;
pub mod momentum;
pub mod multi_event;
pub mod multi_outcome;
pub mod risk;
pub mod signal;
pub mod split_arb;
pub mod validation;

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
