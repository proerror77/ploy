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
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use sqlx::PgPool;

use crate::domain::OrderRequest;
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, OrderIntent, OrderQueue, PositionAggregator, RiskCheckResult, RiskGate,
};
use crate::strategy::executor::OrderExecutor;

use super::command::{CoordinatorCommand, CoordinatorControlCommand};
use super::config::CoordinatorConfig;
use super::state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};

/// Clonable handle given to agents for submitting orders and state updates
#[derive(Clone)]
pub struct CoordinatorHandle {
    order_tx: mpsc::Sender<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    global_state: Arc<RwLock<GlobalState>>,
}

impl CoordinatorHandle {
    /// Submit an order intent to the coordinator for risk checking and execution
    pub async fn submit_order(&self, intent: OrderIntent) -> Result<()> {
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
        self.control_tx
            .send(CoordinatorControlCommand::PauseAll)
            .await
            .map_err(|_| crate::error::PloyError::Internal("coordinator control channel closed".into()))
    }

    /// Resume all agents
    pub async fn resume_all(&self) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ResumeAll)
            .await
            .map_err(|_| crate::error::PloyError::Internal("coordinator control channel closed".into()))
    }

    /// Force-close all positions and stop agents
    pub async fn force_close_all(&self) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ForceCloseAll)
            .await
            .map_err(|_| crate::error::PloyError::Internal("coordinator control channel closed".into()))
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown_all(&self) -> Result<()> {
        self.control_tx
            .send(CoordinatorControlCommand::ShutdownAll)
            .await
            .map_err(|_| crate::error::PloyError::Internal("coordinator control channel closed".into()))
    }

    /// Read the current global state (non-blocking snapshot)
    pub async fn read_state(&self) -> GlobalState {
        self.global_state.read().await.clone()
    }
}

/// The Coordinator — owns shared infrastructure and runs the main event loop
pub struct Coordinator {
    config: CoordinatorConfig,
    account_id: String,
    risk_gate: Arc<RiskGate>,
    order_queue: Arc<RwLock<OrderQueue>>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    global_state: Arc<RwLock<GlobalState>>,
    execution_log_pool: Option<PgPool>,

    // Channels
    order_tx: mpsc::Sender<OrderIntent>,
    order_rx: mpsc::Receiver<OrderIntent>,
    state_tx: mpsc::Sender<AgentSnapshot>,
    state_rx: mpsc::Receiver<AgentSnapshot>,
    control_tx: mpsc::Sender<CoordinatorControlCommand>,
    control_rx: mpsc::Receiver<CoordinatorControlCommand>,

    // Per-agent command channels
    agent_commands: HashMap<String, mpsc::Sender<CoordinatorCommand>>,
}

