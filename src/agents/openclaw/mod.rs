//! OpenClaw meta-agent — Layer 3 orchestrator for capital allocation,
//! regime detection, conflict resolution, and temporal straddle coordination.
//!
//! OpenClaw implements `GovernanceAgent` and plugs into the coordinator
//! governance plane. It never trades directly and only observes or controls
//! strategy/runtime state via the `CoordinatorHandle` API.

pub mod agent;
pub mod allocator;
pub mod config;
pub mod conflict;
pub mod performance;
pub mod regime;
pub mod straddle;

pub use agent::OpenClawAgent;
