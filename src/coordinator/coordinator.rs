//! Coordinator — central orchestrator for multi-agent trading
//!
//! The Coordinator owns the order queue, risk gate, and position aggregator.
//! Agents communicate with it via `CoordinatorHandle` (clone-friendly).
//! The main `run()` loop uses `tokio::select!` to:
//!   - Process incoming order intents (risk check → enqueue)
//!   - Process agent state updates (heartbeats)
//!   - Periodically drain the queue and execute orders
//!   - Periodically refresh GlobalState from aggregators

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use sqlx::PgPool;

use crate::agent_runtime::AgentRiskParams;
use crate::control_plane::StrategyDeployment;
use crate::error::Result;
use crate::platform::{
    Domain, OrderIntent, OrderQueue, PositionAggregator, RiskCheckResult, RiskGate,
};
use crate::strategy::executor::OrderExecutor;

use super::admission::{
    buy_intent_missing_deployment_reason, sell_reduce_only_violation_reason, AdmissionController,
};
use super::capital::CapitalPolicy;
use super::command::{CoordinatorCommand, CoordinatorControlCommand};
use super::config::CoordinatorConfig;
use super::governance::{
    governance_block_reason, load_governance_policy, GovernanceController, IngressMode,
};
use super::journal::ExecutionJournal;
use super::state::{AgentSnapshot, GlobalState};

mod execution;
mod runtime_status;

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

