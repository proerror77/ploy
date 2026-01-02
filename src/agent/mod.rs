//! Claude Agent integration for intelligent trading assistance
//!
//! This module provides integration with Claude AI for:
//! - Market analysis and advisory
//! - Trading decision support
//! - Strategy optimization
//! - Autonomous trading operations

pub mod advisor;
pub mod autonomous;
pub mod client;
pub mod protocol;

pub use advisor::AdvisoryAgent;
pub use autonomous::{AutonomousAgent, AutonomousConfig};
pub use client::{ClaudeAgentClient, AgentClientConfig};
pub use protocol::{
    AgentAction, AgentContext, AgentResponse, MarketSnapshot,
    PositionInfo, RiskAssessment, TradeRecord,
};
