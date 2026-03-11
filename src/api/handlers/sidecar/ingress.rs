use axum::http::StatusCode;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::api::{
    state::AppState,
    types::{MarketData, PositionResponse, TradeResponse, WsMessage},
};
use crate::domain::market::Side;
use crate::error::PloyError;
use crate::coordinator::OrderPriority;
use crate::domain::Domain;

mod deployment_gate;

pub(super) use deployment_gate::{
    apply_deployment_metadata, deployment_default_priority, ensure_agent_authorized,
    ensure_deployment_accepts_live_ingress, ensure_domain_allowed, resolve_intent_deployment,
    resolve_request_account_scope, table_has_account_scope, validate_account_scope,
    validate_deployment_binding,
};

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn env_bool(keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|v| parse_boolish(&v))
        .unwrap_or(false)
}

pub(super) fn sidecar_orders_live_enabled() -> bool {
    env_bool(&["PLOY_SIDECAR_ORDERS_LIVE_ENABLED"])
}

fn external_critical_priority_allowed() -> bool {
    env_bool(&["PLOY_ALLOW_EXTERNAL_CRITICAL_PRIORITY"])
}

pub(super) fn clamp_external_priority(priority: OrderPriority) -> OrderPriority {
    if priority == OrderPriority::Critical && !external_critical_priority_allowed() {
        return OrderPriority::High;
    }
    priority
}

fn side_to_label(side: Side) -> String {
    match side {
        Side::Up => "UP".to_string(),
        Side::Down => "DOWN".to_string(),
    }
}

pub(super) fn broadcast_sidecar_activity(
    state: &AppState,
    intent_id: &str,
    market_slug: &str,
    token_id: &str,
    side: Side,
    shares: u64,
    price: Decimal,
) {
    let now = Utc::now();
    let side_label = side_to_label(side);
    let shares_i32 = i32::try_from(shares).unwrap_or(i32::MAX);
    let price_f64 = price.to_f64().unwrap_or_default();

    state.broadcast(WsMessage::Trade(TradeResponse {
        id: intent_id.to_string(),
        timestamp: now,
        token_id: token_id.to_string(),
        token_name: market_slug.to_string(),
        side: side_label.clone(),
        shares: shares_i32,
        entry_price: price_f64,
        exit_price: None,
        pnl: None,
        status: "PENDING".to_string(),
        error_message: None,
    }));

    state.broadcast(WsMessage::Position(PositionResponse {
        token_id: token_id.to_string(),
        token_name: market_slug.to_string(),
        side: side_label,
        shares: shares_i32,
        entry_price: price_f64,
        current_price: price_f64,
        unrealized_pnl: 0.0,
        entry_time: now,
        duration_seconds: 0,
    }));

    state.broadcast(WsMessage::Market(MarketData {
        token_id: token_id.to_string(),
        token_name: market_slug.to_string(),
        best_bid: price_f64,
        best_ask: price_f64,
        spread: 0.0,
        last_price: price_f64,
        volume_24h: 0.0,
        timestamp: now,
    }));
}

pub(super) fn parse_sidecar_domain(
    raw: Option<&str>,
    default_domain: Domain,
) -> std::result::Result<Domain, (StatusCode, String)> {
    Domain::parse_optional(raw, default_domain).map_err(|msg| (StatusCode::BAD_REQUEST, msg))
}

pub(super) fn parse_binary_side(
    raw: Option<&str>,
) -> std::result::Result<Side, (StatusCode, String)> {
    let Some(raw) = raw else {
        return Ok(Side::Up);
    };
    match raw.trim().to_ascii_uppercase().as_str() {
        "UP" | "YES" => Ok(Side::Up),
        "DOWN" | "NO" => Ok(Side::Down),
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("invalid side '{}', expected UP|DOWN|YES|NO", other),
        )),
    }
}

pub(super) fn parse_is_buy(
    order_side: Option<&str>,
    is_buy: Option<bool>,
) -> std::result::Result<bool, (StatusCode, String)> {
    let parsed_order_side = match order_side.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match raw.to_ascii_uppercase().as_str() {
            "BUY" => Some(true),
            "SELL" => Some(false),
            other => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("invalid order_side '{}', expected BUY|SELL", other),
                ));
            }
        },
        None => None,
    };

    if let Some(v) = is_buy {
        if let Some(side_bool) = parsed_order_side {
            if side_bool != v {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "order_side conflicts with is_buy".to_string(),
                ));
            }
        }
        return Ok(v);
    }

    Ok(parsed_order_side.unwrap_or(true))
}

pub(super) fn parse_order_priority(
    raw: Option<&str>,
) -> std::result::Result<OrderPriority, (StatusCode, String)> {
    match raw.unwrap_or("normal").trim().to_ascii_lowercase().as_str() {
        "critical" if external_critical_priority_allowed() => Ok(OrderPriority::Critical),
        "critical" => Err((
            StatusCode::BAD_REQUEST,
            "critical priority is disabled for external sidecar requests".to_string(),
        )),
        "high" => Ok(OrderPriority::High),
        "normal" => Ok(OrderPriority::Normal),
        "low" => Ok(OrderPriority::Low),
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("invalid priority '{}', expected high|normal|low", other),
        )),
    }
}

pub(super) fn map_coordinator_submit_error(prefix: &str, err: PloyError) -> (StatusCode, String) {
    match err {
        PloyError::Validation(msg) => (StatusCode::CONFLICT, format!("{}: {}", prefix, msg)),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {}", prefix, other),
        ),
    }
}