impl Coordinator {
    pub fn new(
        config: CoordinatorConfig,
        executor: Arc<OrderExecutor>,
        account_id: String,
    ) -> Self {
        let (order_tx, order_rx) = mpsc::channel(256);
        let (state_tx, state_rx) = mpsc::channel(128);
        let (control_tx, control_rx) = mpsc::channel(32);

        let risk_gate = Arc::new(RiskGate::new(config.risk.clone()));
        let order_queue = Arc::new(RwLock::new(OrderQueue::new(1024)));
        let positions = Arc::new(PositionAggregator::new());
        let global_state = Arc::new(RwLock::new(GlobalState::new()));
        let account_id = if account_id.trim().is_empty() {
            "default".to_string()
        } else {
            account_id
        };

        Self {
            config,
            account_id,
            risk_gate,
            order_queue,
            positions,
            executor,
            global_state,
            execution_log_pool: None,
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
        self.execution_log_pool = Some(pool);
    }

    /// Create a clonable handle for agents
    pub fn handle(&self) -> CoordinatorHandle {
        CoordinatorHandle {
            order_tx: self.order_tx.clone(),
            state_tx: self.state_tx.clone(),
            control_tx: self.control_tx.clone(),
            global_state: self.global_state.clone(),
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
        risk_params: AgentRiskParams,
    ) -> mpsc::Receiver<CoordinatorCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        self.agent_commands.insert(agent_id.clone(), cmd_tx);

        // Register with risk gate (fire-and-forget via spawn since we're not async here)
        let risk_gate = self.risk_gate.clone();
        let id = agent_id.clone();
        tokio::spawn(async move {
            risk_gate.register_agent(&id, risk_params).await;
        });

        info!(agent_id, "agent registered with coordinator");
        cmd_rx
    }

    /// Send a command to a specific agent
    pub async fn send_command(&self, agent_id: &str, cmd: CoordinatorCommand) -> Result<()> {
        if let Some(tx) = self.agent_commands.get(agent_id) {
            tx.send(cmd).await.map_err(|_| {
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

    /// Pause all agents
    pub async fn pause_all(&self) {
        for (id, tx) in &self.agent_commands {
            if let Err(e) = tx.send(CoordinatorCommand::Pause).await {
                warn!(agent_id = %id, error = %e, "failed to send pause");
            }
        }
    }

    /// Resume all agents
    pub async fn resume_all(&self) {
        for (id, tx) in &self.agent_commands {
            if let Err(e) = tx.send(CoordinatorCommand::Resume).await {
                warn!(agent_id = %id, error = %e, "failed to send resume");
            }
        }
    }

    /// Force-close all agents (best-effort)
    pub async fn force_close_all(&self) {
        info!("coordinator: sending force-close to all agents");
        for (id, tx) in &self.agent_commands {
            if let Err(e) = tx.send(CoordinatorCommand::ForceClose).await {
                warn!(agent_id = %id, error = %e, "failed to send force-close");
            }
        }
    }

    /// Shutdown all agents gracefully
    pub async fn shutdown(&self) {
        info!("coordinator: sending shutdown to all agents");
        for (id, tx) in &self.agent_commands {
            if let Err(e) = tx.send(CoordinatorCommand::Shutdown).await {
                warn!(agent_id = %id, error = %e, "failed to send shutdown");
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
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        self.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.persist_exit_reason_intent(&intent).await;
        }

        match self.risk_gate.check_order(&intent).await {
            RiskCheckResult::Passed => {
                self.persist_risk_decision(&intent, "PASSED", None, None)
                    .await;
                let mut queue = self.order_queue.write().await;
                match queue.enqueue(intent) {
                    Ok(()) => {
                        debug!(
                            %agent_id, %intent_id,
                            "order enqueued"
                        );
                    }
                    Err(e) => {
                        warn!(%agent_id, %intent_id, error = %e, "queue full, order dropped");
                    }
                }
            }
            RiskCheckResult::Blocked(reason) => {
                self.persist_risk_decision(&intent, "BLOCKED", Some(reason.to_string()), None)
                    .await;
                warn!(
                    %agent_id, %intent_id,
                    reason = ?reason,
                    "order blocked by risk gate"
                );
                self.risk_gate
                    .record_failure(&agent_id, &format!("{:?}", reason))
                    .await;
            }
            RiskCheckResult::Adjusted(suggestion) => {
                self.persist_risk_decision(
                    &intent,
                    "ADJUSTED",
                    None,
                    Some((suggestion.max_shares, suggestion.reason.clone())),
                )
                .await;
                info!(
                    %agent_id, %intent_id,
                    max_shares = suggestion.max_shares,
                    reason = %suggestion.reason,
                    "order adjusted by risk gate — dropping (agent should resubmit)"
                );
            }
        }
    }

    /// Update agent snapshot in global state
    async fn handle_state_update(&self, snapshot: AgentSnapshot) {
        let agent_id = snapshot.agent_id.clone();

        // Update risk gate with latest exposure data
        self.risk_gate
            .update_agent_exposure(
                &agent_id,
                snapshot.exposure,
                snapshot.unrealized_pnl,
                snapshot.position_count,
                0, // unhedged count not tracked in snapshot
            )
            .await;

        // Store snapshot
        let mut state = self.global_state.write().await;
        state.agents.insert(agent_id, snapshot);
    }

    /// Drain the order queue and execute via OrderExecutor
    async fn drain_and_execute(&self) {
        let batch = {
            let mut queue = self.order_queue.write().await;
            queue.cleanup_expired();
            queue.dequeue_batch(self.config.batch_size)
        };

        if batch.is_empty() {
            return;
        }

        debug!(count = batch.len(), "draining order queue");

        for intent in batch {
            let agent_id = intent.agent_id.clone();
            let intent_id = intent.intent_id;
            let execute_started_at = Utc::now();
            let queue_delay_ms = execute_started_at
                .signed_duration_since(intent.created_at)
                .num_milliseconds()
                .max(0);

            // Convert OrderIntent → OrderRequest for the executor
            let request = Self::intent_to_request(&intent);

            match self.executor.execute(&request).await {
                Ok(result) => {
                    info!(
                        %agent_id, %intent_id,
                        order_id = %result.order_id,
                        filled = result.filled_shares,
                        "order executed successfully"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        Some(&result),
                        None,
                        Some(queue_delay_ms),
                    )
                    .await;

                    // Record success with risk gate
                    self.risk_gate
                        .record_success(&agent_id, Decimal::ZERO)
                        .await;

                    // Open position in aggregator if filled
                    if result.filled_shares > 0 {
                        let _pos_id = self
                            .positions
                            .open_position(
                                &agent_id,
                                intent.domain.clone(),
                                &intent.market_slug,
                                &intent.token_id,
                                intent.side.clone(),
                                result.filled_shares,
                                result.avg_fill_price.unwrap_or(intent.limit_price),
                            )
                            .await;
                    }
                }
                Err(e) => {
                    error!(
                        %agent_id, %intent_id,
                        error = %e,
                        "order execution failed"
                    );

                    self.persist_execution(
                        &intent,
                        &request,
                        None,
                        Some(e.to_string()),
                        Some(queue_delay_ms),
                    )
                    .await;

                    self.risk_gate
                        .record_failure(&agent_id, &e.to_string())
                        .await;
                }
            }
        }
    }

    async fn persist_execution(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        error_message: Option<String>,
        queue_delay_ms: Option<i64>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let dry_run = self.executor.is_dry_run();

        let (order_id, status, filled_shares, avg_fill_price, elapsed_ms) = match result {
            Some(r) => (
                Some(r.order_id.clone()),
                format!("{:?}", r.status),
                r.filled_shares as i64,
                r.avg_fill_price,
                Some(r.elapsed_ms as i64),
            ),
            None => (
                None,
                format!("{:?}", crate::domain::OrderStatus::Failed),
                0,
                None,
                None,
            ),
        };

        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let config_hash = intent.metadata.get("config_hash").cloned();

        let query = sqlx::query(
            r#"
            INSERT INTO agent_order_executions (
                account_id,
                agent_id,
                intent_id,
                domain,
                market_slug,
                token_id,
                market_side,
                is_buy,
                shares,
                limit_price,
                order_id,
                status,
                filled_shares,
                avg_fill_price,
                elapsed_ms,
                dry_run,
                error,
                intent_created_at,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                order_id = EXCLUDED.order_id,
                status = EXCLUDED.status,
                filled_shares = EXCLUDED.filled_shares,
                avg_fill_price = EXCLUDED.avg_fill_price,
                elapsed_ms = EXCLUDED.elapsed_ms,
                dry_run = EXCLUDED.dry_run,
                error = EXCLUDED.error,
                metadata = EXCLUDED.metadata,
                executed_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(&intent.agent_id)
        .bind(intent.intent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(intent.is_buy)
        .bind(intent.shares as i64)
        .bind(request.limit_price)
        .bind(order_id)
        .bind(status)
        .bind(filled_shares)
        .bind(avg_fill_price)
        .bind(elapsed_ms)
        .bind(dry_run)
        .bind(error_message.clone())
        .bind(intent.created_at)
        .bind(sqlx::types::Json(metadata));

        if let Err(e) = query.execute(pool).await {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist agent order execution"
            );
        }

        self.persist_execution_analysis(intent, request, result, queue_delay_ms, config_hash)
            .await;

        if !intent.is_buy {
            self.persist_exit_reason_execution(intent, result, error_message)
                .await;
        }
    }

    fn metadata_decimal(intent: &OrderIntent, key: &str) -> Option<Decimal> {
        intent
            .metadata
            .get(key)
            .and_then(|v| Decimal::from_str(v).ok())
    }

    async fn persist_signal_from_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let signal_type = intent
            .metadata
            .get("signal_type")
            .cloned()
            .unwrap_or_else(|| {
                if intent.is_buy {
                    "entry_intent".to_string()
                } else {
                    "exit_intent".to_string()
                }
            });
        let symbol = intent.metadata.get("symbol").cloned();
        let confidence = Self::metadata_decimal(intent, "signal_confidence");
        let momentum_value = Self::metadata_decimal(intent, "signal_momentum_value");
        let short_ma = Self::metadata_decimal(intent, "signal_short_ma");
        let long_ma = Self::metadata_decimal(intent, "signal_long_ma");
        let rolling_volatility = Self::metadata_decimal(intent, "signal_rolling_volatility");
        let fair_value = Self::metadata_decimal(intent, "signal_fair_value");
        let market_price = Self::metadata_decimal(intent, "signal_market_price");
        let edge = Self::metadata_decimal(intent, "signal_edge");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let context =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO signal_history (
                account_id, intent_id, agent_id, strategy_id, domain, signal_type, market_slug, token_id,
                symbol, side, confidence, momentum_value, short_ma, long_ma, rolling_volatility,
                fair_value, market_price, edge, config_hash, context
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15,
                $16,$17,$18,$19,$20
            )
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(&strategy_id)
        .bind(intent.domain.to_string())
        .bind(&signal_type)
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(symbol)
        .bind(intent.side.as_str())
        .bind(confidence)
        .bind(momentum_value)
        .bind(short_ma)
        .bind(long_ma)
        .bind(rolling_volatility)
        .bind(fair_value)
        .bind(market_price)
        .bind(edge)
        .bind(config_hash)
        .bind(sqlx::types::Json(context))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist signal history"
            );
        }
    }

    async fn persist_risk_decision(
        &self,
        intent: &OrderIntent,
        decision: &str,
        block_reason: Option<String>,
        adjusted: Option<(u64, String)>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let (suggestion_max_shares, suggestion_reason) = adjusted
            .map(|(shares, reason)| (Some(shares as i64), Some(reason)))
            .unwrap_or((None, None));
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO risk_gate_decisions (
                account_id, intent_id, agent_id, domain, decision, block_reason, suggestion_max_shares,
                suggestion_reason, notional_value, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            ON CONFLICT (intent_id) DO UPDATE SET
                decision = EXCLUDED.decision,
                block_reason = EXCLUDED.block_reason,
                suggestion_max_shares = EXCLUDED.suggestion_max_shares,
                suggestion_reason = EXCLUDED.suggestion_reason,
                notional_value = EXCLUDED.notional_value,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                decided_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(decision)
        .bind(block_reason)
        .bind(suggestion_max_shares)
        .bind(suggestion_reason)
        .bind(intent.notional_value())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist risk gate decision"
            );
        }
    }

    async fn persist_exit_reason_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let reason_code = intent
            .metadata
            .get("exit_reason")
            .or_else(|| intent.metadata.get("reason_code"))
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let reason_detail = intent.metadata.get("exit_detail").cloned();
        let entry_price = Self::metadata_decimal(intent, "entry_price");
        let pnl_pct = Self::metadata_decimal(intent, "pnl_pct");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, pnl_pct, status, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'INTENT_SUBMITTED',$12,$13)
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_code = EXCLUDED.reason_code,
                reason_detail = EXCLUDED.reason_detail,
                entry_price = COALESCE(EXCLUDED.entry_price, exit_reasons.entry_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(reason_code)
        .bind(reason_detail)
        .bind(entry_price)
        .bind(pnl_pct)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason intent"
            );
        }
    }

    async fn persist_exit_reason_execution(
        &self,
        intent: &OrderIntent,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        error_message: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let executed_price = result.and_then(|r| r.avg_fill_price);
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());
        let reason_detail = error_message.or_else(|| {
            intent
                .metadata
                .get("exit_detail")
                .cloned()
                .or_else(|| intent.metadata.get("error").cloned())
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, exit_price, pnl_pct, status, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_detail = COALESCE(EXCLUDED.reason_detail, exit_reasons.reason_detail),
                exit_price = COALESCE(EXCLUDED.exit_price, exit_reasons.exit_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = COALESCE(EXCLUDED.config_hash, exit_reasons.config_hash),
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(
            intent
                .metadata
                .get("exit_reason")
                .or_else(|| intent.metadata.get("reason_code"))
                .cloned()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
        )
        .bind(reason_detail)
        .bind(Self::metadata_decimal(intent, "entry_price"))
        .bind(executed_price)
        .bind(Self::metadata_decimal(intent, "pnl_pct"))
        .bind(status)
        .bind(intent.metadata.get("config_hash").cloned())
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason execution"
            );
        }
    }

    async fn persist_execution_analysis(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&crate::strategy::executor::ExecutionResult>,
        queue_delay_ms: Option<i64>,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.execution_log_pool.as_ref() else {
            return;
        };

        let expected_price = request.limit_price;
        let executed_price = result.and_then(|r| r.avg_fill_price);
        let execution_latency_ms = result.map(|r| r.elapsed_ms as i64);
        let total_latency_ms = match (queue_delay_ms, execution_latency_ms) {
            (Some(q), Some(e)) => Some(q + e),
            (Some(q), None) => Some(q),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        };

        let actual_slippage_bps = executed_price.and_then(|fill| {
            if expected_price.is_zero() {
                return None;
            }
            let signed = if intent.is_buy {
                (fill - expected_price) / expected_price
            } else {
                (expected_price - fill) / expected_price
            };
            Some(signed * Decimal::from(10_000))
        });

        let expected_slippage_bps = Self::metadata_decimal(intent, "expected_slippage_bps")
            .or_else(|| Self::metadata_decimal(intent, "signal_expected_slippage_bps"));
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());

        let result = sqlx::query(
            r#"
            INSERT INTO execution_analysis (
                account_id, intent_id, agent_id, domain, market_slug, token_id, is_buy,
                expected_price, executed_price, expected_slippage_bps, actual_slippage_bps,
                queue_delay_ms, execution_latency_ms, total_latency_ms,
                status, dry_run, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,
                $8,$9,$10,$11,
                $12,$13,$14,
                $15,$16,$17,$18
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                executed_price = EXCLUDED.executed_price,
                expected_slippage_bps = EXCLUDED.expected_slippage_bps,
                actual_slippage_bps = EXCLUDED.actual_slippage_bps,
                queue_delay_ms = EXCLUDED.queue_delay_ms,
                execution_latency_ms = EXCLUDED.execution_latency_ms,
                total_latency_ms = EXCLUDED.total_latency_ms,
                status = EXCLUDED.status,
                dry_run = EXCLUDED.dry_run,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                recorded_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.is_buy)
        .bind(expected_price)
        .bind(executed_price)
        .bind(expected_slippage_bps)
        .bind(actual_slippage_bps)
        .bind(queue_delay_ms)
        .bind(execution_latency_ms)
        .bind(total_latency_ms)
        .bind(status)
        .bind(self.executor.is_dry_run())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist execution analysis"
            );
        }
    }

    /// Refresh GlobalState from aggregators
    async fn refresh_global_state(&self) {
        let portfolio = self.positions.aggregate().await;
        let positions = self.positions.all_positions().await;
        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let circuit_breaker_events = self.risk_gate.circuit_breaker_events().await;
        let queue_stats = self.order_queue.read().await.stats();
        let total_realized = self.positions.total_realized_pnl().await;

        let mut state = self.global_state.write().await;
        state.portfolio = portfolio;
        state.positions = positions;
        state.risk_state = risk_state;
        state.daily_pnl = daily_pnl;
        state.daily_loss_limit = daily_loss_limit;
        state.circuit_breaker_events = circuit_breaker_events;
        state.queue_stats = QueueStatsSnapshot::from(queue_stats);
        state.total_realized_pnl = total_realized;
        state.last_refresh = Utc::now();

        // Check for stale agents
        let timeout = chrono::Duration::milliseconds(self.config.heartbeat_timeout_ms as i64);
        let now = Utc::now();
        for (id, agent) in state.agents.iter_mut() {
            if now - agent.last_heartbeat > timeout
                && matches!(agent.status, crate::platform::AgentStatus::Running)
            {
                warn!(agent_id = %id, "agent heartbeat stale");
                agent.error_message = Some("heartbeat timeout".into());
            }
        }
    }

    /// Convert an OrderIntent into an OrderRequest for the executor
    fn intent_to_request(intent: &OrderIntent) -> OrderRequest {
        use crate::domain::OrderSide;

        let order_side = if intent.is_buy {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };

        OrderRequest {
            client_order_id: intent.intent_id.to_string(),
            idempotency_key: Some(intent.intent_id.to_string()),
            token_id: intent.token_id.clone(),
            market_side: intent.side.clone(),
            order_side,
            shares: intent.shares,
            limit_price: intent.limit_price,
            order_type: crate::domain::OrderType::Limit,
            time_in_force: crate::domain::TimeInForce::GTC,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{AgentStatus, Domain, OrderPriority, QueueStats};
    use rust_decimal_macros::dec;

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
            last_heartbeat: Utc::now(),
            error_message: None,
        }
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
}
