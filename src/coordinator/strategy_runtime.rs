//! Canonical managed strategy runtime for live strategy execution.
//!
//! This module owns the runtime concerns that previously lived inside
//! `bootstrap.rs`: strategy instantiation, feed wiring, action execution,
//! and managed-runtime observability. `bootstrap` should only assemble and
//! launch this runtime, not re-implement its internals.

use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

use crate::adapters::PolymarketClient;
use crate::coordinator::{CoordinatorCommand, CoordinatorHandle};
use crate::data_plane::PlatformDataPlane;
use crate::domain::Domain;
use crate::error::Result;
use crate::strategy::OrderUpdate;

mod actions;
mod control;
mod observability;
mod order_store;
mod session;
mod signal_history;
mod startup;

use control::drive_managed_runtime_control_loop;
use startup::start_managed_runtime_session;

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
    let mut session = start_managed_runtime_session(
        strategy_label,
        agent_id,
        &strategy_config_toml,
        dry_run,
        pm_client,
        &pm_ws_url,
        data_plane,
        observability_pool,
        observability_account_id,
        coordinator_handle,
        order_update_rx,
    )
    .await?;

    info!(
        strategy = strategy_label,
        agent_id = agent_id,
        strategy_id = %session.strategy_id,
        subscribed_tokens = session.subscribed_token_count,
        dry_run = dry_run,
        "managed strategy runtime started"
    );

    drive_managed_runtime_control_loop(
        strategy_label,
        agent_id,
        domain,
        &session.strategy_id,
        session.started_at,
        session.manager.clone(),
        session.paused.clone(),
        session.orders_submitted.clone(),
        session.orders_filled.clone(),
        &mut session.status,
        &mut cmd_rx,
        &mut shutdown_rx,
    )
    .await;

    session.shutdown(strategy_label, agent_id).await;

    Ok(())
}
