//! OpenClaw meta-agent — Layer 3 orchestrator for capital allocation,
//! regime detection, conflict resolution, and temporal straddle coordination.
//!
//! OpenClaw implements `TradingAgent` and plugs into the existing coordinator
//! bootstrap. It never trades directly — only observes and controls via
//! the `CoordinatorHandle` API.

pub mod agent;
pub mod allocator;
pub mod config;
pub mod conflict;
pub mod performance;
pub mod regime;
pub mod straddle;

pub use agent::OpenClawAgent;
pub use config::OpenClawConfig;
pub use regime::{MarketRegime, RegimeSnapshot};
