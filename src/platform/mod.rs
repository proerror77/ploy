//! Order Platform - 統一下單平台
//!
//! 提供領域無關的訂單執行、風控和倉位管理。
//! 所有策略 Agent 透過這個平台提交訂單。

mod traits;
mod types;
mod queue;
mod risk;
mod position;
mod platform;
mod router;
pub mod agents;

pub use traits::{DomainAgent, AgentStatus, AgentRiskParams, AgentHealthStatus, SimpleAgent};
pub use types::{
    Domain, DomainEvent, OrderIntent, OrderPriority,
    ExecutionReport, ExecutionStatus,
    SportsEvent, CryptoEvent, PoliticsEvent,
    QuoteData, QuoteUpdateEvent, OrderUpdateEvent,
};
pub use queue::{OrderQueue, QueueStats};
pub use risk::{RiskGate, RiskCheckResult, RiskConfig, BlockReason, PlatformRiskState};
pub use position::{PositionAggregator, AggregatedPosition, Position, AgentPositionStats};
pub use platform::{OrderPlatform, PlatformConfig, PlatformStats};
pub use router::{EventRouter, AgentSubscription, RouterStats};

// RL-powered agents (requires 'rl' feature)
#[cfg(feature = "rl")]
pub use agents::{RLCryptoAgent, RLCryptoAgentConfig};
