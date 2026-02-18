//! Multi-Agent Coordinator
//!
//! Central orchestrator that manages trading agents across domains.
//! Provides a single order submission chokepoint with risk checks,
//! cross-agent position awareness, and dynamic pause/resume control.

pub mod bootstrap;
pub mod command;
pub mod config;
pub mod coordinator;
pub mod state;

pub use bootstrap::{start_platform, PlatformBootstrapConfig, PlatformStartControl};
pub use command::{AgentHealthResponse, CoordinatorCommand, CoordinatorControlCommand};
pub use config::CoordinatorConfig;
pub use coordinator::{Coordinator, CoordinatorHandle};
pub use state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};
