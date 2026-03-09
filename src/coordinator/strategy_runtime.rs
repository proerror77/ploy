//! Canonical managed strategy runtime for live strategy execution.
//!
//! This module owns the runtime concerns that previously lived inside
//! `bootstrap.rs`: strategy instantiation, feed wiring, action execution,
//! and managed-runtime observability. `bootstrap` should only assemble and
//! launch this runtime, not re-implement its internals.

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
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
use crate::coordinator::{AgentHealthResponse, AgentSnapshot, CoordinatorCommand};
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::Result;
use crate::platform::{AgentStatus, Domain, PlatformDataPlane};
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{
    DataFeed, DataFeedManager, StrategyAction, StrategyControlAction, StrategyFactory,
    StrategyManager,
};

fn split_arb_status_key(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Submitted => "submitted",
        OrderStatus::PartiallyFilled => "partially_filled",
        OrderStatus::Filled => "filled",
        OrderStatus::Cancelled => "cancelled",
        OrderStatus::Rejected => "rejected",
        OrderStatus::Expired => "expired",
        OrderStatus::Failed => "failed",
    }
}

fn split_arb_leg_and_mode(client_order_id: &str) -> (&'static str, &'static str) {
    if client_order_id.starts_with("stag_leg1_") {
        ("leg1", "entry")
    } else if client_order_id.starts_with("stag_leg2_merge_") {
        ("leg2", "merge")
    } else if client_order_id.starts_with("stag_leg2_forced_") {
        ("leg2", "forced")
    } else if client_order_id.starts_with("stag_leg2_") {
        ("leg2", "unknown")
    } else {
        ("unknown", "unknown")
    }
}

fn split_arb_event_signal_type(event: &crate::strategy::StrategyEvent) -> String {
    match &event.event_type {
        crate::strategy::StrategyEventType::SignalDetected => {
            "split_arb_signal_detected".to_string()
        }
        crate::strategy::StrategyEventType::EntryTriggered => {
            "split_arb_entry_triggered".to_string()
        }
        crate::strategy::StrategyEventType::ExitTriggered => "split_arb_exit_triggered".to_string(),
        crate::strategy::StrategyEventType::OrderFilled => "split_arb_order_filled".to_string(),
        crate::strategy::StrategyEventType::CycleCompleted => {
            "split_arb_cycle_completed".to_string()
        }
        crate::strategy::StrategyEventType::RiskTriggered => "split_arb_risk_triggered".to_string(),
        crate::strategy::StrategyEventType::StateChanged => "split_arb_state_changed".to_string(),
        crate::strategy::StrategyEventType::Error => "split_arb_error".to_string(),
        crate::strategy::StrategyEventType::Custom(name) => {
            let sanitized: String = name
                .trim()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("split_arb_custom_{}", sanitized)
        }
    }
}

async fn persist_split_arb_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_name: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    fair_value: Option<Decimal>,
    market_price: Option<Decimal>,
    edge: Option<Decimal>,
    context: serde_json::Value,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO signal_history (
            account_id, intent_id, agent_id, strategy_id, domain, signal_type,
            market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
        )
        VALUES (
            $1, NULL, $2, $3, 'crypto', $4,
            NULL, $5, NULL, $6, NULL, $7, $8, $9, NULL, $10
        )
        "#,
    )
    .bind(account_id)
    .bind(strategy_name)
    .bind(strategy_id)
    .bind(signal_type)
    .bind(token_id)
    .bind(side)
    .bind(fair_value)
    .bind(market_price)
    .bind(edge)
    .bind(sqlx::types::Json(context))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            strategy = strategy_name,
            strategy_id = strategy_id,
            signal_type = signal_type,
            error = %e,
            "failed to persist managed staggered_arb signal_history observation"
        );
    }
}

fn is_managed_staggered_arb_label(strategy_label: &str) -> bool {
    matches!(strategy_label, "split_arb" | "staggered_arb")
}

