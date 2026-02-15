//! Claude Agent integration for intelligent trading assistance
//!
//! This module provides integration with Claude AI for:
//! - Market analysis and advisory
//! - Trading decision support
//! - Strategy optimization
//! - Autonomous trading operations
//! - Sports event analysis with Grok + Claude
//!
//! Also includes Grok API integration for real-time search.

pub mod advisor;
pub mod autonomous;
pub mod client;
pub mod grok;
pub mod odds_provider;
pub mod polymarket_politics;
pub mod polymarket_sports;
pub mod protocol;
pub mod sports_analyst;
pub mod sports_data;

pub use advisor::AdvisoryAgent;
pub use autonomous::{AutonomousAgent, AutonomousConfig};
pub use client::{AgentClientConfig, ClaudeAgentClient};
pub use grok::{GrokClient, GrokConfig, SearchResult, Sentiment};
pub use odds_provider::{EdgeAnalysis, GameEvent, Market, OddsProvider, OddsProviderConfig, Sport};
pub use polymarket_politics::{
    PoliticalCategory, PoliticalEventDetails, PoliticsEdgeAnalysis, PoliticsMarketDetails,
    PolymarketPoliticsClient, PolymarketPoliticsMarket, POLITICS_KEYWORDS,
};
pub use polymarket_sports::{
    EventDetails, LiveGameEvent, LiveGameMarket, PolymarketEdgeAnalysis, PolymarketSportsClient,
    PolymarketSportsMarket, SportsMarketDetails, NBA_SERIES_ID,
};
pub use protocol::{
    AgentAction, AgentContext, AgentResponse, MarketSnapshot, PositionInfo, RiskAssessment,
    TradeRecord,
};
pub use sports_analyst::{SportsAnalysis, SportsAnalysisWithDK, SportsAnalyst};
pub use sports_data::{SportsDataFetcher, StructuredGameData};
