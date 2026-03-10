use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::warn;

use crate::adapters::PolymarketClient;
use crate::agent_runtime::AgentStatus;
use crate::coordinator::CoordinatorHandle;
use crate::error::{PloyError, Result};
use crate::platform::PlatformDataPlane;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::OrderUpdate;
use crate::strategy::{StrategyFactory, StrategyManager};

use super::actions::handle_strategy_actions_runtime;
use super::observability::is_managed_staggered_arb_label;
use super::setup::{
    build_managed_feed_manager, ensure_managed_runtime_observability,
    start_account_claimer_daemon_if_needed,
};

pub(super) struct ManagedRuntimeSession {
    pub(super) strategy_id: String,
    pub(super) started_at: DateTime<Utc>,
    pub(super) manager: Arc<StrategyManager>,
    pub(super) paused: Arc<AtomicBool>,
    pub(super) orders_submitted: Arc<AtomicU64>,
    pub(super) orders_filled: Arc<AtomicU64>,
    pub(super) status: AgentStatus,
    pub(super) subscribed_token_count: usize,
    action_task: JoinHandle<()>,
}

pub(super) async fn start_managed_runtime_session(
    strategy_label: &str,
    agent_id: &str,
    strategy_config_toml: &str,
    dry_run: bool,
    pm_client: PolymarketClient,
    pm_ws_url: &str,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
    coordinator_handle: CoordinatorHandle,
    order_update_rx: mpsc::Receiver<OrderUpdate>,
) -> Result<ManagedRuntimeSession> {
    let strategy = StrategyFactory::from_toml(strategy_config_toml, dry_run)?;
    let strategy_id = strategy.id().to_string();
    let required_feeds = strategy.required_feeds();
    let started_at = Utc::now();
    let paused = Arc::new(AtomicBool::new(false));
    let orders_submitted = Arc::new(AtomicU64::new(0));
    let orders_filled = Arc::new(AtomicU64::new(0));
    let status = AgentStatus::Running;

    let manager = Arc::new(StrategyManager::new(1000));
    let action_rx = manager.take_action_receiver().await.ok_or_else(|| {
        PloyError::Internal(format!(
            "strategy {} failed to take action receiver",
            strategy_label
        ))
    })?;

    ensure_managed_runtime_observability(
        strategy_label,
        observability_pool.as_ref(),
        is_managed_staggered_arb_label(strategy_label),
    )
    .await;

    let feed_manager = build_managed_feed_manager(
        &required_feeds,
        data_plane,
        manager.clone(),
        &pm_client,
        pm_ws_url,
    );

    manager.start_strategy(strategy, None).await?;
    start_account_claimer_daemon_if_needed(strategy_label, agent_id, dry_run).await?;

    feed_manager.start().await?;
    let subscribed_token_count = feed_manager.start_for_feeds(required_feeds).await?.len();

    let action_task = spawn_action_task(
        strategy_label,
        agent_id,
        &strategy_id,
        manager.clone(),
        action_rx,
        order_update_rx,
        coordinator_handle,
        pm_client,
        paused.clone(),
        orders_submitted.clone(),
        orders_filled.clone(),
        observability_pool,
        observability_account_id,
    );

    Ok(ManagedRuntimeSession {
        strategy_id,
        started_at,
        manager,
        paused,
        orders_submitted,
        orders_filled,
        status,
        subscribed_token_count,
        action_task,
    })
}

impl ManagedRuntimeSession {
    pub(super) async fn shutdown(self, strategy_label: &str, agent_id: &str) {
        if let Err(error) = self.manager.stop_all(true).await {
            warn!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %self.strategy_id,
                error = %error,
                "managed strategy runtime stop_all failed"
            );
        }
        self.action_task.abort();
    }
}

fn spawn_action_task(
    strategy_label: &str,
    agent_id: &str,
    strategy_id: &str,
    manager: Arc<StrategyManager>,
    action_rx: mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    order_update_rx: mpsc::Receiver<OrderUpdate>,
    coordinator_handle: CoordinatorHandle,
    pm_client: PolymarketClient,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
) -> JoinHandle<()> {
    let executor = Arc::new(OrderExecutor::new(
        pm_client,
        crate::config::ExecutionConfig::default(),
    ));
    let strategy_label_owned = strategy_label.to_string();
    let agent_id_owned = agent_id.to_string();
    let strategy_id_owned = strategy_id.to_string();

    tokio::spawn(async move {
        handle_strategy_actions_runtime(
            &strategy_label_owned,
            &agent_id_owned,
            &strategy_id_owned,
            manager,
            action_rx,
            order_update_rx,
            coordinator_handle,
            executor,
            paused,
            orders_submitted,
            orders_filled,
            observability_pool,
            observability_account_id,
        )
        .await;
    })
}
