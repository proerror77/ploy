use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tracing::warn;

use crate::domain::OrderStatus;
use crate::strategy::{StrategyEvent, StrategyEventType};

pub(super) fn split_arb_status_key(status: OrderStatus) -> &'static str {
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

pub(super) fn split_arb_leg_and_mode(client_order_id: &str) -> (&'static str, &'static str) {
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

pub(super) fn split_arb_event_signal_type(event: &StrategyEvent) -> String {
    match &event.event_type {
        StrategyEventType::SignalDetected => "split_arb_signal_detected".to_string(),
        StrategyEventType::EntryTriggered => "split_arb_entry_triggered".to_string(),
        StrategyEventType::ExitTriggered => "split_arb_exit_triggered".to_string(),
        StrategyEventType::OrderFilled => "split_arb_order_filled".to_string(),
        StrategyEventType::CycleCompleted => "split_arb_cycle_completed".to_string(),
        StrategyEventType::RiskTriggered => "split_arb_risk_triggered".to_string(),
        StrategyEventType::StateChanged => "split_arb_state_changed".to_string(),
        StrategyEventType::Error => "split_arb_error".to_string(),
        StrategyEventType::Custom(name) => {
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

pub(super) fn is_managed_staggered_arb_label(strategy_label: &str) -> bool {
    matches!(strategy_label, "split_arb" | "staggered_arb")
}

pub(super) async fn persist_split_arb_signal_history(
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
    context: Value,
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

pub(super) async fn persist_live_order_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_label: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    order_price: Option<Decimal>,
    fill_price: Option<Decimal>,
    context: Value,
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
