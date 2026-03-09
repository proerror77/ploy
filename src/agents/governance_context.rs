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

/// Governance-only context given to non-trading control-plane agents.
///
/// This intentionally excludes order-submission capability so governance agents
/// cannot enter the trading path directly.
pub struct GovernanceContext {
    pub agent_id: String,
    pub domain: Domain,
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
            domain: self.domain,
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

    pub async fn read_global_state(&self) -> GlobalState {
        self.handle.read_state().await
    }

    pub fn try_recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.try_recv().ok()
    }

    pub async fn recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.recv().await
    }

    pub fn command_rx(&mut self) -> &mut mpsc::Receiver<CoordinatorCommand> {
        &mut self.commands
    }

    pub async fn submit_pause_agent(&self, agent_id: &str) -> Result<()> {
        self.handle.pause_agent(agent_id).await
    }

    pub async fn submit_resume_agent(&self, agent_id: &str) -> Result<()> {
        self.handle.resume_agent(agent_id).await
    }

    pub async fn read_governance_policy(&self) -> GovernancePolicySnapshot {
        self.handle.governance_policy().await
    }

    pub async fn update_governance_policy(
        &self,
        update: GovernancePolicyUpdate,
    ) -> Result<GovernancePolicySnapshot> {
        self.handle.update_governance_policy(update).await
    }
}
