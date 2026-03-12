//! Governance agent traits.
//!
//! The pull-based `TradingAgent` compatibility runtime has been retired.
//! Remaining agent-level extensions are governance-only and must not receive
//! direct order ingress access.

use async_trait::async_trait;

use crate::error::Result;
use crate::platform::Domain;

use super::governance_context::GovernanceContext;

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
