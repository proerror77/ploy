//! GovernanceContext — governance-only access to the coordinator
//!
//! Policy agents can observe runtime state, receive coordinator commands,
//! and project pause/resume or governance updates. They do not receive
//! direct order-ingress access.

use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::coordinator::{
    AgentSnapshot, CoordinatorCommand, CoordinatorHandle, GlobalState, GovernancePolicySnapshot,
    GovernancePolicyUpdate,
};
use crate::error::Result;
use crate::agent_runtime::AgentStatus;
use crate::platform::Domain;

/// Narrowed coordinator access for governance-only agents such as OpenClaw.
pub struct GovernanceContext {
    agent_id: String,
    domain: Domain,
    handle: CoordinatorHandle,
    commands: mpsc::Receiver<CoordinatorCommand>,
}

impl GovernanceContext {
    pub fn new(
        agent_id: String,
        domain: Domain,
        handle: CoordinatorHandle,
        commands: mpsc::Receiver<CoordinatorCommand>,
    ) -> Self {
        Self {
            agent_id,
            domain,
            handle,
            commands,
        }
    }
}

impl GovernanceContext {
    /// Report agent state to the coordinator (call periodically as heartbeat).
    pub async fn report_state(
        &self,
        name: &str,
        status: AgentStatus,
        position_count: usize,
        exposure: Decimal,
        daily_pnl: Decimal,
        unrealized_pnl: Decimal,
        error_message: Option<String>,
    ) -> Result<()> {
        self.report_state_with_metrics(
            name,
            status,
            position_count,
            exposure,
            daily_pnl,
            unrealized_pnl,
            HashMap::new(),
            error_message,
        )
        .await
    }

    /// Report agent state with strategy-specific metrics.
    pub async fn report_state_with_metrics(
        &self,
        name: &str,
        status: AgentStatus,
        position_count: usize,
        exposure: Decimal,
        daily_pnl: Decimal,
        unrealized_pnl: Decimal,
        metrics: HashMap<String, String>,
        error_message: Option<String>,
    ) -> Result<()> {
        let snapshot = AgentSnapshot {
            agent_id: self.agent_id.clone(),
            name: name.into(),
            domain: self.domain.clone(),
            status,
            position_count,
            exposure,
            daily_pnl,
            unrealized_pnl,
            metrics,
            last_heartbeat: Utc::now(),
            error_message,
        };
        self.handle.update_agent_state(snapshot).await
    }

    /// Read the current global state (snapshot of all agents + portfolio).
    pub async fn read_global_state(&self) -> GlobalState {
        self.handle.read_state().await
    }

    /// Non-blocking check for incoming commands.
    pub fn try_recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.try_recv().ok()
    }

    /// Async wait for the next command (use in select! branches).
    pub async fn recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.recv().await
    }

    /// Mutable access to the command receiver (for use in select! macros).
    pub fn command_rx(&mut self) -> &mut mpsc::Receiver<CoordinatorCommand> {
        &mut self.commands
    }

    /// Pause a single agent by ID.
    pub async fn submit_pause_agent(&self, agent_id: &str) -> Result<()> {
        self.handle.pause_agent(agent_id).await
    }

    /// Resume a single agent by ID.
    pub async fn submit_resume_agent(&self, agent_id: &str) -> Result<()> {
        self.handle.resume_agent(agent_id).await
    }

    /// Read the current governance policy snapshot.
    pub async fn read_governance_policy(&self) -> GovernancePolicySnapshot {
        self.handle.governance_policy().await
    }

    /// Replace the governance policy snapshot.
    pub async fn update_governance_policy(
        &self,
        update: GovernancePolicyUpdate,
    ) -> Result<GovernancePolicySnapshot> {
        self.handle.update_governance_policy(update).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::PolymarketClient;
    use crate::config::ExecutionConfig;
    use crate::coordinator::{Coordinator, CoordinatorConfig};
    use crate::strategy::execution::OrderExecutor;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn make_governance_context() -> (
        GovernanceContext,
        mpsc::Sender<CoordinatorCommand>,
        Coordinator,
    ) {
        let client = PolymarketClient::new("https://clob.polymarket.com", true)
            .expect("build dry-run polymarket client");
        let executor = Arc::new(OrderExecutor::new(client, ExecutionConfig::default()));
        let allowed_domains = HashSet::from([Domain::Crypto, Domain::Sports]);
        let coordinator = Coordinator::new(
            CoordinatorConfig::default(),
            executor,
            "acct-test".to_string(),
            allowed_domains,
        );
        let (commands_tx, commands_rx) = mpsc::channel(8);
        let ctx = GovernanceContext::new(
            "openclaw".to_string(),
            Domain::Custom(0),
            coordinator.handle(),
            commands_rx,
        );

        (ctx, commands_tx, coordinator)
    }

    #[tokio::test]
    async fn governance_context_reads_state_and_updates_policy() {
        let (ctx, _commands_tx, _coordinator) = make_governance_context();

        let state = ctx.read_global_state().await;
        assert_eq!(state.active_agent_count(), 0);

        let snapshot = ctx
            .update_governance_policy(GovernancePolicyUpdate {
                block_new_intents: true,
                blocked_domains: vec!["sports".to_string()],
                max_intent_notional_usd: None,
                max_total_notional_usd: None,
                updated_by: "openclaw".to_string(),
                reason: Some("risk_off".to_string()),
                metadata: HashMap::from([("regime".to_string(), "risk_off".to_string())]),
            })
            .await
            .expect("update governance policy");

        assert!(snapshot.block_new_intents);
        assert_eq!(snapshot.updated_by, "openclaw");
        assert_eq!(
            snapshot.metadata.get("regime").map(String::as_str),
            Some("risk_off")
        );

        let readback = ctx.read_governance_policy().await;
        assert!(readback.block_new_intents);
        assert_eq!(readback.blocked_domains, vec!["sports".to_string()]);
        assert_eq!(
            readback.metadata.get("regime").map(String::as_str),
            Some("risk_off")
        );
    }

    #[tokio::test]
    async fn governance_context_receives_commands() {
        let (mut ctx, commands_tx, _coordinator) = make_governance_context();

        commands_tx
            .send(CoordinatorCommand::Pause)
            .await
            .expect("send coordinator command");

        assert!(matches!(
            ctx.recv_command().await,
            Some(CoordinatorCommand::Pause)
        ));
    }

    #[tokio::test]
    async fn governance_context_can_submit_pause_and_resume_commands() {
        let (ctx, _commands_tx, _coordinator) = make_governance_context();

        ctx.submit_pause_agent("momentum")
            .await
            .expect("pause agent through governance context");
        ctx.submit_resume_agent("momentum")
            .await
            .expect("resume agent through governance context");
    }
}
