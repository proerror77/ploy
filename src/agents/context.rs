//! AgentContext — the agent's interface to the coordinator
//!
//! Wraps a `CoordinatorHandle` + command receiver, providing a clean API
//! for agents to submit orders, report state, and receive commands.

use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::coordinator::{AgentSnapshot, CoordinatorCommand, CoordinatorHandle, GlobalState};
use crate::error::Result;
use crate::platform::{AgentStatus, Domain, OrderIntent};

/// Context given to each agent when spawned — not Clone (owns command receiver)
pub struct AgentContext {
    pub agent_id: String,
    pub domain: Domain,
    handle: CoordinatorHandle,
    commands: mpsc::Receiver<CoordinatorCommand>,
}

impl AgentContext {
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

    /// Submit an order intent to the coordinator
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        self.handle.submit_order(intent).await
    }

    /// Report agent state to the coordinator (call periodically as heartbeat)
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

    /// Read the current global state (snapshot of all agents + portfolio)
    pub async fn read_global_state(&self) -> GlobalState {
        self.handle.read_state().await
    }

    /// Non-blocking check for incoming commands (Pause/Resume/Shutdown/etc.)
    pub fn try_recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.try_recv().ok()
    }

    /// Async wait for the next command (use in select! branches)
    pub async fn recv_command(&mut self) -> Option<CoordinatorCommand> {
        self.commands.recv().await
    }

    /// Mutable access to the command receiver (for use in select! macros)
    pub fn command_rx(&mut self) -> &mut mpsc::Receiver<CoordinatorCommand> {
        &mut self.commands
    }
}
