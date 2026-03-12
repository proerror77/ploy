//! OpenClaw meta-agent — Layer 3 orchestrator for capital allocation,
//! regime detection, conflict resolution, and temporal straddle coordination.
//!
//! OpenClaw now implements a governance-only agent contract.
//! It never trades directly — only observes and controls via governance APIs.

pub mod agent;
pub mod allocator;
pub mod conflict;
pub mod performance;
pub mod regime;
pub mod straddle;

pub use agent::OpenClawAgent;
