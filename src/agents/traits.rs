//! TradingAgent trait — pull-based agent interface
//!
//! Unlike the existing `DomainAgent` (push-based, router calls `on_event()`),
//! `TradingAgent` is pull-based: the agent owns its main loop via `run()`.
//! This gives each agent full control over its data sources and concurrency.
//!
//! Transitional status:
//! This is a compatibility surface during the layered live runtime migration.
//! New live strategies must not implement `TradingAgent`; they should implement
//! `crate::strategy::traits::Strategy` and run through the canonical strategy
//! runtime instead.

use async_trait::async_trait;

use crate::error::Result;
use crate::platform::{AgentRiskParams, Domain};

use super::context::AgentContext;
use super::governance_context::GovernanceContext;

/// Risk parameters specific to a trading agent instance
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub agent_id: String,
    pub name: String,
    pub domain: Domain,
    /// Deployment/bootstrap-projected risk binding for coordinator registration.
    pub risk_params: AgentRiskParams,
    pub dry_run: bool,
}

/// Pull-based trading agent trait.
///
/// Each agent owns its main loop and data sources. The coordinator
/// communicates with agents via `AgentContext` (orders out, commands in).
///
/// `run()` consumes `self` — an agent is a one-shot task spawned as a tokio task.
///
/// This trait is transitional and should only be used for compatibility while
/// existing pull-based runtimes are migrated or reduced to governance-only
/// roles.
#[async_trait]
pub trait TradingAgent: Send + Sync + 'static {
    /// Unique identifier for this agent instance
    fn id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Trading domain this agent operates in
    fn domain(&self) -> Domain;

    /// Main agent loop. Owns data feeds, generates orders via ctx.submit_order().
    /// Should handle CoordinatorCommands (Pause/Resume/Shutdown) from ctx.
    /// Returns when the agent is done (shutdown or fatal error).
    async fn run(self, ctx: AgentContext) -> Result<()>;
}

/// Governance-only agent trait.
///
/// Governance agents can observe coordinator state, receive commands, and
/// project pause/resume or policy updates, but do not receive direct order
/// ingress access.
#[async_trait]
pub trait GovernanceAgent: Send + Sync + 'static {
    /// Unique identifier for this governance agent instance.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Domain label used for coordinator registration and snapshots.
    fn domain(&self) -> Domain;

    /// Main governance loop. Owns policy logic and reacts to coordinator
    /// commands through a governance-only context.
    async fn run(self, ctx: GovernanceContext) -> Result<()>;
}
