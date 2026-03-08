//! AI client integrations for intelligent trading assistance.
//!
//! LLM API wrappers and data providers:
//! - Claude AI: advisory, autonomous trading, strategy optimization
//! - Grok: real-time search, sentiment analysis
//! - Sports data: odds providers, game analytics
//!
//! NOTE: This module was renamed from `agent/` to clarify that these are
//! API clients, not autonomous runtime agents (see `agents/` for those).

pub mod advisor;
pub mod autonomous;
pub mod client;
pub mod grok;
pub mod odds_provider;
pub mod polymarket_sports;
pub mod protocol;
pub mod sports_analyst;
pub mod sports_data;

pub use advisor::AdvisoryAgent;
pub use autonomous::{AutonomousAgent, AutonomousConfig};
pub use client::{AgentClientConfig, ClaudeAgentClient};
pub use grok::{GrokClient, GrokConfig};
pub use polymarket_sports::{
    EventDetails, LiveGameMarket, PolymarketSportsClient, NBA_SERIES_ID,
};
pub use sports_analyst::SportsAnalyst;