async fn persist_live_order_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_label: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    order_price: Option<Decimal>,
    fill_price: Option<Decimal>,
    context: serde_json::Value,
) {
    let agent_id = format!("{}_runtime", strategy_label);
    let result = sqlx::query(
        r#"
        INSERT INTO signal_history (
            account_id, intent_id, agent_id, strategy_id, domain, signal_type,
            market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
        )
        VALUES (
            $1, NULL, $2, $3, 'strategy_runtime', $4,
            NULL, $5, NULL, $6, NULL, $7, $8, NULL, NULL, $9
        )
        "#,
    )
    .bind(account_id)
    .bind(agent_id)
    .bind(strategy_id)
    .bind(signal_type)
    .bind(token_id)
    .bind(side)
    .bind(order_price)
    .bind(fill_price)
    .bind(sqlx::types::Json(context))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            strategy = strategy_label,
            strategy_id = strategy_id,
            signal_type = signal_type,
            error = %e,
            "failed to persist live order signal_history observation"
        );
    }
}

#[async_trait]
pub(crate) trait RuntimeOrderStore: Send + Sync {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()>;
    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()>;
    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()>;
}

#[async_trait]
impl RuntimeOrderStore for PostgresStore {
    async fn insert_order(&self, order: &crate::domain::Order) -> Result<()> {
        PostgresStore::insert_order(self, order).await.map(|_| ())
    }

    async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()> {
        PostgresStore::update_order_status(self, client_order_id, status, exchange_order_id).await
    }

    async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()> {
        PostgresStore::update_order_fill(
            self,
            client_order_id,
            filled_shares,
            avg_fill_price,
            status,
        )
        .await
    }
}

pub(crate) fn normalize_runtime_order_request(client_order_id: &str, order: &mut OrderRequest) {
    if order.client_order_id != client_order_id {
        warn!(
            "Mismatched order IDs in managed runtime action: action={}, request={}; using action ID",
            client_order_id, order.client_order_id
        );
        order.client_order_id = client_order_id.to_string();
    }
    let missing_idempotency = order
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty);
    if missing_idempotency {
        order.idempotency_key = Some(client_order_id.to_string());
    }
}

fn runtime_order_leg(client_order_id: &str) -> u8 {
    if client_order_id.contains("leg2") {
        2
    } else {
        1
    }
}

pub(crate) async fn persist_runtime_order_insert(
    store: &dyn RuntimeOrderStore,
    strategy_id: &str,
    order: &OrderRequest,
) -> Result<()> {
    let db_order = crate::domain::Order::from_request(
        order,
        None,
        runtime_order_leg(&order.client_order_id),
        Some(strategy_id.to_string()),
    );
    store.insert_order(&db_order).await
}

