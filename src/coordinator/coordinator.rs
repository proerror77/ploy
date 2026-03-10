//! Coordinator — central orchestrator for multi-agent trading
//!
//! The Coordinator owns the order queue, risk gate, and position aggregator.
//! Agents communicate with it via `CoordinatorHandle` (clone-friendly).
//! The main `run()` loop uses `tokio::select!` to:
//!   - Process incoming order intents (risk check → enqueue)
//!   - Process agent state updates (heartbeats)
//!   - Periodically drain the queue and execute orders
//!   - Periodically refresh GlobalState from aggregators

use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use sqlx::PgPool;

use crate::agent_runtime::AgentRiskParams;
use crate::strategy::executor::OrderExecutor;

use super::admission::AdmissionController;
use super::capital::CapitalPolicy;
use super::command::{CoordinatorCommand, CoordinatorControlCommand};
use super::config::CoordinatorConfig;
use super::governance::{governance_block_reason, GovernanceController};
use super::journal::ExecutionJournal;
use super::position::PositionAggregator;
use super::queue::OrderQueue;
use super::risk::{RiskCheckResult, RiskGate};
use super::state::{AgentSnapshot, GlobalState};
use crate::coordinator::OrderIntent;
use crate::domain::Domain;

mod control_surface;
mod execution;
mod execution_settlement;
mod ingress;
mod ingress_preflight;
mod ingress_rejections;
mod order_updates;
mod recovery;
mod runtime_status;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
struct AgentCommandChannel {
    domain: Domain,
    tx: mpsc::Sender<CoordinatorCommand>,
}

/// Clonable handle given to agents for submitting orders and state updates
#[derive(Clone)]
pub struct CoordinatorHandle {
    account_id: String,
    order_tx: mpsc::Sender<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    global_state: Arc<RwLock<GlobalState>>,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    capital_policy: Arc<CapitalPolicy>,
    positions: Arc<PositionAggregator>,
    governance: Arc<GovernanceController>,
    admission: Arc<AdmissionController>,
    allowed_domains: Arc<HashSet<Domain>>,
    authorized_agents: Arc<std::sync::RwLock<HashSet<String>>>,
    governance_store_pool: Option<PgPool>,
}

impl CoordinatorHandle {}

/// The Coordinator — owns shared infrastructure and runs the main event loop
pub struct Coordinator {
    config: CoordinatorConfig,
    account_id: String,
    allowed_domains: Arc<HashSet<Domain>>,
    authorized_agents: Arc<std::sync::RwLock<HashSet<String>>>,
    admission: Arc<AdmissionController>,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    capital_policy: Arc<CapitalPolicy>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    global_state: Arc<RwLock<GlobalState>>,
    journal: ExecutionJournal,
    governance_store_pool: Option<PgPool>,
    governance: Arc<GovernanceController>,
    stale_heartbeat_warn_at: Arc<RwLock<HashMap<String, chrono::DateTime<Utc>>>>,
    order_update_sinks:
        Arc<std::sync::RwLock<HashMap<String, mpsc::Sender<crate::strategy::OrderUpdate>>>>,

    // Channels
    order_tx: mpsc::Sender<OrderIntent>,
    order_rx: mpsc::Receiver<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    state_rx: mpsc::Receiver<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    control_rx: mpsc::Receiver<CoordinatorControlCommand>,

    // Per-agent command channels
    agent_commands: HashMap<String, AgentCommandChannel>,
}

impl Coordinator {
    pub fn new(
        config: CoordinatorConfig,
        executor: Arc<OrderExecutor>,
        account_id: String,
        allowed_domains: HashSet<Domain>,
    ) -> Self {
        let (order_tx, order_rx) = mpsc::channel(256);
        let (state_tx, state_rx) = mpsc::channel(128);
        let (control_tx, control_rx) = mpsc::channel(32);

        let allowed_domains = Arc::new(allowed_domains);
        let authorized_agents = Arc::new(std::sync::RwLock::new(HashSet::new()));
        let risk_gate = Arc::new(RiskGate::new(config.risk.clone()));
        let order_queue = Arc::new(RwLock::new(OrderQueue::new(1024)));
        let admission = Arc::new(AdmissionController::new(&config));
        let capital_policy = Arc::new(CapitalPolicy::new(&config));
        let positions = Arc::new(PositionAggregator::new());
        let global_state = Arc::new(RwLock::new(GlobalState::new()));
        let governance = Arc::new(GovernanceController::new(&config));
        let stale_heartbeat_warn_at = Arc::new(RwLock::new(HashMap::new()));
        let order_update_sinks = Arc::new(std::sync::RwLock::new(HashMap::new()));
        let account_id = if account_id.trim().is_empty() {
            "default".to_string()
        } else {
            account_id
        };
        let journal = ExecutionJournal::new(account_id.clone());

        Self {
            config,
            account_id,
            allowed_domains,
            authorized_agents,
            admission,
            risk_gate,
            order_queue,
            capital_policy,
            positions,
            executor,
            global_state,
            journal,
            governance_store_pool: None,
            governance,
            stale_heartbeat_warn_at,
            order_update_sinks,
            order_tx,
            order_rx,
            state_tx,
            state_rx,
            control_tx,
            control_rx,
            agent_commands: HashMap::new(),
        }
    }

