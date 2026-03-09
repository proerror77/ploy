//! Agent compatibility surfaces that remain after the legacy trading runtime
//! was retired.
//!
//! The canonical trading path now runs through `Strategy` runtimes. This module
//! keeps:
//! - governance-plane traits/context for OpenClaw
//! - no trading runtime surfaces
pub mod governance_agent;
pub mod governance_context;
pub mod openclaw;

pub use governance_agent::GovernanceAgent;
pub use governance_context::GovernanceContext;
pub use openclaw::OpenClawAgent;
