use serde_json::json;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::adapters::PostgresStore;
use crate::coordinator::CoordinatorHandle;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{OrderUpdate, StrategyAction, StrategyManager};

mod order_commands;
mod update_flow;

use super::signal_history::{
    is_managed_staggered_arb_label, persist_split_arb_signal_history,
    split_arb_event_signal_type,
};
use super::order_store::RuntimeOrderStore;
use order_commands::{handle_cancel_order, handle_submit_intent};
use update_flow::handle_runtime_order_update;

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
    runtime_alive: Arc<AtomicBool>,
    orders_submitted: Arc<AtomicU64>,
    orders_filled: Arc<AtomicU64>,
    split_arb_poll_registry: Arc<Mutex<HashSet<String>>>,
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
                    &runtime_alive,
                    &orders_filled,
                    &split_arb_poll_registry,
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
