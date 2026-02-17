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

pub use comeback_stats::{ComebackStatsProvider, TeamComebackProfile};
pub use core::{ComebackOpportunity, NbaComebackCore, NbaComebackState};
pub use espn::{EspnClient, GameStatus, LiveGame, QuarterScore};
pub use grok_decision::{GrokDecision, UnifiedDecisionRequest};
pub use grok_intel::{GrokGameIntel, GrokSignalEvaluator, GrokTradeSignal};
