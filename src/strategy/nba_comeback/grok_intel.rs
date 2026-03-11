//! Grok X.com Live Search Integration for NBA Games
//!
//! Queries Grok (xAI API with real-time X.com search) for live NBA game
//! intelligence: injury updates, momentum shifts, fan/analyst sentiment,
//! and independent win probability estimates.
//!
//! Produces `GrokGameIntel` structs that can be evaluated by
//! `GrokSignalEvaluator` to generate independent trading signals.

mod parsing;
mod query;
mod signal_eval;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use parsing::extract_json_block;
pub use parsing::parse_grok_response;
pub use query::{build_grok_game_prompt, query_grok_for_game};
pub use signal_eval::GrokSignalEvaluator;
pub use types::{
    GrokGameIntel, GrokSignalType, GrokTradeSignal, InjuryImpact, InjuryUpdate, MomentumDirection,
};
