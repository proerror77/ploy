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
use super::governance::{governance_block_reason, GovernanceController, IngressMode};
use super::journal::ExecutionJournal;
use super::state::{AgentSnapshot, GlobalState};

mod control_surface;
mod execution;
mod ingress;
mod recovery;
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
