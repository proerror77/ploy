use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tracing::{debug, warn};

use crate::domain::OrderStatus;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{OrderUpdate, StrategyManager};

use super::super::observability::persist_live_order_signal_history;
use super::super::order_store::{
    persist_runtime_order_result, persist_runtime_order_update, RuntimeOrderStore,
};
use super::super::signal_history::{
    persist_split_arb_signal_history, split_arb_leg_and_mode, split_arb_status_key,
};
use super::env_u64;

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_runtime_order_update(
    strategy_label: &str,
    strategy_id: &str,
    manager: &Arc<StrategyManager>,
    runtime_order_store: Option<&Arc<dyn RuntimeOrderStore>>,
    executor: &Arc<OrderExecutor>,
    orders_filled: &Arc<AtomicU64>,
    observability_pool: Option<&PgPool>,
    observability_account_id: &str,
    split_arb_managed: bool,
    update: OrderUpdate,
) {
    if matches!(update.status, OrderStatus::Filled) {
        orders_filled.fetch_add(1, Ordering::Relaxed);
    }

    let client_order_id = update
        .client_order_id
        .clone()
        .unwrap_or_else(|| update.order_id.clone());
    let exchange_order_id = if update.order_id == client_order_id {
        None
    } else {
        Some(update.order_id.as_str())
    };

    manager.send_order_update(update.clone());

    if let Some(store) = runtime_order_store {
        if let Err(error) = persist_runtime_order_update(
            store.as_ref(),
            &client_order_id,
            exchange_order_id,
            update.status,
            update.filled_qty,
            update.avg_fill_price,
        )
        .await
        {
            warn!(
                strategy = strategy_label,
                strategy_id = %strategy_id,
                client_order_id = %client_order_id,
                order_id = %update.order_id,
                error = %error,
                "failed to persist managed runtime coordinator update"
            );
        }
    }

    persist_runtime_observability(
        strategy_label,
        strategy_id,
        observability_pool,
        observability_account_id,
        split_arb_managed,
        None,
        &update,
        "coordinator_update",
    )
    .await;

    if split_arb_managed
        && exchange_order_id.is_some()
        && matches!(
            update.status,
            OrderStatus::Submitted | OrderStatus::PartiallyFilled
        )
    {
        spawn_split_arb_poll_task(
            strategy_label.to_string(),
            strategy_id.to_string(),
            manager.clone(),
            executor.clone(),
            runtime_order_store.cloned(),
            orders_filled.clone(),
            observability_pool.cloned(),
            observability_account_id.to_string(),
            update,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_runtime_observability(
    strategy_label: &str,
    strategy_id: &str,
    observability_pool: Option<&PgPool>,
    observability_account_id: &str,
    split_arb_managed: bool,
    order: Option<&crate::domain::OrderRequest>,
    update: &OrderUpdate,
    phase: &str,
) {
    let Some(pool) = observability_pool else {
        return;
    };

    let token_id = order.map(|order| order.token_id.as_str());
    let market_side = order.map(|order| order.market_side.as_str());
    let limit_price = order.map(|order| order.limit_price);
    let context = json!({
        "source": "managed_runtime",
        "phase": phase,
        "order_id": update.order_id,
        "client_order_id": update.client_order_id.clone(),
        "status": format!("{:?}", update.status),
        "filled_qty": update.filled_qty,
        "avg_fill_price": update.avg_fill_price.map(|price| price.to_string()),
        "error": update.error.clone(),
    });
    let signal_type = if update.error.is_some() {
        "live_order_submit_error"
    } else {
        "live_order_update"
    };
    persist_live_order_signal_history(
        pool,
        observability_account_id,
        strategy_label,
        strategy_id,
        signal_type,
        token_id,
        market_side,
        limit_price,
        update.avg_fill_price,
        context,
    )
    .await;

    if split_arb_managed {
        let client_order_id = update
            .client_order_id
            .as_deref()
            .unwrap_or(update.order_id.as_str());
        let (leg, mode) = split_arb_leg_and_mode(client_order_id);
        let signal_type = if update.error.is_some() {
            format!("split_arb_{}_{}_failed", leg, mode)
        } else {
            let status_key = split_arb_status_key(update.status);
            format!("split_arb_{}_{}_{}", leg, mode, status_key)
        };
        let context = json!({
            "source": "managed_runtime",
            "phase": phase,
            "order_id": update.order_id,
            "client_order_id": update.client_order_id.clone(),
            "status": format!("{:?}", update.status),
            "filled_qty": update.filled_qty,
            "avg_fill_price": update.avg_fill_price.map(|price| price.to_string()),
            "error": update.error.clone(),
            "leg": leg,
            "mode": mode,
        });
        persist_split_arb_signal_history(
            pool,
            observability_account_id,
            strategy_label,
            strategy_id,
            &signal_type,
            token_id,
            market_side,
            limit_price,
            update.avg_fill_price,
            None,
            context,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_split_arb_poll_task(
    strategy_label: String,
    strategy_id: String,
    manager: Arc<StrategyManager>,
    executor: Arc<OrderExecutor>,
    runtime_order_store: Option<Arc<dyn RuntimeOrderStore>>,
    orders_filled: Arc<AtomicU64>,
    observability_pool: Option<PgPool>,
    observability_account_id: String,
    update: OrderUpdate,
) {
    let client_order_id = update
        .client_order_id
        .clone()
        .unwrap_or_else(|| update.order_id.clone());
    let exchange_order_id = update.order_id.clone();
    let mut last_status = update.status;
    let mut last_filled_qty = update.filled_qty;
    let mut last_fill_price = update.avg_fill_price;
    let poll_interval_ms = env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MS", 1500).clamp(200, 10_000);
    let poll_max_ms =
        env_u64("PLOY_MANAGED_STRATEGY_ORDER_POLL_MAX_MS", 600_000).max(poll_interval_ms);

    tokio::spawn(async move {
        let started_at = std::time::Instant::now();
        while started_at.elapsed().as_millis() < poll_max_ms as u128 {
            tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;

            let polled = match executor.query_order_status(&exchange_order_id).await {
                Ok(result) => result,
                Err(error) => {
                    debug!(
                        strategy = strategy_label.as_str(),
                        strategy_id = %strategy_id,
                        client_order_id = %client_order_id,
                        exchange_order_id = %exchange_order_id,
                        error = %error,
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

            if polled.status == OrderStatus::Filled && last_status != OrderStatus::Filled {
                orders_filled.fetch_add(1, Ordering::Relaxed);
            }

            let update = OrderUpdate {
                order_id: polled.order_id,
                client_order_id: Some(client_order_id.clone()),
                status: polled.status,
                filled_qty: polled.filled_shares,
                avg_fill_price: polled.avg_fill_price,
                timestamp: Utc::now(),
                error: None,
            };
            manager.send_order_update(update.clone());

            if let Some(store) = runtime_order_store.as_ref() {
                if let Err(error) = persist_runtime_order_result(
                    store.as_ref(),
                    &client_order_id,
                    &update.order_id,
                    update.status,
                    update.filled_qty,
                    update.avg_fill_price,
                    update.avg_fill_price.unwrap_or_default(),
                )
                .await
                {
                    warn!(
                        strategy = strategy_label.as_str(),
                        strategy_id = %strategy_id,
                        client_order_id = %client_order_id,
                        exchange_order_id = %update.order_id,
                        error = %error,
                        "failed to persist managed runtime poll update"
                    );
                }
            }

            persist_runtime_observability(
                strategy_label.as_str(),
                &strategy_id,
                observability_pool.as_ref(),
                &observability_account_id,
                true,
                None,
                &update,
                "poll",
            )
            .await;

            last_status = update.status;
            last_filled_qty = update.filled_qty;
            last_fill_price = update.avg_fill_price;

            if update.status.is_terminal() {
                break;
            }
        }
    });
}
