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

use crate::adapters::{PolymarketClient, PolymarketWebSocket};
use crate::agent_runtime::AgentStatus;
use crate::coordinator::CoordinatorCommand;
use crate::error::Result;
use crate::platform::{Domain, PlatformDataPlane};
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{DataFeed, DataFeedManager, StrategyFactory, StrategyManager};

mod actions;
mod control;
mod observability;
mod order_store;

use actions::handle_strategy_actions_runtime;
use control::drive_managed_runtime_control_loop;
use observability::is_managed_staggered_arb_label;

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
    mut cmd_rx: mpsc::Receiver<CoordinatorCommand>,
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

    if is_managed_staggered_arb_label(strategy_label) {
        if let Some(pool) = observability_pool.as_ref() {
            if let Err(e) = crate::persistence::ensure_strategy_observability_tables(pool).await {
                warn!(
                    strategy = strategy_label,
                    error = %e,
                    "failed to ensure strategy observability tables for managed runtime"
                );
            }
        }
    }

    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only = true;

    for feed in &required_feeds {
        match feed {
            DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            DataFeed::BinanceKlines {
                symbols,
                intervals,
                closed_only,
            } => {
                binance_kline_symbols.extend(symbols.clone());
                binance_kline_intervals.extend(intervals.clone());
                if !*closed_only {
                    binance_kline_closed_only = false;
                }
            }
            _ => {}
        }
    }

    binance_spot_symbols.sort();
    binance_spot_symbols.dedup();
    binance_kline_symbols.sort();
    binance_kline_symbols.dedup();
    binance_kline_intervals.sort();
    binance_kline_intervals.dedup();

    let feed_manager = if let Some(dp) = data_plane {
        DataFeedManager::from_data_plane(dp, manager.clone()).with_pm_client(pm_client.clone())
    } else {
        let mut feed_manager = DataFeedManager::new(manager.clone());
        if !binance_spot_symbols.is_empty() {
            feed_manager = feed_manager.with_binance(binance_spot_symbols.clone());
        }

        if !binance_kline_symbols.is_empty() && !binance_kline_intervals.is_empty() {
            let backfill_limit = std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(300);
            feed_manager = feed_manager.with_binance_klines(
                binance_kline_symbols.clone(),
                binance_kline_intervals.clone(),
                binance_kline_closed_only,
                backfill_limit,
            );
        }

        let has_polymarket_feed = required_feeds.iter().any(|f| {
            matches!(
                f,
                DataFeed::PolymarketEvents { .. } | DataFeed::PolymarketQuotes { .. }
            )
        });
        if has_polymarket_feed {
            let pm_ws = PolymarketWebSocket::new(&pm_ws_url);
            feed_manager = feed_manager.with_polymarket(pm_ws, pm_client.clone());
        }

        feed_manager
    };

    manager.start_strategy(strategy, None).await?;

    #[cfg(feature = "claimer_daemon")]
    if !dry_run {
        if let Err(e) = crate::strategy::ensure_account_claimer_daemon().await {
            warn!(
                strategy = strategy_label,
                agent_id = agent_id,
                error = %e,
                "failed to start account-level auto-claimer daemon"
            );
        }
    }

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
    let observability_pool_for_actions = observability_pool.clone();
    let observability_account_for_actions = observability_account_id.clone();
    let action_task = tokio::spawn(async move {
        handle_strategy_actions_runtime(
            &strategy_label_owned,
            manager_for_actions,
            action_rx,
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
