//! Canonical managed strategy runtime for live strategy execution.
//!
//! This module owns the runtime concerns that previously lived inside
//! `bootstrap.rs`: strategy instantiation, feed wiring, action execution,
//! and managed-runtime observability. `bootstrap` should only assemble and
//! launch this runtime, not re-implement its internals.

use chrono::Utc;
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::adapters::PolymarketClient;
use crate::agent_runtime::AgentStatus;
use crate::coordinator::{CoordinatorCommand, CoordinatorHandle};
use crate::error::Result;
use crate::platform::{Domain, PlatformDataPlane};
use crate::strategy::executor::OrderExecutor;
use crate::strategy::OrderUpdate;
use crate::strategy::{StrategyFactory, StrategyManager};

mod actions;
mod control;
mod observability;
mod order_store;
mod setup;

use actions::handle_strategy_actions_runtime;
use control::drive_managed_runtime_control_loop;
use observability::is_managed_staggered_arb_label;
use setup::{
    build_managed_feed_manager, ensure_managed_runtime_observability,
    start_account_claimer_daemon_if_needed,
};

pub(crate) async fn run_managed_strategy_runtime(
    strategy_label: &str,
    agent_id: &str,
    domain: Domain,
    strategy_config_toml: String,
    dry_run: bool,
    pm_client: PolymarketClient,
    pm_ws_url: String,
    data_plane: Option<Arc<PlatformDataPlane>>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
    coordinator_handle: CoordinatorHandle,
    mut cmd_rx: mpsc::Receiver<CoordinatorCommand>,
    order_update_rx: mpsc::Receiver<OrderUpdate>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let strategy = StrategyFactory::from_toml(&strategy_config_toml, dry_run)?;
    let strategy_id = strategy.id().to_string();
    let required_feeds = strategy.required_feeds();
    let started_at = Utc::now();
    let paused = Arc::new(AtomicBool::new(false));
    let orders_submitted = Arc::new(AtomicU64::new(0));
    let orders_filled = Arc::new(AtomicU64::new(0));
    let mut status = AgentStatus::Running;

    let manager = Arc::new(StrategyManager::new(1000));
    let action_rx = manager.take_action_receiver().await.ok_or_else(|| {
        crate::error::PloyError::Internal(format!(
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
        &pm_ws_url,
    );

    manager.start_strategy(strategy, None).await?;
    start_account_claimer_daemon_if_needed(strategy_label, agent_id, dry_run).await?;

    feed_manager.start().await?;
    let subscribed_tokens = feed_manager.start_for_feeds(required_feeds).await?;

    let executor = Arc::new(OrderExecutor::new(
        pm_client.clone(),
        crate::config::ExecutionConfig::default(),
    ));
    let manager_for_actions = manager.clone();
    let paused_for_actions = paused.clone();
    let orders_submitted_for_actions = orders_submitted.clone();
    let orders_filled_for_actions = orders_filled.clone();
    let strategy_label_owned = strategy_label.to_string();
    let strategy_id_for_actions = strategy_id.clone();
    let agent_id_owned = agent_id.to_string();
    let observability_pool_for_actions = observability_pool.clone();
    let observability_account_for_actions = observability_account_id.clone();
    let action_task = tokio::spawn(async move {
        handle_strategy_actions_runtime(
            &strategy_label_owned,
            &agent_id_owned,
            &strategy_id_for_actions,
            manager_for_actions,
            action_rx,
            order_update_rx,
            coordinator_handle,
            executor,
            paused_for_actions,
            orders_submitted_for_actions,
            orders_filled_for_actions,
            observability_pool_for_actions,
            observability_account_for_actions,
        )
        .await;
    });

    info!(
        strategy = strategy_label,
        agent_id = agent_id,
        strategy_id = %strategy_id,
        subscribed_tokens = subscribed_tokens.len(),
        dry_run = dry_run,
        "managed strategy runtime started"
    );

    drive_managed_runtime_control_loop(
        strategy_label,
        agent_id,
        domain,
        &strategy_id,
        started_at,
        manager.clone(),
        paused.clone(),
        orders_submitted.clone(),
        orders_filled.clone(),
        &mut status,
        &mut cmd_rx,
        &mut shutdown_rx,
    )
    .await;

    if let Err(e) = manager.stop_all(true).await {
        warn!(
            strategy = strategy_label,
            agent_id = agent_id,
            strategy_id = %strategy_id,
            error = %e,
            "managed strategy runtime stop_all failed"
        );
    }
    action_task.abort();

    Ok(())
}
