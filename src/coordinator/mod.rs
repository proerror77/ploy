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
pub mod state;
pub mod strategy_runtime;

pub use bootstrap::{start_platform, PlatformBootstrapConfig, PlatformStartControl};
pub use command::{
    AgentHealthResponse, AllocatorLedgerSnapshot, CoordinatorCommand, CoordinatorControlCommand,
    DomainIngressSnapshot, GovernanceAgentSnapshot, GovernancePolicyHistoryEntry,
    GovernancePolicySnapshot, GovernancePolicyUpdate, GovernanceStatusSnapshot,
};
pub use config::CoordinatorConfig;
pub use coordinator::{Coordinator, CoordinatorHandle};
pub use state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};
