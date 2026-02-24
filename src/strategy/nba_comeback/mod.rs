//! NBA Q3→Q4 Comeback Trading Strategy
//!
//! Scans live NBA games via ESPN, identifies teams trailing in Q3 with
//! high historical comeback rates, and buys YES shares on Polymarket
//! when the market underprices their win probability.

pub mod comeback_stats;
pub mod core;
pub mod espn;
pub mod grok_decision;
pub mod grok_intel;

// Infrastructure modules (moved from strategy/ root)
pub mod nba_data_collector;
pub mod nba_entry;
pub mod nba_exit;
pub mod nba_filters;
pub mod nba_state_machine;
pub mod nba_winprob;

pub use comeback_stats::{ComebackStatsProvider, TeamComebackProfile};
pub use core::{
    ComebackOpportunity, GamePosition, NbaComebackCore, NbaComebackState, PositionEntry,
};
pub use espn::{EspnClient, GameStatus, LiveGame, QuarterScore};
pub use grok_decision::{GrokDecision, RiskMetrics, UnifiedDecisionRequest};
pub use grok_intel::{GrokGameIntel, GrokSignalEvaluator, GrokTradeSignal};
