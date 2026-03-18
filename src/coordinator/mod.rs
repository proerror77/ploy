//! Multi-Agent Coordinator
//!
//! Central orchestrator that manages trading agents across domains.
//! Provides a single order submission chokepoint with risk checks,
//! cross-agent position awareness, and dynamic pause/resume control.

mod admission;
pub mod bootstrap;
mod capital;
pub mod command;
pub mod config;
pub mod coordinator;
mod governance;
mod journal;
mod order_intent;
mod position;
mod queue;
mod risk;
pub mod state;
pub mod strategy_runtime;
pub mod subscription_planner;

pub use bootstrap::{start_platform, PlatformBootstrapConfig, PlatformStartControl};
pub use command::{
    AgentHealthResponse, AllocatorLedgerSnapshot, CoordinatorCommand, CoordinatorControlCommand,
    DomainIngressSnapshot, GovernanceAgentSnapshot, GovernancePolicyHistoryEntry,
    GovernancePolicySnapshot, GovernancePolicyUpdate, GovernanceStatusSnapshot,
};
pub use config::CoordinatorConfig;
pub use coordinator::{Coordinator, CoordinatorHandle};
pub use order_intent::{OrderIntent, OrderPriority};
pub use position::{AggregatedPosition, Position, PositionAggregator};
pub use queue::{OrderQueue, QueueStats};
pub use risk::{
    BlockReason, CircuitBreakerEvent, DrawdownSnapshot, PlatformRiskState, RiskCheckResult,
    RiskConfig, RiskGate,
};
pub use state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};