impl CoordinatorHandle {
    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
        if !self.allowed_domains.contains(&intent.domain) {
            return Err(crate::error::PloyError::Validation(format!(
                "domain {} is not enabled for this runtime",
                intent.domain
            )));
        }
        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            return Err(crate::error::PloyError::Validation(reason));
        }

        if !intent.is_buy {
            let tracked_open_shares = self
                .positions
                .agent_open_shares_for_token_side(
                    &intent.agent_id,
                    intent.domain,
                    &intent.token_id,
                    intent.side,
                )
                .await;
            let pending_sell_shares = self.order_queue.read().await.pending_sell_shares_for(
                &intent.agent_id,
                intent.domain,
                &intent.token_id,
                intent.side,
            );
            if let Some(reason) =
                sell_reduce_only_violation_reason(&intent, tracked_open_shares, pending_sell_shares)
            {
                return Err(crate::error::PloyError::Validation(reason));
            }
        }

        // Binary-options semantics (Polymarket): SELL intents are treated as
        // reduce-only exits and must remain allowed during pause/halt.
        if intent.is_buy {
            let (global_mode, domain_mode) = self.governance.ingress_modes(intent.domain).await;

            if global_mode != IngressMode::Running {
                return Err(crate::error::PloyError::Validation(format!(
                    "coordinator global ingress is {:?}; new intents are blocked",
                    global_mode
                )));
            }

            if domain_mode != IngressMode::Running {
                return Err(crate::error::PloyError::Validation(format!(
                    "coordinator {:?} ingress is {:?}; new intents are blocked",
                    intent.domain, domain_mode
                )));
            }
        }
        self.order_tx.send(intent).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator order channel closed".into())
        })
    }

    /// Report agent state (heartbeat + position/PnL snapshot)
    pub async fn update_agent_state(&self, snapshot: AgentSnapshot) -> Result<()> {
        self.state_tx.send(snapshot).await.map_err(|_| {
            crate::error::PloyError::Internal("coordinator state channel closed".into())
        })
    }

    /// Pause all agents
    pub async fn pause_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Paused).await;
        self.control_tx
            .send(CoordinatorControlCommand::PauseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume all agents
    pub async fn resume_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Running).await;
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close all positions and stop agents
    pub async fn force_close_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownAll)
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a specific domain
    pub async fn pause_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Paused)
            .await;
        self.control_tx
            .send(CoordinatorControlCommand::PauseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a specific domain
    pub async fn resume_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Running)
            .await;
        self.control_tx
            .send(CoordinatorControlCommand::ResumeDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Force-close positions for one domain
    pub async fn force_close_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shutdown one domain
    pub async fn shutdown_domain(&self, domain: Domain) -> Result<()> {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownDomain(domain))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Pause a single agent by ID (used by OpenClaw meta-agent)
    pub async fn pause_agent(&self, agent_id: &str) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::PauseAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Resume a single agent by ID (used by OpenClaw meta-agent)
    pub async fn resume_agent(&self, agent_id: &str) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAgent(agent_id.to_string()))
            .await
            .map_err(|_| {
                crate::error::PloyError::Internal("coordinator control channel closed".into())
            })
    }

    /// Shared deployment registry (single source of truth for API + coordinator).
    pub fn shared_deployments(&self) -> Arc<RwLock<HashMap<String, StrategyDeployment>>> {
        self.admission.shared_deployments()
    }

    /// Runtime-enabled domains for this coordinator process.
    pub fn allowed_domains(&self) -> Arc<HashSet<Domain>> {
        self.allowed_domains.clone()
    }

    /// Whether a domain is enabled in this runtime.
    pub fn is_domain_allowed(&self, domain: Domain) -> bool {
        self.allowed_domains.contains(&domain)
    }

    /// Whether an agent_id is registered/authorized for order ingress.
    pub fn is_agent_authorized(&self, agent_id: &str) -> bool {
        self.authorized_agents
            .read()
            .map(|agents| agents.contains(agent_id))
            .unwrap_or(false)
    }
}

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
            order_tx,
            order_rx,
            state_tx,
            state_rx,
            control_tx,
            control_rx,
            agent_commands: HashMap::new(),
        }
    }

    /// Enable DB logging for order execution outcomes (including dry-run).
    pub fn set_execution_log_pool(&mut self, pool: PgPool) {
        self.journal.set_pool(pool);
    }

    /// Restore persisted risk runtime state (drawdown + daily pnl continuity).
    pub async fn restore_risk_runtime_state(&self) -> Result<()> {
        let Some(snapshot) = self.journal.load_risk_runtime_state().await? else {
            return Ok(());
        };

        self.risk_gate
            .restore_drawdown_snapshot(snapshot.drawdown)
            .await;

        if snapshot.daily_date == Some(Utc::now().date_naive()) {
            self.risk_gate
                .restore_daily_pnl_for_today(snapshot.daily_pnl)
                .await;
        }

        if snapshot.risk_state_raw.eq_ignore_ascii_case("halted") {
            self.risk_gate
                .trigger_circuit_breaker("restored persisted halted risk state")
                .await;
        }

        info!(
            account_id = %self.account_id,
            daily_pnl = %snapshot.daily_pnl,
            risk_state = %snapshot.risk_state_raw,
            "restored persisted risk runtime state"
        );
        Ok(())
    }

    /// Enable DB persistence for coordinator governance policy.
    pub fn set_governance_store_pool(&mut self, pool: PgPool) {
        self.governance_store_pool = Some(pool);
    }

    /// Restore runtime governance policy from DB (if a persisted row exists).
    pub async fn load_persisted_governance_policy(&self) -> Result<()> {
        let Some(pool) = self.governance_store_pool.as_ref() else {
            return Ok(());
        };

        let Some(policy) = load_governance_policy(pool, &self.account_id).await? else {
            return Ok(());
        };

        let snapshot = self.governance.replace_policy(policy).await;
        info!(
            account_id = %self.account_id,
            updated_by = %snapshot.updated_by,
            updated_at = %snapshot.updated_at,
            "restored governance policy from DB"
        );
        Ok(())
    }

    /// Rebuild runtime position/allocator state from persisted execution fills.
    ///
    /// This prevents cold-start underestimation of account exposure when a process restarts.
    pub async fn restore_runtime_state_from_execution_log(&self) -> Result<()> {
        let today = Utc::now().date_naive();
        let window_start = DateTime::<Utc>::from_naive_utc_and_offset(
            today
                .and_hms_opt(0, 0, 0)
                .expect("00:00:00 is always a valid UTC time"),
            Utc,
        );
        let window_end = window_start + ChronoDuration::days(1);
        let dry_run = self.executor.is_dry_run();

        let Some(restore_data) = self
            .journal
            .load_execution_restore_data(dry_run, window_start, window_end)
            .await?
        else {
            return Ok(());
        };
        let fills = restore_data.fills;
        let outcomes_today = restore_data.outcomes_today;

        if fills.is_empty() && outcomes_today.is_empty() {
            return Ok(());
        }

        self.positions.clear().await;
        self.capital_policy.reset_runtime_state().await;

        let restored_fill_count = fills.len();
        let mut restored_agents = HashSet::new();
        let mut daily_total_pnl = Decimal::ZERO;
        let mut daily_domain_pnl: HashMap<Domain, Decimal> = HashMap::new();
        let mut daily_agent_pnl: HashMap<String, Decimal> = HashMap::new();

        for fill in fills {
            let mut intent = OrderIntent::new(
                fill.agent_id.clone(),
                fill.domain,
                fill.market_slug.clone(),
                fill.token_id.clone(),
                fill.side,
                fill.is_buy,
                fill.filled_shares,
                fill.fill_price,
            );
            intent.intent_id = fill.intent_id;
            intent.created_at = fill.executed_at;
            intent.metadata = fill.metadata;

            self.settle_domain_success(&intent, fill.filled_shares, fill.fill_price)
                .await;

            if fill.is_buy {
                let position_id = self
                    .positions
                    .open_position(
                        &fill.agent_id,
                        fill.domain,
                        &fill.market_slug,
                        &fill.token_id,
                        fill.side,
                        fill.filled_shares,
                        fill.fill_price,
                    )
                    .await;
                debug!(
                    agent_id = %fill.agent_id,
                    intent_id = %fill.intent_id,
                    %position_id,
                    shares = fill.filled_shares,
                    fill_price = %fill.fill_price,
                    "restored tracked BUY position"
                );
            } else {
                let realized_pnl = self
                    .apply_sell_fill_to_positions(&intent, fill.filled_shares, fill.fill_price)
                    .await;
                if fill.executed_at >= window_start && fill.executed_at < window_end {
                    daily_total_pnl += realized_pnl;
                    *daily_domain_pnl.entry(fill.domain).or_insert(Decimal::ZERO) += realized_pnl;
                    *daily_agent_pnl
                        .entry(fill.agent_id.clone())
                        .or_insert(Decimal::ZERO) += realized_pnl;
                }
            }
            restored_agents.insert(fill.agent_id);
        }

        let mut daily_order_count: u32 = 0;
        let mut daily_success_count: u32 = 0;
        let mut daily_failure_count: u32 = 0;
        let mut global_consecutive_failures: u32 = 0;
        let mut per_agent_consecutive_failures: HashMap<String, u32> = HashMap::new();
        let mut last_risk_event_at: Option<DateTime<Utc>> = None;

        for outcome in outcomes_today {
            daily_order_count = daily_order_count.saturating_add(1);
            last_risk_event_at = Some(outcome.executed_at);
            if outcome.is_failure {
                daily_failure_count = daily_failure_count.saturating_add(1);
                global_consecutive_failures = global_consecutive_failures.saturating_add(1);
                let entry = per_agent_consecutive_failures
                    .entry(outcome.agent_id)
                    .or_insert(0);
                *entry = entry.saturating_add(1);
            } else {
                daily_success_count = daily_success_count.saturating_add(1);
                global_consecutive_failures = 0;
                per_agent_consecutive_failures.insert(outcome.agent_id, 0);
            }
        }

        self.risk_gate
            .restore_runtime_counters(
                today,
                daily_total_pnl,
                daily_domain_pnl,
                daily_order_count,
                daily_success_count,
                daily_failure_count,
                global_consecutive_failures,
                daily_agent_pnl,
                per_agent_consecutive_failures,
                last_risk_event_at,
            )
            .await;

        for agent_id in &restored_agents {
            self.refresh_risk_exposure_for_agent(agent_id).await;
        }
        self.refresh_global_state().await;

        info!(
            account_id = %self.account_id,
            fill_count = restored_fill_count,
            restored_agents = restored_agents.len(),
            daily_order_count,
            daily_success_count,
            daily_failure_count,
            global_consecutive_failures,
            "restored coordinator runtime state from execution log"
        );
        Ok(())
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

    pub async fn authorize_external_agent(&self, agent_id: &str, params: AgentRiskParams) {
        let id = agent_id.trim();
        if id.is_empty() {
            return;
        }
        if let Ok(mut authorized) = self.authorized_agents.write() {
            authorized.insert(id.to_string());
        }
        self.risk_gate.register_agent(id, params).await;
        info!(agent_id = %id, "external ingress agent authorized");
    }

    /// Send a command to a specific agent
    pub async fn send_command(&self, agent_id: &str, cmd: CoordinatorCommand) -> Result<()> {
        if let Some(tx) = self.agent_commands.get(agent_id) {
            tx.tx.send(cmd).await.map_err(|_| {
                crate::error::PloyError::Internal(format!(
                    "agent {} command channel closed",
                    agent_id
                ))
            })
        } else {
            Err(crate::error::PloyError::Internal(format!(
                "agent {} not registered",
                agent_id
            )))
        }
    }

    fn should_apply_domain_cmd(&self, entry: &AgentCommandChannel, target: Domain) -> bool {
        entry.domain == target
    }

    fn is_domain_allowed(&self, domain: Domain) -> bool {
        self.allowed_domains.contains(&domain)
    }

    async fn cancel_queued_buy_intents(&self, domain: Option<Domain>, reason: &str) {
        let dropped = {
            let mut queue = self.order_queue.write().await;
            queue.remove_buy_orders(domain)
        };

        if dropped.is_empty() {
            return;
        }

        for intent in dropped {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.to_string()), None)
                .await;
            self.settle_domain_failure(&intent).await;
        }
    }

    /// Pause all agents
    pub async fn pause_all(&self) {
        self.governance.set_global_mode(IngressMode::Paused).await;
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                warn!(agent_id = %id, error = %e, "failed to send pause");
            }
        }
    }

    /// Resume all agents
    pub async fn resume_all(&self) {
        self.governance.set_global_mode(IngressMode::Running).await;
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                warn!(agent_id = %id, error = %e, "failed to send resume");
            }
        }
    }

    /// Force-close all agents (best-effort)
    pub async fn force_close_all(&self) {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.cancel_queued_buy_intents(None, "dropped by coordinator global halt")
            .await;
        info!("coordinator: sending force-close to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                warn!(agent_id = %id, error = %e, "failed to send force-close");
            }
        }
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown(&self) {
        self.governance.set_global_mode(IngressMode::Halted).await;
        self.cancel_queued_buy_intents(None, "dropped by coordinator shutdown")
            .await;
        info!("coordinator: sending shutdown to all agents");
        for (id, entry) in &self.agent_commands {
            if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                warn!(agent_id = %id, error = %e, "failed to send shutdown");
            }
        }
    }

    /// Pause one domain
    pub async fn pause_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Paused)
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Pause).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain pause");
                }
            }
        }
    }

    /// Resume one domain
    pub async fn resume_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Running)
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Resume).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain resume");
                }
            }
        }
    }

    /// Force-close all agents in one domain
    pub async fn force_close_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.cancel_queued_buy_intents(Some(domain), "dropped by coordinator domain halt")
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::ForceClose).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain force-close");
                }
            }
        }
    }

    /// Shutdown all agents in one domain
    pub async fn shutdown_domain(&self, domain: Domain) {
        self.governance
            .set_domain_mode(domain, IngressMode::Halted)
            .await;
        self.cancel_queued_buy_intents(Some(domain), "dropped by coordinator domain shutdown")
            .await;
        for (id, entry) in &self.agent_commands {
            if self.should_apply_domain_cmd(entry, domain) {
                if let Err(e) = entry.tx.send(CoordinatorCommand::Shutdown).await {
                    warn!(agent_id = %id, error = %e, "failed to send domain shutdown");
                }
            }
        }
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
                    match cmd {
                        CoordinatorControlCommand::PauseAll => self.pause_all().await,
                        CoordinatorControlCommand::ResumeAll => self.resume_all().await,
                        CoordinatorControlCommand::ForceCloseAll => self.force_close_all().await,
                        CoordinatorControlCommand::ShutdownAll => self.shutdown().await,
                        CoordinatorControlCommand::PauseDomain(domain) => self.pause_domain(domain).await,
                        CoordinatorControlCommand::ResumeDomain(domain) => {
                            self.resume_domain(domain).await
                        }
                        CoordinatorControlCommand::ForceCloseDomain(domain) => {
                            self.force_close_domain(domain).await
                        }
                        CoordinatorControlCommand::ShutdownDomain(domain) => {
                            self.shutdown_domain(domain).await
                        }
                        CoordinatorControlCommand::PauseAgent(id) => {
                            self.governance.pause_agent(&id).await;
                            self.send_command(&id, CoordinatorCommand::Pause).await.ok();
                        }
                        CoordinatorControlCommand::ResumeAgent(id) => {
                            self.governance.resume_agent(&id).await;
                            self.send_command(&id, CoordinatorCommand::Resume).await.ok();
                        }
                    }
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

    /// Risk-check an incoming order intent and enqueue if passed
    async fn handle_order_intent(&self, intent: OrderIntent) {
        let mut intent = intent;
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;
        let strategy_max_shares = intent.shares;

        if !self.is_domain_allowed(intent.domain) {
            let reason = format!("domain {} is not enabled for this runtime", intent.domain);
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by runtime domain allowlist"
            );
            return;
        }

        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked due to missing deployment identity"
            );
            return;
        }

        if !intent.is_buy {
            let tracked_open_shares = self
                .positions
                .agent_open_shares_for_token_side(
                    &intent.agent_id,
                    intent.domain,
                    &intent.token_id,
                    intent.side,
                )
                .await;
            let pending_sell_shares = self.order_queue.read().await.pending_sell_shares_for(
                &intent.agent_id,
                intent.domain,
                &intent.token_id,
                intent.side,
            );

            if let Some(reason) =
                sell_reduce_only_violation_reason(&intent, tracked_open_shares, pending_sell_shares)
            {
                self.journal
                    .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id, reason = %reason,
                    "order blocked by reduce-only sell guard"
                );
                return;
            }
        }
        let (ingress_mode, domain_mode) = self.governance.ingress_modes(intent.domain).await;
        if intent.is_buy && ingress_mode != IngressMode::Running {
            let reason = format!(
                "Coordinator ingress is {:?}; blocking BUY intent while paused/halted",
                ingress_mode
            );
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator ingress state"
            );
            return;
        }
        if intent.is_buy {
            if domain_mode != IngressMode::Running {
                let reason = format!(
                    "Domain {:?} ingress is {:?}; blocking BUY intent while paused/halted",
                    intent.domain, domain_mode
                );
                self.journal
                    .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id, reason = %reason,
                    "order blocked by coordinator domain ingress state"
                );
                return;
            }
        }
        // Per-agent pause check
        if intent.is_buy && self.governance.is_agent_paused(&intent.agent_id).await {
            let reason = format!("Agent {} is paused; blocking BUY intent", intent.agent_id);
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by per-agent pause"
            );
            return;
        }

        if let Some(reason) = self.check_governance_policy(&intent).await {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by global governance policy"
            );
            return;
        }

        if let Err(reason) = self
            .admission
            .enforce_live_buy_deployment_gate(
                self.account_id.as_str(),
                self.executor.is_dry_run(),
                &self.allowed_domains,
                &mut intent,
            )
            .await
        {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by deployment gate"
            );
            return;
        }

        self.journal.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.journal.persist_exit_reason_intent(&intent).await;
        }

        if let Some(reason) = self.admission.check_duplicate_intent(&intent).await {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by duplicate-intent guard"
            );
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_kelly_sizing(&self.capital_policy, &mut intent)
            .await
        {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by kelly sizing policy"
            );
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_min_order_constraints(&mut intent, strategy_max_shares)
        {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by venue minimum constraints"
            );
            return;
        }

        let mut adjusted: Option<(u64, String)> = None;
        let mut evaluated = intent;
        for attempt in 0..3 {
            match self.risk_gate.check_order(&evaluated).await {
                RiskCheckResult::Passed => {
                    if let Some(reason) = self.reserve_domain_capital(&evaluated).await {
                        self.journal
                            .persist_risk_decision(
                                &evaluated,
                                "BLOCKED",
                                Some(reason.clone()),
                                adjusted.clone(),
                            )
                            .await;
                        warn!(
                            %agent_id, %intent_id, reason = %reason,
                            "order blocked by domain allocator"
                        );
                        return;
                    }

                    self.journal
                        .persist_risk_decision(&evaluated, "PASSED", None, adjusted.clone())
                        .await;
                    let mut queue = self.order_queue.write().await;
                    match queue.enqueue(evaluated) {
                        Ok(()) => {
                            debug!(
                                %agent_id, %intent_id,
                                "order enqueued"
                            );
                        }
                        Err(e) => {
                            self.release_domain_reservation(intent_id).await;
                            warn!(%agent_id, %intent_id, error = %e, "queue full, order dropped");
                        }
                    }
                    return;
                }
                RiskCheckResult::Blocked(reason) => {
                    self.journal
                        .persist_risk_decision(
                            &evaluated,
                            "BLOCKED",
                            Some(reason.to_string()),
                            adjusted.clone(),
                        )
                        .await;
                    warn!(
                        %agent_id, %intent_id,
                        reason = ?reason,
                        "order blocked by risk gate"
                    );
                    return;
                }
                RiskCheckResult::Adjusted(suggestion) => {
                    // Apply adjustment locally and re-evaluate; agents do not automatically resubmit.
                    if suggestion.max_shares == 0 {
                        let reason =
                            format!("risk-gate suggested max_shares=0: {}", suggestion.reason);
                        self.journal
                            .persist_risk_decision(
                                &evaluated,
                                "BLOCKED",
                                Some(reason.clone()),
                                adjusted.clone(),
                            )
                            .await;
                        warn!(%agent_id, %intent_id, reason = %reason, "order blocked after risk adjustment");
                        return;
                    }

                    adjusted = Some((suggestion.max_shares, suggestion.reason.clone()));
                    evaluated.shares = suggestion.max_shares;
                    info!(
                        %agent_id, %intent_id,
                        attempt,
                        max_shares = suggestion.max_shares,
                        reason = %suggestion.reason,
                        "order adjusted by risk gate; re-evaluating"
                    );
                }
            }
        }

        let reason = "risk-gate adjustment loop exceeded max attempts".to_string();
        self.journal
            .persist_risk_decision(&evaluated, "BLOCKED", Some(reason.clone()), adjusted)
            .await;
        warn!(%agent_id, %intent_id, reason = %reason, "order blocked");
    }

    async fn check_governance_policy(&self, intent: &OrderIntent) -> Option<String> {
        let policy = self.governance.current_policy().await;
        let current_notional = self.current_account_notional().await;
        governance_block_reason(&policy, intent, current_notional)
    }

    async fn current_account_notional(&self) -> Decimal {
        let platform_exposure = self.risk_gate.total_exposure().await;
        let (allocator_open, allocator_pending) = self.capital_policy.allocator_totals().await;
        let other_pending_buy_notional = self
            .order_queue
            .read()
            .await
            .pending_buy_notional_excluding_domains(&[
                Domain::Crypto,
                Domain::Sports,
                Domain::Politics,
                Domain::Economics,
            ]);

        let open_notional = platform_exposure.max(allocator_open);
        open_notional + allocator_pending + other_pending_buy_notional
    }

    async fn reserve_domain_capital(&self, intent: &OrderIntent) -> Option<String> {
        self.capital_policy.reserve_buy(intent).await
    }

    async fn release_domain_reservation(&self, intent_id: Uuid) {
        self.capital_policy.release_buy_reservation(intent_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::PolymarketClient;
    use crate::agent_runtime::AgentStatus;
    use crate::config::ExecutionConfig;
    use crate::coordinator::QueueStatsSnapshot;
    use crate::platform::{Domain, OrderPriority, QueueStats};
    use crate::strategy::executor::OrderExecutor;
    use rust_decimal_macros::dec;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn mock_snapshot(agent_id: &str) -> AgentSnapshot {
        AgentSnapshot {
            agent_id: agent_id.into(),
            name: agent_id.into(),
            domain: Domain::Crypto,
            status: AgentStatus::Running,
            position_count: 1,
            exposure: dec!(100),
            daily_pnl: dec!(5),
            unrealized_pnl: dec!(2),
            metrics: HashMap::new(),
            last_heartbeat: Utc::now(),
            error_message: None,
        }
    }

    fn make_test_handle() -> (CoordinatorHandle, Coordinator) {
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
        let handle = coordinator.handle();
        (handle, coordinator)
    }

    #[test]
    fn test_global_state_defaults() {
        let state = GlobalState::new();
        assert_eq!(state.active_agent_count(), 0);
        assert_eq!(state.total_exposure(), Decimal::ZERO);
        assert_eq!(state.total_unrealized_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_global_state_active_count() {
        let mut state = GlobalState::new();
        state.agents.insert("a".into(), mock_snapshot("a"));
        state.agents.insert("b".into(), {
            let mut s = mock_snapshot("b");
            s.status = AgentStatus::Paused;
            s
        });
        assert_eq!(state.active_agent_count(), 1);
    }

    #[test]
    fn test_queue_stats_snapshot_from() {
        let qs = QueueStats {
            current_size: 5,
            max_size: 100,
            enqueued_total: 50,
            dequeued_total: 45,
            expired_total: 3,
            critical_count: 1,
            high_count: 2,
            normal_count: 1,
            low_count: 1,
        };
        let snap = QueueStatsSnapshot::from(qs);
        assert_eq!(snap.current_size, 5);
        assert_eq!(snap.enqueued_total, 50);
    }

    fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto_lob_ml",
            Domain::Crypto,
            "btc-updown-5m-123",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            100,
            dec!(0.42),
        );
        intent.priority = priority;
        intent
    }

    #[test]
    fn test_buy_intent_requires_deployment_id_metadata() {
        let intent = make_intent(true, OrderPriority::Normal);
        let reason = buy_intent_missing_deployment_reason(&intent);
        assert_eq!(
            reason.as_deref(),
            Some("BUY intent missing required metadata field 'deployment_id'")
        );
    }

    #[test]
    fn test_sell_intent_does_not_require_deployment_id_metadata() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(buy_intent_missing_deployment_reason(&intent).is_none());
    }

    #[test]
    fn test_sell_reduce_only_violation_when_no_tracked_shares() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 0, 0);
        assert!(reason
            .unwrap_or_default()
            .contains("no tracked open shares"));
    }

    #[test]
    fn test_sell_reduce_only_violation_when_requested_exceeds_tracked() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 30, 0);
        assert!(reason
            .unwrap_or_default()
            .contains("requested shares 100 exceeds available reduce-only shares 30"));
    }

    #[test]
    fn test_sell_reduce_only_allows_with_sufficient_tracked_shares() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(sell_reduce_only_violation_reason(&intent, 100, 0).is_none());
        assert!(sell_reduce_only_violation_reason(&intent, 150, 0).is_none());
    }

    #[test]
    fn test_sell_reduce_only_violation_when_pending_sells_exhaust_available() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 100, 100);
        assert!(reason
            .unwrap_or_default()
            .contains("fully reserved by pending SELL intents 100"));
    }

    #[test]
    fn test_sell_reduce_only_violation_when_requested_exceeds_available_after_pending() {
        let intent = make_intent(false, OrderPriority::Normal);
        let reason = sell_reduce_only_violation_reason(&intent, 100, 40);
        assert!(reason
            .unwrap_or_default()
            .contains("requested shares 100 exceeds available reduce-only shares 60"));
    }

    #[tokio::test]
    async fn test_drain_and_execute_records_single_success_for_buy_fill() {
        let (_handle, coordinator) = make_test_handle();
        coordinator
            .risk_gate
            .register_agent_with_domain("crypto_lob_ml", Domain::Crypto, AgentRiskParams::default())
            .await;

        let intent =
            make_intent(true, OrderPriority::Normal).with_metadata("deployment_id", "deploy.test");

        coordinator.handle_order_intent(intent).await;
        coordinator.drain_and_execute().await;

        let (total_pnl, success_count, failure_count) = coordinator.risk_gate.daily_stats().await;
        assert_eq!(total_pnl, Decimal::ZERO);
        assert_eq!(success_count, 1);
        assert_eq!(failure_count, 0);
    }

    #[tokio::test]
    async fn test_drain_and_execute_sell_fill_reduces_position_and_realizes_pnl() {
        let (_handle, coordinator) = make_test_handle();
        coordinator
            .risk_gate
            .register_agent_with_domain("crypto_lob_ml", Domain::Crypto, AgentRiskParams::default())
            .await;

        let buy_intent =
            make_intent(true, OrderPriority::Normal).with_metadata("deployment_id", "deploy.test");
        coordinator.handle_order_intent(buy_intent).await;
        coordinator.drain_and_execute().await;

        let mut sell_intent = make_intent(false, OrderPriority::Normal);
        sell_intent.shares = 60;
        sell_intent.limit_price = dec!(0.60);

        coordinator.handle_order_intent(sell_intent).await;
        coordinator.drain_and_execute().await;

        let remaining_shares = coordinator
            .positions
            .agent_open_shares_for_token_side(
                "crypto_lob_ml",
                Domain::Crypto,
                "token-up-123",
                crate::domain::Side::Up,
            )
            .await;
        assert_eq!(remaining_shares, 40);
        assert_eq!(coordinator.positions.total_realized_pnl().await, dec!(10.8));

        let (total_pnl, success_count, failure_count) = coordinator.risk_gate.daily_stats().await;
        assert_eq!(total_pnl, dec!(10.8));
        assert_eq!(success_count, 2);
        assert_eq!(failure_count, 0);
    }

    #[tokio::test]
    async fn test_handle_force_close_domain_blocks_new_buy_immediately() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .force_close_domain(Domain::Sports)
            .await
            .expect("force-close domain command accepted");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        )
        .with_deployment_id("deploy.sports.nba.test");

        let err = handle
            .submit_order(intent)
            .await
            .expect_err("buy intent should be blocked once domain is force-closed");
        assert!(err.to_string().contains("new intents are blocked"));
    }

    #[tokio::test]
    async fn test_handle_shutdown_domain_blocks_new_buy_immediately() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .shutdown_domain(Domain::Sports)
            .await
            .expect("shutdown domain command accepted");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-2",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.40),
        )
        .with_deployment_id("deploy.sports.nba.test");

        let err = handle
            .submit_order(intent)
            .await
            .expect_err("buy intent should be blocked once domain is shut down");
        assert!(err.to_string().contains("new intents are blocked"));
    }

    #[tokio::test]
    async fn test_governance_status_includes_domain_ingress_and_agents() {
        let (handle, _coordinator) = make_test_handle();
        handle
            .pause_domain(Domain::Sports)
            .await
            .expect("pause domain command accepted");
        {
            let mut state = handle.global_state.write().await;
            state.agents.insert(
                "sports_agent".to_string(),
                AgentSnapshot {
                    agent_id: "sports_agent".to_string(),
                    name: "sports_agent".to_string(),
                    domain: Domain::Sports,
                    status: AgentStatus::Running,
                    position_count: 0,
                    exposure: dec!(12.5),
                    daily_pnl: dec!(1.2),
                    unrealized_pnl: dec!(0.3),
                    metrics: HashMap::new(),
                    last_heartbeat: Utc::now(),
                    error_message: None,
                },
            );
        }

        let snapshot = handle.governance_status().await;

        assert!(snapshot
            .domain_ingress_modes
            .iter()
            .any(|row| row.domain == "sports" && row.mode == "paused"));
        assert!(snapshot
            .agents
            .iter()
            .any(|agent| agent.agent_id == "sports_agent"
                && agent.domain == "sports"
                && agent.status == "running"));
    }
}
