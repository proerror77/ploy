//! Order Platform - 統一下單平台
//!
//! 提供領域無關的訂單執行、風控和倉位管理。
//! 所有策略 Agent 透過這個平台提交訂單。

pub mod agents;
mod contracts;
mod platform;
mod position;
mod queue;
mod risk;
mod router;
mod traits;
mod types;

pub use contracts::{
    DeploymentExecutionMode, MarketSelector, OrderCommand, OrderExecutionReport, RiskDecision,
    RiskDecisionStatus, StrategyDeployment, Timeframe, TradeIntent,
};
pub use platform::{OrderPlatform, PlatformConfig, PlatformStats};
pub use position::{AgentPositionStats, AggregatedPosition, Position, PositionAggregator};
pub use queue::{OrderQueue, QueueStats};
pub use risk::{
    BlockReason, CircuitBreakerEvent, DrawdownSnapshot, PlatformRiskState, RiskCheckResult,
    RiskConfig, RiskGate,
};
pub use router::{AgentSubscription, EventRouter, RouterStats};
pub use traits::{AgentHealthStatus, AgentRiskParams, AgentStatus, DomainAgent, SimpleAgent};
pub use types::{
    CryptoEvent, Domain, DomainEvent, ExecutionReport, ExecutionStatus, OrderIntent, OrderPriority,
    OrderUpdateEvent, PoliticsEvent, QuoteData, QuoteUpdateEvent, SportsEvent,
};

pub use agents::EventEdgePlatformAgent;
pub use agents::NbaComebackAgent;

// RL-powered agents (requires 'rl' feature)
#[cfg(feature = "rl")]
pub use agents::{RLCryptoAgent, RLCryptoAgentConfig};
