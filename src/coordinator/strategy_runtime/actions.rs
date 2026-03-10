use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::adapters::PostgresStore;
use crate::coordinator::CoordinatorHandle;
use crate::domain::OrderStatus;
use crate::error::PloyError;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{OrderUpdate, StrategyAction, StrategyManager};

mod update_flow;

use super::observability::{
    is_managed_staggered_arb_label, persist_split_arb_signal_history,
    split_arb_event_signal_type,
};
use super::order_store::{
    normalize_runtime_order_request, persist_runtime_order_insert, persist_runtime_order_update,
    RuntimeOrderStore,
};
use update_flow::{handle_runtime_order_update, persist_runtime_observability};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

pub(super) async fn handle_strategy_actions_runtime(
    strategy_label: &str,
    agent_id: &str,
    strategy_id: &str,
    manager: Arc<StrategyManager>,
    mut action_rx: mpsc::Receiver<(String, StrategyAction)>,
    mut order_update_rx: mpsc::Receiver<OrderUpdate>,
    coordinator_handle: CoordinatorHandle,
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
    let split_arb_managed = is_managed_staggered_arb_label(strategy_label);

    loop {
        tokio::select! {
            Some((action_strategy_id, action)) = action_rx.recv() => {
                let runtime_strategy_id = if action_strategy_id.is_empty() {
                    strategy_id
                } else {
                    action_strategy_id.as_str()
                };
                match action {
                    StrategyAction::SubmitIntent { intent } => {
                        handle_submit_intent(
                            strategy_label,
                            agent_id,
                            runtime_strategy_id,
                            &manager,
                            &coordinator_handle,
                            runtime_order_store.as_ref(),
                            paused.as_ref(),
                            orders_submitted.as_ref(),
                            observability_pool.as_ref(),
                            observability_account_id.as_str(),
                            split_arb_managed,
                            intent,
                        )
                        .await;
                    }
                    StrategyAction::CancelOrder { order_id } => {
                        handle_cancel_order(
                            strategy_label,
                            runtime_strategy_id,
                            &manager,
                            executor.as_ref(),
                            order_id,
                        )
                        .await;
                    }
                    StrategyAction::ModifyOrder {
                        order_id,
                        new_price,
                        new_size,
                    } => {
                        warn!(
                            strategy = strategy_label,
                            strategy_id = %runtime_strategy_id,
                            order_id = %order_id,
                            new_price = ?new_price,
                            new_size = ?new_size,
                            "strategy modify-order action is not implemented"
                        );
                    }
                    StrategyAction::Alert { level, message } => {
                        info!(
                            strategy = strategy_label,
                            strategy_id = %runtime_strategy_id,
                            alert_level = ?level,
                            message = message,
                            "strategy alert"
                        );
                    }
                    StrategyAction::LogEvent { event } => {
                        debug!(
                            strategy = strategy_label,
                            strategy_id = %runtime_strategy_id,
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
                                    runtime_strategy_id,
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
            Some(update) = order_update_rx.recv() => {
                handle_runtime_order_update(
                    strategy_label,
                    strategy_id,
                    &manager,
                    runtime_order_store.as_ref(),
                    &executor,
                    &orders_filled,
                    observability_pool.as_ref(),
                    observability_account_id.as_str(),
                    split_arb_managed,
                    update,
                )
                .await;
            }
            else => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_submit_intent(
    strategy_label: &str,
    agent_id: &str,
    strategy_id: &str,
    manager: &Arc<StrategyManager>,
    coordinator_handle: &CoordinatorHandle,
    runtime_order_store: Option<&Arc<dyn RuntimeOrderStore>>,
    paused: &AtomicBool,
    orders_submitted: &AtomicU64,
    observability_pool: Option<&PgPool>,
    observability_account_id: &str,
    split_arb_managed: bool,
    intent: crate::strategy::traits::StrategyOrderIntent,
) {
    let client_order_id = intent.client_order_id.clone();
    let mut order = crate::strategy::order_request_from_intent(&intent);
    normalize_runtime_order_request(&client_order_id, &mut order);

    if paused.load(Ordering::Relaxed) {
        let update = OrderUpdate {
            order_id: client_order_id.clone(),
            client_order_id: Some(client_order_id.clone()),
            status: OrderStatus::Rejected,
            filled_qty: 0,
            avg_fill_price: None,
            timestamp: Utc::now(),
            error: Some("strategy paused by coordinator".to_string()),
        };
        manager.send_order_update(update.clone());
        persist_runtime_failure(
            strategy_label,
            strategy_id,
            runtime_order_store,
            observability_pool,
            observability_account_id,
            split_arb_managed,
            &order,
            &update,
        )
        .await;
        return;
    }

    if let Some(store) = runtime_order_store {
        if let Err(error) = persist_runtime_order_insert(store.as_ref(), strategy_id, &order).await
        {
            warn!(
                strategy = strategy_label,
                strategy_id = %strategy_id,
                client_order_id = %client_order_id,
                error = %error,
                "failed to persist managed runtime order insert"
            );
        }
    }

    let order_intent =
        crate::strategy::runtime_order::order_intent_from_strategy_intent(agent_id, &intent);
    match coordinator_handle.submit_order(order_intent).await {
        Ok(()) => {
            orders_submitted.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) => {
            warn!(
                strategy = strategy_label,
                strategy_id = %strategy_id,
                client_order_id = %client_order_id,
                error = %error,
                "managed runtime submit to coordinator failed"
            );
            let status = if matches!(error, PloyError::Validation(_)) {
                OrderStatus::Rejected
            } else {
                OrderStatus::Failed
            };
            let update = OrderUpdate {
                order_id: client_order_id.clone(),
                client_order_id: Some(client_order_id),
                status,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp: Utc::now(),
                error: Some(error.to_string()),
            };
            manager.send_order_update(update.clone());
            persist_runtime_failure(
                strategy_label,
                strategy_id,
                runtime_order_store,
                observability_pool,
                observability_account_id,
                split_arb_managed,
                &order,
                &update,
            )
            .await;
        }
    }
}

async fn handle_cancel_order(
    strategy_label: &str,
    strategy_id: &str,
    manager: &Arc<StrategyManager>,
    executor: &OrderExecutor,
    order_id: String,
) {
    match executor.cancel(&order_id).await {
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
                    Err(error) => {
                        debug!(
                            strategy = strategy_label,
                            strategy_id = %strategy_id,
                            order_id = %order_id,
                            error = %error,
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
            manager.send_order_update(OrderUpdate {
                order_id: order_id.clone(),
                client_order_id: None,
                status,
                filled_qty,
                avg_fill_price,
                timestamp: Utc::now(),
                error,
            });
        }
        Err(error) => {
            warn!(
                strategy = strategy_label,
                strategy_id = %strategy_id,
                order_id = %order_id,
                error = %error,
                "strategy cancel failed"
            );
            manager.send_order_update(OrderUpdate {
                order_id,
                client_order_id: None,
                status: OrderStatus::Failed,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp: Utc::now(),
                error: Some(error.to_string()),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_runtime_failure(
    strategy_label: &str,
    strategy_id: &str,
    runtime_order_store: Option<&Arc<dyn RuntimeOrderStore>>,
    observability_pool: Option<&PgPool>,
    observability_account_id: &str,
    split_arb_managed: bool,
    order: &crate::domain::OrderRequest,
    update: &OrderUpdate,
) {
    let client_order_id = update
        .client_order_id
        .as_deref()
        .unwrap_or(update.order_id.as_str());
    if let Some(store) = runtime_order_store {
        if let Err(error) = persist_runtime_order_update(
            store.as_ref(),
            client_order_id,
            None,
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
                error = %error,
                "failed to persist managed runtime order failure"
            );
        }
    }

    persist_runtime_observability(
        strategy_label,
        strategy_id,
        observability_pool,
        observability_account_id,
        split_arb_managed,
        Some(order),
        update,
        "submit_error",
    )
    .await;
}