    /// Create a clonable handle for agents
    pub fn handle(&self) -> CoordinatorHandle {
        CoordinatorHandle {
            account_id: self.account_id.clone(),
            order_tx: self.order_tx.clone(),
            state_tx: self.state_tx.clone(),
            control_tx: self.control_tx.clone(),
            global_state: self.global_state.clone(),
            risk_gate: self.risk_gate.clone(),
            order_queue: self.order_queue.clone(),
            capital_policy: self.capital_policy.clone(),
            positions: self.positions.clone(),
            governance: self.governance.clone(),
            admission: self.admission.clone(),
            allowed_domains: self.allowed_domains.clone(),
            authorized_agents: self.authorized_agents.clone(),
            governance_store_pool: self.governance_store_pool.clone(),
        }
    }

    /// Shared global state reference (for TUI)
    pub fn global_state(&self) -> Arc<RwLock<GlobalState>> {
        self.global_state.clone()
    }

    /// Position aggregator reference
    pub fn positions(&self) -> Arc<PositionAggregator> {
        self.positions.clone()
    }

    /// Register an agent and return its command receiver
    pub fn register_agent(
        &mut self,
        agent_id: String,
        domain: Domain,
        risk_params: AgentRiskParams,
    ) -> mpsc::Receiver<CoordinatorCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        self.agent_commands
            .insert(agent_id.clone(), AgentCommandChannel { domain, tx: cmd_tx });
        if let Ok(mut authorized) = self.authorized_agents.write() {
            authorized.insert(agent_id.clone());
        }

        // Register with risk gate (fire-and-forget via spawn since we're not async here)
        let risk_gate = self.risk_gate.clone();
        let id = agent_id.clone();
        tokio::spawn(async move {
            risk_gate
                .register_agent_with_domain(&id, domain, risk_params)
                .await;
        });

        info!(agent_id, "agent registered with coordinator");
        cmd_rx
    }

    /// Main coordinator loop — blocks until shutdown
    pub async fn run(mut self, mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) {
        info!(
            agents = self.agent_commands.len(),
            "coordinator starting main loop"
        );

        let drain_interval = tokio::time::Duration::from_millis(self.config.queue_drain_ms);
        let refresh_interval = tokio::time::Duration::from_millis(self.config.state_refresh_ms);

        let mut drain_tick = tokio::time::interval(drain_interval);
        let mut refresh_tick = tokio::time::interval(refresh_interval);

        // Don't burst-fire missed ticks
        drain_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                // --- Control commands (pause/resume/force-close) ---
                Some(cmd) = self.control_rx.recv() => {
                    self.handle_control_command(cmd).await;
                }

                // --- Incoming order intents ---
                Some(intent) = self.order_rx.recv() => {
                    self.handle_order_intent(intent).await;
                }

                // --- Agent state updates (heartbeats) ---
                Some(snapshot) = self.state_rx.recv() => {
                    self.handle_state_update(snapshot).await;
                }

                // --- Periodic: drain queue and execute ---
                _ = drain_tick.tick() => {
                    self.drain_and_execute().await;
                }

                // --- Periodic: refresh global state ---
                _ = refresh_tick.tick() => {
                    self.refresh_global_state().await;
                }

                // --- Shutdown signal ---
                _ = shutdown_rx.recv() => {
                    info!("coordinator: shutdown signal received");
                    self.shutdown().await;
                    break;
                }
            }
        }

        info!("coordinator: main loop exited");
    }
}
