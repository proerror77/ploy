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
pub mod protocol;
pub mod sports_analyst;
pub mod sports_data;

pub use advisor::AdvisoryAgent;
pub use autonomous::{AutonomousAgent, AutonomousConfig};
pub use client::{ClaudeAgentClient, AgentClientConfig};
pub use grok::{GrokClient, GrokConfig, SearchResult, Sentiment};
pub use protocol::{
    AgentAction, AgentContext, AgentResponse, MarketSnapshot,
    PositionInfo, RiskAssessment, TradeRecord,
};
pub use sports_analyst::{SportsAnalyst, SportsAnalysis};
pub use sports_data::{SportsDataFetcher, StructuredGameData};
