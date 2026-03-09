//! Canonical managed strategy runtime for live strategy execution.
//!
//! This module owns the runtime concerns that previously lived inside
//! `bootstrap.rs`: strategy instantiation, feed wiring, action execution,
//! and managed-runtime observability. `bootstrap` should only assemble and
//! launch this runtime, not re-implement its internals.

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::adapters::{PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::agent_runtime::AgentStatus;
use crate::coordinator::{AgentHealthResponse, AgentSnapshot, CoordinatorCommand};
use crate::domain::OrderStatus;
use crate::error::Result;
use crate::platform::{Domain, PlatformDataPlane};
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{
    DataFeed, DataFeedManager, StrategyAction, StrategyFactory, StrategyManager,
};

mod observability;
mod order_store;

use observability::{
    is_managed_staggered_arb_label, persist_live_order_signal_history,
    persist_split_arb_signal_history, split_arb_event_signal_type, split_arb_leg_and_mode,
    split_arb_status_key,
};
use order_store::{
    normalize_runtime_order_request, persist_runtime_order_insert,
    persist_runtime_order_result, RuntimeOrderStore,
};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

async fn handle_strategy_actions_runtime(
    strategy_label: &str,
    manager: Arc<StrategyManager>,
    mut rx: mpsc::Receiver<(String, StrategyAction)>,
    executor: Arc<OrderExecutor>,
    paused: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
) {
    let runtime_order_store: Option<Arc<dyn RuntimeOrderStore>> = observability_pool
        .as_ref()
        .map(|pool| Arc::new(PostgresStore::from_pool(pool.clone())) as Arc<dyn RuntimeOrderStore>);

    while let Some((strategy_id, action)) = rx.recv().await {
        let split_arb_managed = is_managed_staggered_arb_label(strategy_label);
        match action {
            StrategyAction::SubmitIntent { intent } => {
                let client_order_id = intent.client_order_id.clone();
                let mut order = crate::strategy::order_request_from_intent(&intent);
                normalize_runtime_order_request(&client_order_id, &mut order);

                if paused.load(Ordering::Relaxed) {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        "strategy submit-order rejected while paused"
                    );
                    let update = crate::strategy::OrderUpdate {
                        order_id: client_order_id.clone(),
                        client_order_id: Some(client_order_id.clone()),
                        status: OrderStatus::Rejected,
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: Some("strategy paused by coordinator".to_string()),
                    };
                    manager.send_order_update(update.clone());
                    if let Some(pool) = observability_pool.as_ref() {
                        let context = json!({
                            "source": "managed_runtime",
                            "phase": "submit_paused",
                            "order_id": update.order_id,
                            "client_order_id": client_order_id.clone(),
                            "status": format!("{:?}", update.status),
                            "filled_qty": update.filled_qty,
                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                            "error": update.error,
                        });
                        persist_live_order_signal_history(
                            pool,
                            &observability_account_id,
                            strategy_label,
                            &strategy_id,
                            "live_order_rejected",
                            Some(order.token_id.as_str()),
                            Some(order.market_side.as_str()),
                            Some(order.limit_price),
                            update.avg_fill_price,
                            context,
                        )
                        .await;
                    }
                    if split_arb_managed {
                        if let Some(pool) = observability_pool.as_ref() {
                            let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                            let signal_type = format!("split_arb_{}_{}_rejected", leg, mode);
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_paused",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id,
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "error": update.error,
                                "leg": leg,
                                "mode": mode,
                            });
                            persist_split_arb_signal_history(
                                pool,
                                &observability_account_id,
                                strategy_label,
                                &strategy_id,
                                &signal_type,
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                None,
                                context,
                            )
                            .await;
                        }
                    }
                    continue;
                }

                orders_submitted.fetch_add(1, Ordering::Relaxed);
                if let Some(store) = runtime_order_store.as_ref() {
                    if let Err(e) =
                        persist_runtime_order_insert(store.as_ref(), &strategy_id, &order).await
                    {
                        warn!(
                            strategy = strategy_label,
                            strategy_id = %strategy_id,
                            client_order_id = %client_order_id,
                            error = %e,
                            "failed to persist managed runtime order insert"
                        );
                    }
                }
                match executor.execute(&order).await {
                    Ok(result) => {
                        if let Some(store) = runtime_order_store.as_ref() {
                            if let Err(e) = persist_runtime_order_result(
                                store.as_ref(),
                                &client_order_id,
                                &result.order_id,
                                result.status,
                                result.filled_shares,
                                result.avg_fill_price,
                                order.limit_price,
                            )
                            .await
                            {
                                warn!(
                                    strategy = strategy_label,
                                    strategy_id = %strategy_id,
                                    client_order_id = %client_order_id,
                                    exchange_order_id = %result.order_id,
                                    error = %e,
                                    "failed to persist managed runtime order result"
                                );
                            }
                        }
                        if matches!(result.status, OrderStatus::Filled) {
                            orders_filled.fetch_add(1, Ordering::Relaxed);
                        }
                        let update = crate::strategy::OrderUpdate {
                            order_id: result.order_id,
                            client_order_id: Some(client_order_id.clone()),
                            status: result.status,
                            filled_qty: result.filled_shares,
                            avg_fill_price: result.avg_fill_price,
                            timestamp: Utc::now(),
                            error: None,
                        };
                        manager.send_order_update(update.clone());
                        if let Some(pool) = observability_pool.as_ref() {
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_result",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id.clone(),
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                            });
                            persist_live_order_signal_history(
                                pool,
                                &observability_account_id,
                                strategy_label,
                                &strategy_id,
                                "live_order_submit_result",
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                context,
                            )
                            .await;
                        }

                        if split_arb_managed {
                            if let Some(pool) = observability_pool.as_ref() {
                                let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                                let status_key = split_arb_status_key(update.status);
                                let signal_type =
                                    format!("split_arb_{}_{}_{}", leg, mode, status_key);
                                let context = json!({
                                    "source": "managed_runtime",
                                    "phase": "submit_result",
                                    "order_id": update.order_id,
                                    "client_order_id": client_order_id.clone(),
                                    "status": format!("{:?}", update.status),
                                    "filled_qty": update.filled_qty,
                                    "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                    "leg": leg,
                                    "mode": mode,
                                });
                                persist_split_arb_signal_history(
                                    pool,
                                    &observability_account_id,
                                    strategy_label,
                                    &strategy_id,
                                    &signal_type,
                                    Some(order.token_id.as_str()),
                                    Some(order.market_side.as_str()),
                                    Some(order.limit_price),
                                    update.avg_fill_price,
                                    None,
                                    context,
                                )
                                .await;
                            }
                        }

                        if split_arb_managed
                            && matches!(
                                update.status,
                                OrderStatus::Pending
                                    | OrderStatus::Submitted
                                    | OrderStatus::PartiallyFilled
                            )
                        {
                            let manager_for_poll = manager.clone();
                            let executor_for_poll = executor.clone();
                            let orders_filled_for_poll = orders_filled.clone();
                            let observability_pool_for_poll = observability_pool.clone();
                            let observability_account_for_poll = observability_account_id.clone();
                            let runtime_order_store_for_poll = runtime_order_store.clone();
                            let strategy_id_for_poll = strategy_id.clone();
                            let client_order_id_for_poll = client_order_id.clone();
                            let exchange_order_id_for_poll = update.order_id.clone();
                            let order_for_poll = order.clone();
                            let mut last_status = update.status;
                            let mut last_filled_qty = update.filled_qty;
                            let mut last_fill_price = update.avg_fill_price;
                            let poll_interval_ms =
                                env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MS", 1500)
                                    .clamp(200, 10_000);
                            let poll_max_ms =
                                env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MAX_MS", 600_000)
                                    .max(poll_interval_ms);
                            let strategy_label_owned = strategy_label.to_string();

                            tokio::spawn(async move {
                                let started_at = std::time::Instant::now();
                                while started_at.elapsed().as_millis() < poll_max_ms as u128 {
                                    tokio::time::sleep(Duration::from_millis(poll_interval_ms))
                                        .await;

                                    let polled = match executor_for_poll
                                        .query_order_status(&exchange_order_id_for_poll)
                                        .await
                                    {
                                        Ok(r) => r,
                                        Err(e) => {
                                            debug!(
                                                strategy = strategy_label_owned.as_str(),
                                                strategy_id = %strategy_id_for_poll,
                                                client_order_id = %client_order_id_for_poll,
                                                exchange_order_id = %exchange_order_id_for_poll,
                                                error = %e,
                                                "managed strategy poll status failed (will retry)"
                                            );
                                            continue;
                                        }
                                    };

                                    let changed = polled.status != last_status
                                        || polled.filled_shares != last_filled_qty
                                        || polled.avg_fill_price != last_fill_price;
                                    if !changed {
                                        if polled.status.is_terminal() {
                                            break;
                                        }
                                        continue;
                                    }

                                    if polled.status == OrderStatus::Filled
                                        && last_status != OrderStatus::Filled
                                    {
                                        orders_filled_for_poll.fetch_add(1, Ordering::Relaxed);
                                    }

                                    let update = crate::strategy::OrderUpdate {
                                        order_id: polled.order_id,
                                        client_order_id: Some(client_order_id_for_poll.clone()),
                                        status: polled.status,
                                        filled_qty: polled.filled_shares,
                                        avg_fill_price: polled.avg_fill_price,
                                        timestamp: Utc::now(),
                                        error: None,
                                    };
                                    manager_for_poll.send_order_update(update.clone());

                                    if let Some(store) = runtime_order_store_for_poll.as_ref() {
                                        if let Err(e) = persist_runtime_order_result(
                                            store.as_ref(),
                                            &client_order_id_for_poll,
                                            &update.order_id,
                                            update.status,
                                            update.filled_qty,
                                            update.avg_fill_price,
                                            order_for_poll.limit_price,
                                        )
                                        .await
                                        {
                                            warn!(
                                                strategy = strategy_label_owned.as_str(),
                                                strategy_id = %strategy_id_for_poll,
                                                client_order_id = %client_order_id_for_poll,
                                                exchange_order_id = %update.order_id,
                                                error = %e,
                                                "failed to persist managed runtime poll update"
                                            );
                                        }
                                    }

                                    if let Some(pool) = observability_pool_for_poll.as_ref() {
                                        let context = json!({
                                            "source": "managed_runtime",
                                            "phase": "poll",
                                            "order_id": update.order_id,
                                            "client_order_id": client_order_id_for_poll.clone(),
                                            "status": format!("{:?}", update.status),
                                            "filled_qty": update.filled_qty,
                                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                        });
                                        persist_live_order_signal_history(
                                            pool,
                                            &observability_account_for_poll,
                                            strategy_label_owned.as_str(),
                                            &strategy_id_for_poll,
                                            "live_order_poll_update",
                                            Some(order_for_poll.token_id.as_str()),
                                            Some(order_for_poll.market_side.as_str()),
                                            Some(order_for_poll.limit_price),
                                            update.avg_fill_price,
                                            context,
                                        )
                                        .await;
                                    }

                                    if let Some(pool) = observability_pool_for_poll.as_ref() {
                                        let (leg, mode) = split_arb_leg_and_mode(
                                            client_order_id_for_poll.as_str(),
                                        );
                                        let status_key = split_arb_status_key(update.status);
                                        let signal_type =
                                            format!("split_arb_{}_{}_{}", leg, mode, status_key);
                                        let context = json!({
                                            "source": "managed_runtime",
                                            "phase": "poll",
                                            "order_id": update.order_id,
                                            "client_order_id": client_order_id_for_poll.clone(),
                                            "status": format!("{:?}", update.status),
                                            "filled_qty": update.filled_qty,
                                            "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                            "leg": leg,
                                            "mode": mode,
                                        });
                                        persist_split_arb_signal_history(
                                            pool,
                                            &observability_account_for_poll,
                                            strategy_label_owned.as_str(),
                                            &strategy_id_for_poll,
                                            &signal_type,
                                            Some(order_for_poll.token_id.as_str()),
                                            Some(order_for_poll.market_side.as_str()),
                                            Some(order_for_poll.limit_price),
                                            update.avg_fill_price,
                                            None,
                                            context,
                                        )
                                        .await;
                                    }

                                    last_status = update.status;
                                    last_filled_qty = update.filled_qty;
                                    last_fill_price = update.avg_fill_price;

                                    if update.status.is_terminal() {
                                        break;
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        warn!(
                            strategy = strategy_label,
                            strategy_id = %strategy_id,
                            error = %e,
                            "strategy action order execution failed"
                        );
                        let update = crate::strategy::OrderUpdate {
                            order_id: client_order_id.clone(),
                            client_order_id: Some(client_order_id.clone()),
                            status: OrderStatus::Failed,
                            filled_qty: 0,
                            avg_fill_price: None,
                            timestamp: Utc::now(),
                            error: Some(e.to_string()),
                        };
                        manager.send_order_update(update.clone());
                        if let Some(store) = runtime_order_store.as_ref() {
                            if let Err(err) = store
                                .update_order_status(&client_order_id, OrderStatus::Failed, None)
                                .await
                            {
                                warn!(
                                    strategy = strategy_label,
                                    strategy_id = %strategy_id,
                                    client_order_id = %client_order_id,
                                    error = %err,
                                    "failed to persist managed runtime order failure"
                                );
                            }
                        }
                        if let Some(pool) = observability_pool.as_ref() {
                            let context = json!({
                                "source": "managed_runtime",
                                "phase": "submit_error",
                                "order_id": update.order_id,
                                "client_order_id": client_order_id.clone(),
                                "status": format!("{:?}", update.status),
                                "filled_qty": update.filled_qty,
                                "avg_fill_price": update.avg_fill_price.map(|p| p.to_string()),
                                "error": update.error,
                            });
                            persist_live_order_signal_history(
                                pool,
                                &observability_account_id,
                                strategy_label,
                                &strategy_id,
                                "live_order_submit_error",
                                Some(order.token_id.as_str()),
                                Some(order.market_side.as_str()),
                                Some(order.limit_price),
                                update.avg_fill_price,
                                context,
                            )
                            .await;
                        }
                        if split_arb_managed {
                            if let Some(pool) = observability_pool.as_ref() {
                                let (leg, mode) = split_arb_leg_and_mode(&client_order_id);
                                let signal_type = format!("split_arb_{}_{}_failed", leg, mode);
                                let context = json!({
                                    "source": "managed_runtime",
                                    "phase": "submit_error",
                                    "order_id": update.order_id,
                                    "client_order_id": client_order_id,
                                    "status": format!("{:?}", update.status),
                                    "filled_qty": update.filled_qty,
                                    "error": update.error,
                                    "leg": leg,
                                    "mode": mode,
                                });
                                persist_split_arb_signal_history(
                                    pool,
                                    &observability_account_id,
                                    strategy_label,
                                    &strategy_id,
                                    &signal_type,
                                    Some(order.token_id.as_str()),
                                    Some(order.market_side.as_str()),
                                    Some(order.limit_price),
                                    None,
                                    None,
                                    context,
                                )
                                .await;
                            }
                        }
                    }
                };
            }
            StrategyAction::CancelOrder { order_id } => match executor.cancel(&order_id).await {
                Ok(cancelled) => {
                    let (status, filled_qty, avg_fill_price, error) = if cancelled {
                        match executor.query_order_status(&order_id).await {
                            Ok(polled) => {
                                let status = if polled.status.is_terminal() {
                                    polled.status
                                } else {
                                    OrderStatus::Cancelled
                                };
                                (status, polled.filled_shares, polled.avg_fill_price, None)
                            }
                            Err(e) => {
                                debug!(
                                    strategy = strategy_label,
                                    strategy_id = %strategy_id,
                                    order_id = %order_id,
                                    error = %e,
                                    "cancel succeeded but post-cancel status query failed"
                                );
                                (OrderStatus::Cancelled, 0, None, None)
                            }
                        }
                    } else {
                        (
                            OrderStatus::Rejected,
                            0,
                            None,
                            Some("order not found or already closed".to_string()),
                        )
                    };
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id: order_id.clone(),
                        client_order_id: None,
                        status,
                        filled_qty,
                        avg_fill_price,
                        timestamp: Utc::now(),
                        error,
                    });
                }
                Err(e) => {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        order_id = %order_id,
                        error = %e,
                        "strategy cancel failed"
                    );
                    manager.send_order_update(crate::strategy::OrderUpdate {
                        order_id,
                        client_order_id: None,
                        status: OrderStatus::Failed,
                        filled_qty: 0,
                        avg_fill_price: None,
                        timestamp: Utc::now(),
                        error: Some(e.to_string()),
                    });
                }
            },
            StrategyAction::ModifyOrder {
                order_id,
                new_price,
                new_size,
            } => {
                warn!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    order_id = %order_id,
                    new_price = ?new_price,
                    new_size = ?new_size,
                    "strategy modify-order action is not implemented"
                );
            }
            StrategyAction::Alert { level, message } => {
                info!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    alert_level = ?level,
                    message = message,
                    "strategy alert"
                );
            }
            StrategyAction::LogEvent { event } => {
                debug!(
                    strategy = strategy_label,
                    strategy_id = %strategy_id,
                    event_type = ?event.event_type,
                    message = event.message,
                    "strategy event"
                );
                if split_arb_managed {
                    if let Some(pool) = observability_pool.as_ref() {
                        let signal_type = split_arb_event_signal_type(&event);
                        let context = json!({
                            "source": "managed_runtime",
                            "phase": "strategy_event",
                            "event_type": format!("{:?}", event.event_type),
                            "message": event.message,
                            "data": event.data,
                            "timestamp": event.timestamp,
                        });
                        persist_split_arb_signal_history(
                            pool,
                            &observability_account_id,
                            strategy_label,
                            &strategy_id,
                            &signal_type,
                            None,
                            None,
                            None,
                            None,
                            None,
                            context,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

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

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!(
                    strategy = strategy_label,
                    agent_id = agent_id,
                    strategy_id = %strategy_id,
                    "managed strategy runtime shutdown requested"
                );
                break;
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(CoordinatorCommand::Pause) => {
                        paused.store(true, Ordering::Relaxed);
                        status = AgentStatus::Paused;
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime paused"
                        );
                    }
                    Some(CoordinatorCommand::Resume) => {
                        paused.store(false, Ordering::Relaxed);
                        status = AgentStatus::Running;
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime resumed"
                        );
                    }
                    Some(CoordinatorCommand::ForceClose) => {
                        warn!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime force-close requested"
                        );
                        break;
                    }
                    Some(CoordinatorCommand::Shutdown) => {
                        info!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime shutdown command received"
                        );
                        break;
                    }
                    Some(CoordinatorCommand::HealthCheck(tx)) => {
                        let position_count = manager
                            .get_strategy_status(&strategy_id)
                            .await
                            .map(|s| s.position_count)
                            .unwrap_or(0);
                        let snapshot = AgentSnapshot {
                            agent_id: agent_id.to_string(),
                            name: strategy_label.to_string(),
                            domain,
                            status,
                            position_count,
                            exposure: rust_decimal::Decimal::ZERO,
                            daily_pnl: rust_decimal::Decimal::ZERO,
                            unrealized_pnl: rust_decimal::Decimal::ZERO,
                            metrics: HashMap::new(),
                            last_heartbeat: Utc::now(),
                            error_message: None,
                        };
                        let uptime_secs = (Utc::now() - started_at).num_seconds().max(0) as u64;
                        let _ = tx.send(AgentHealthResponse {
                            snapshot,
                            is_healthy: matches!(status, AgentStatus::Running | AgentStatus::Paused),
                            uptime_secs,
                            orders_submitted: orders_submitted.load(Ordering::Relaxed),
                            orders_filled: orders_filled.load(Ordering::Relaxed),
                        });
                    }
                    None => {
                        warn!(
                            strategy = strategy_label,
                            agent_id = agent_id,
                            strategy_id = %strategy_id,
                            "managed strategy runtime command channel closed"
                        );
                        break;
                    }
                }
            }
        }
    }

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
