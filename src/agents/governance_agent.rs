use async_trait::async_trait;

use crate::error::Result;

use super::governance_context::GovernanceContext;

/// Governance-plane agent trait.
///
/// Governance agents can observe runtime state and issue coordinator control /
/// policy updates, but they do not receive order-submission capability.
#[async_trait]
pub trait GovernanceAgent: Send + Sync + 'static {
    /// Unique identifier for this governance agent instance.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Main governance loop. May update policy, pause/resume agents, and
    /// report heartbeats, but cannot submit orders directly.
    async fn run(self, ctx: GovernanceContext) -> Result<()>;
}
