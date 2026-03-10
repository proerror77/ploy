use chrono::Utc;
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::warn;

use crate::adapters::{PolymarketClient, PolymarketWebSocket};
use crate::coordinator::CoordinatorHandle;
use crate::error::{PloyError, Result};
use crate::platform::PlatformDataPlane;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{DataFeed, DataFeedManager, OrderUpdate, StrategyFactory, StrategyManager};

use super::actions::handle_strategy_actions_runtime;
use super::observability::is_managed_staggered_arb_label;
use super::session::ManagedRuntimeSession;

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
    let status = crate::agent_runtime::AgentStatus::Running;

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

async fn ensure_managed_runtime_observability(
    strategy_label: &str,
    observability_pool: Option<&PgPool>,
    is_split_arb_managed: bool,
) {
    if !is_split_arb_managed {
        return;
    }

    if let Some(pool) = observability_pool {
        if let Err(error) = crate::persistence::ensure_strategy_observability_tables(pool).await {
            warn!(
                strategy = strategy_label,
                error = %error,
                "failed to ensure strategy observability tables for managed runtime"
            );
        }
    }
}

fn build_managed_feed_manager(
    required_feeds: &[DataFeed],
    data_plane: Option<Arc<PlatformDataPlane>>,
    manager: Arc<StrategyManager>,
    pm_client: &PolymarketClient,
    pm_ws_url: &str,
) -> DataFeedManager {
    if let Some(data_plane) = data_plane {
        return DataFeedManager::from_data_plane(data_plane, manager)
            .with_pm_client(pm_client.clone());
    }

    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only = true;

    for feed in required_feeds {
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

    let mut feed_manager = DataFeedManager::new(manager);
    if !binance_spot_symbols.is_empty() {
        feed_manager = feed_manager.with_binance(binance_spot_symbols);
    }

    if !binance_kline_symbols.is_empty() && !binance_kline_intervals.is_empty() {
        let backfill_limit = std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(300);
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    let has_polymarket_feed = required_feeds.iter().any(|feed| {
        matches!(
            feed,
            DataFeed::PolymarketEvents { .. } | DataFeed::PolymarketQuotes { .. }
        )
    });
    if has_polymarket_feed {
        let pm_ws = PolymarketWebSocket::new(pm_ws_url);
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client.clone());
    }

    feed_manager
}

async fn start_account_claimer_daemon_if_needed(
    strategy_label: &str,
    agent_id: &str,
    dry_run: bool,
) -> Result<()> {
    #[cfg(feature = "claimer_daemon")]
    {
        if !dry_run {
            if let Err(error) = crate::strategy::ensure_account_claimer_daemon().await {
                warn!(
                    strategy = strategy_label,
                    agent_id = agent_id,
                    error = %error,
                    "failed to start account-level auto-claimer daemon"
                );
            }
        }
    }

    #[cfg(not(feature = "claimer_daemon"))]
    let _ = (strategy_label, agent_id, dry_run);

    Ok(())
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
