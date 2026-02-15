//! TradingAgent trait — pull-based agent interface
//!
//! Unlike the existing `DomainAgent` (push-based, router calls `on_event()`),
//! `TradingAgent` is pull-based: the agent owns its main loop via `run()`.
//! This gives each agent full control over its data sources and concurrency.

use async_trait::async_trait;

use crate::error::Result;
use crate::platform::{AgentRiskParams, Domain};

use super::context::AgentContext;

/// Risk parameters specific to a trading agent instance
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub agent_id: String,
    pub name: String,
    pub domain: Domain,
    pub risk_params: AgentRiskParams,
    pub dry_run: bool,
}

/// Pull-based trading agent trait.
///
/// Each agent owns its main loop and data sources. The coordinator
/// communicates with agents via `AgentContext` (orders out, commands in).
///
/// `run()` consumes `self` — an agent is a one-shot task spawned as a tokio task.
#[async_trait]
pub trait TradingAgent: Send + Sync + 'static {
    /// Unique identifier for this agent instance
    fn id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Trading domain this agent operates in
    fn domain(&self) -> Domain;

    /// Risk parameters for this agent
    fn risk_params(&self) -> AgentRiskParams;

    /// Main agent loop. Owns data feeds, generates orders via ctx.submit_order().
    /// Should handle CoordinatorCommands (Pause/Resume/Shutdown) from ctx.
    /// Returns when the agent is done (shutdown or fatal error).
    async fn run(self, ctx: AgentContext) -> Result<()>;
}