pub(crate) async fn persist_runtime_order_result(
    store: &dyn RuntimeOrderStore,
    client_order_id: &str,
    exchange_order_id: &str,
    status: OrderStatus,
    filled_shares: u64,
    avg_fill_price: Option<Decimal>,
    fallback_price: Decimal,
) -> Result<()> {
    store
        .update_order_status(
            client_order_id,
            OrderStatus::Submitted,
            Some(exchange_order_id),
        )
        .await?;

    if filled_shares > 0 {
        store
            .update_order_fill(
                client_order_id,
                filled_shares,
                avg_fill_price.unwrap_or(fallback_price),
                status,
            )
            .await?;
    } else if status != OrderStatus::Submitted {
        store
            .update_order_status(client_order_id, status, Some(exchange_order_id))
            .await?;
    }

    Ok(())
}

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
            submit_action @ (StrategyAction::SubmitIntent { .. }
            | StrategyAction::SubmitOrder { .. }) => {
                let (client_order_id, mut order, _priority) = submit_action
                    .into_submit_order()
                    .expect("submit action should normalize");
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
            StrategyAction::LegacyControl(control) => match control {
                StrategyControlAction::UpdateRisk { level, reason } => {
                    info!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        risk_level = ?level,
                        reason = reason,
                        "legacy strategy risk update"
                    );
                }
                StrategyControlAction::SubscribeFeed { feed } => {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        feed = ?feed,
                        "legacy dynamic subscribe-feed action is not implemented in managed runtime"
                    );
                }
                StrategyControlAction::UnsubscribeFeed { feed } => {
                    warn!(
                        strategy = strategy_label,
                        strategy_id = %strategy_id,
                        feed = ?feed,
                        "legacy dynamic unsubscribe-feed action is not implemented in managed runtime"
                    );
                }
            },
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
            if let Err(e) = super::bootstrap::ensure_strategy_observability_tables(pool).await {
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

#[cfg(test)]
mod tests {
    use super::{
        normalize_runtime_order_request, persist_runtime_order_insert,
        persist_runtime_order_result, RuntimeOrderStore,
    };
    use crate::domain::{OrderRequest, OrderStatus, Side};
    use crate::error::Result;
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockRuntimeOrderStore {
        inserted: Mutex<Vec<crate::domain::Order>>,
        status_updates: Mutex<Vec<(String, OrderStatus, Option<String>)>>,
        fill_updates: Mutex<Vec<(String, u64, rust_decimal::Decimal, OrderStatus)>>,
    }

    #[async_trait]
    impl RuntimeOrderStore for MockRuntimeOrderStore {
        async fn insert_order(&self, order: &crate::domain::Order) -> Result<()> {
            self.inserted
                .lock()
                .expect("inserted lock")
                .push(order.clone());
            Ok(())
        }

        async fn update_order_status(
            &self,
            client_order_id: &str,
            status: OrderStatus,
            exchange_order_id: Option<&str>,
        ) -> Result<()> {
            self.status_updates
                .lock()
                .expect("status_updates lock")
                .push((
                    client_order_id.to_string(),
                    status,
                    exchange_order_id.map(str::to_string),
                ));
            Ok(())
        }

        async fn update_order_fill(
            &self,
            client_order_id: &str,
            filled_shares: u64,
            avg_fill_price: rust_decimal::Decimal,
            status: OrderStatus,
        ) -> Result<()> {
            self.fill_updates.lock().expect("fill_updates lock").push((
                client_order_id.to_string(),
                filled_shares,
                avg_fill_price,
                status,
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn persist_runtime_order_insert_uses_action_order_id_and_leg() {
        let store = Arc::new(MockRuntimeOrderStore::default());
        let mut order = OrderRequest::buy_limit("token-1".to_string(), Side::Down, 20, dec!(0.55));

        normalize_runtime_order_request("stag_leg2_merge_123", &mut order);
        persist_runtime_order_insert(store.as_ref(), "staggered_arb_strategy", &order)
            .await
            .expect("insert should succeed");

        let inserted = store.inserted.lock().expect("inserted lock");
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].client_order_id, "stag_leg2_merge_123");
        assert_eq!(inserted[0].leg, 2);
        assert_eq!(
            inserted[0].strategy_id.as_deref(),
            Some("staggered_arb_strategy")
        );
    }

    #[test]
    fn normalize_runtime_order_request_sets_idempotency_key_from_action_id() {
        let mut order = OrderRequest::buy_limit("token-1".to_string(), Side::Up, 20, dec!(0.40));
        order.client_order_id = "mismatched".to_string();
        order.idempotency_key = None;

        normalize_runtime_order_request("stag_leg1_123", &mut order);

        assert_eq!(order.client_order_id, "stag_leg1_123");
        assert_eq!(order.idempotency_key.as_deref(), Some("stag_leg1_123"));
    }

    #[tokio::test]
    async fn persist_runtime_order_result_records_submission_and_fill() {
        let store = Arc::new(MockRuntimeOrderStore::default());

        persist_runtime_order_result(
            store.as_ref(),
            "stag_leg1_123",
            "exchange-123",
            OrderStatus::Filled,
            20,
            Some(dec!(0.34)),
            dec!(0.40),
        )
        .await
        .expect("persist result should succeed");

        let status_updates = store.status_updates.lock().expect("status_updates lock");
        assert_eq!(status_updates.len(), 1);
        assert_eq!(
            status_updates[0],
            (
                "stag_leg1_123".to_string(),
                OrderStatus::Submitted,
                Some("exchange-123".to_string())
            )
        );
        drop(status_updates);

        let fill_updates = store.fill_updates.lock().expect("fill_updates lock");
        assert_eq!(fill_updates.len(), 1);
        assert_eq!(
            fill_updates[0],
            (
                "stag_leg1_123".to_string(),
                20,
                dec!(0.34),
                OrderStatus::Filled
            )
        );
    }
}
