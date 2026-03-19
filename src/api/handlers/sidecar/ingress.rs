use axum::http::StatusCode;
use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;

use crate::api::{
    state::AppState,
    types::{MarketData, PositionResponse, TradeResponse, WsMessage},
};
use crate::control_plane::{MarketSelector, StrategyDeployment};
use crate::domain::market::Side;
use crate::error::PloyError;
use crate::platform::{Domain, OrderPriority};

mod deployment_gate;

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn normalize_account(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn request_metadata_account(metadata: &HashMap<String, String>) -> Option<String> {
    metadata
        .get("account_id")
        .or_else(|| metadata.get("account"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

pub(crate) async fn table_has_account_scope(pool: &sqlx::PgPool, table_name: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = $1
          AND column_name = 'account_id'
        LIMIT 1
        "#,
    )
    .bind(table_name)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .is_some()
}

pub(crate) fn resolve_request_account_scope(
    explicit: Option<&str>,
    metadata: Option<&HashMap<String, String>>,
) -> std::result::Result<Option<String>, (StatusCode, String)> {
    let explicit = normalize_account(explicit);
    let metadata = metadata.and_then(request_metadata_account);

    if let (Some(e), Some(m)) = (explicit.as_ref(), metadata.as_ref()) {
        if !e.eq_ignore_ascii_case(m) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "account_id mismatch between request field ({}) and metadata ({})",
                    e, m
                ),
            ));
        }
    }

    Ok(explicit.or(metadata))
}

pub(crate) fn validate_account_scope(
    state: &AppState,
    requested_account: Option<&str>,
) -> std::result::Result<(), (StatusCode, String)> {
    let runtime_account = state.account_id.trim();
    if let Some(account) = requested_account
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
    {
        if !account.eq_ignore_ascii_case(runtime_account) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "account scope mismatch: runtime account is {}, request account is {}",
                    runtime_account, account
                ),
            ));
        }
    }
    Ok(())
}

fn env_bool(keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|v| parse_boolish(&v))
        .unwrap_or(false)
}

fn deployment_gate_required() -> bool {
    match std::env::var("PLOY_DEPLOYMENT_GATE_REQUIRED")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
    {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "no" | "off"),
        None => true,
    }
}

pub(crate) fn ensure_domain_allowed(
    state: &AppState,
    domain: Domain,
    reason: &str,
) -> std::result::Result<(), (StatusCode, String)> {
    if state.is_domain_allowed(domain) {
        return Ok(());
    }
    let allowed = state.allowed_domains_labels().join(", ");
    Err((
        StatusCode::CONFLICT,
        format!(
            "{} is not enabled for runtime scope (requested={}, allowed=[{}])",
            reason, domain, allowed
        ),
    ))
}

fn allow_non_live_deployment_ingress() -> bool {
    env_bool(&["PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS"])
}

pub(crate) fn ensure_deployment_accepts_live_ingress(
    deployment: &StrategyDeployment,
) -> std::result::Result<(), (StatusCode, String)> {
    if allow_non_live_deployment_ingress() {
        return Ok(());
    }
    if deployment.lifecycle_stage.allows_live_ingress() {
        return Ok(());
    }

    Err((
        StatusCode::CONFLICT,
        format!(
            "deployment {} lifecycle_stage={} does not allow live ingress",
            deployment.id,
            deployment.lifecycle_stage.as_str()
        ),
    ))
}

pub(crate) async fn ensure_agent_authorized(
    state: &AppState,
    agent_id: &str,
) -> std::result::Result<(), (StatusCode, String)> {
    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;
    if coordinator.is_agent_authorized(agent_id).await {
        return Ok(());
    }
    Err((
        StatusCode::CONFLICT,
        format!("agent_id '{}' is not registered/authorized", agent_id),
    ))
}

pub(crate) async fn resolve_intent_deployment(
    state: &AppState,
    deployment_id: &str,
) -> std::result::Result<Option<StrategyDeployment>, (StatusCode, String)> {
    let key = deployment_id.trim();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment_id is required".to_string(),
        ));
    }

    let deployments = state.deployments.read().await;
    if deployments.is_empty() {
        if deployment_gate_required() {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "deployment registry is empty while deployment gate is required".to_string(),
            ));
        }
        return Ok(None);
    }

    let Some(dep) = deployments.get(key) else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown deployment_id: {}", key),
        ));
    };
    if !dep.enabled {
        return Err((
            StatusCode::CONFLICT,
            format!("deployment {} is disabled", key),
        ));
    }
    if !dep.matches_account(state.account_id.as_str()) {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "deployment {} is not scoped to runtime account {}",
                key, state.account_id
            ),
        ));
    }
    if !dep.matches_execution_mode(state.dry_run) {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "deployment {} does not match runtime mode (dry_run={})",
                key, state.dry_run
            ),
        ));
    }
    ensure_domain_allowed(
        state,
        dep.domain,
        &format!("deployment {}", dep.id.as_str()),
    )?;
    Ok(Some(dep.clone()))
}

pub(crate) fn sidecar_orders_live_enabled() -> bool {
    env_bool(&["PLOY_SIDECAR_ORDERS_LIVE_ENABLED"])
}

fn normalize_opt(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

fn normalize_meta<'a>(metadata: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| metadata.get(*k))
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub(crate) fn validate_deployment_binding(
    deployment: &StrategyDeployment,
    domain: Domain,
    market_slug: &str,
    metadata: &HashMap<String, String>,
) -> std::result::Result<(), (StatusCode, String)> {
    if deployment.domain != domain {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "deployment {} is bound to domain {}, but request domain is {}",
                deployment.id, deployment.domain, domain
            ),
        ));
    }

    if let Some(tf) = normalize_meta(metadata, &["timeframe"]) {
        let expected = deployment.timeframe.as_str();
        if !tf.eq_ignore_ascii_case(expected) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "deployment {} timeframe mismatch: expected {}, got {}",
                    deployment.id, expected, tf
                ),
            ));
        }
    }

    match &deployment.market_selector {
        MarketSelector::Static {
            symbol,
            series_id,
            market_slug: expected_market_slug,
        } => {
            if let Some(expected_slug) = normalize_opt(expected_market_slug) {
                if !market_slug.eq_ignore_ascii_case(expected_slug) {
                    return Err((
                        StatusCode::CONFLICT,
                        format!(
                            "deployment {} market mismatch: expected {}, got {}",
                            deployment.id, expected_slug, market_slug
                        ),
                    ));
                }
            }
            if let Some(expected_symbol) = normalize_opt(symbol) {
                if let Some(actual_symbol) = normalize_meta(metadata, &["symbol"]) {
                    if !actual_symbol.eq_ignore_ascii_case(expected_symbol) {
                        return Err((
                            StatusCode::CONFLICT,
                            format!(
                                "deployment {} symbol mismatch: expected {}, got {}",
                                deployment.id, expected_symbol, actual_symbol
                            ),
                        ));
                    }
                }
            }
            if let Some(expected_series_id) = normalize_opt(series_id) {
                if let Some(actual_series_id) =
                    normalize_meta(metadata, &["series_id", "event_series_id"])
                {
                    if !actual_series_id.eq_ignore_ascii_case(expected_series_id) {
                        return Err((
                            StatusCode::CONFLICT,
                            format!(
                                "deployment {} series mismatch: expected {}, got {}",
                                deployment.id, expected_series_id, actual_series_id
                            ),
                        ));
                    }
                }
            }
        }
        MarketSelector::Dynamic {
            domain: selector_domain,
            ..
        } => {
            if *selector_domain != domain {
                return Err((
                    StatusCode::CONFLICT,
                    format!(
                        "deployment {} dynamic selector domain mismatch: expected {}, got {}",
                        deployment.id, selector_domain, domain
                    ),
                ));
            }
        }
    }

    Ok(())
}

pub(crate) fn apply_deployment_metadata(
    metadata: &mut HashMap<String, String>,
    deployment: &StrategyDeployment,
) {
    metadata.insert("deployment_id".to_string(), deployment.id.clone());
    metadata
        .entry("timeframe".to_string())
        .or_insert_with(|| deployment.timeframe.as_str().to_string());
    metadata
        .entry("allocator_profile".to_string())
        .or_insert_with(|| deployment.allocator_profile.clone());
    metadata
        .entry("risk_profile".to_string())
        .or_insert_with(|| deployment.risk_profile.clone());
    metadata
        .entry("deployment_strategy".to_string())
        .or_insert_with(|| deployment.strategy.clone());
    metadata
        .entry("strategy_version".to_string())
        .or_insert_with(|| deployment.strategy_version.clone());
    metadata
        .entry("lifecycle_stage".to_string())
        .or_insert_with(|| deployment.lifecycle_stage.as_str().to_string());
    metadata
        .entry("product_type".to_string())
        .or_insert_with(|| deployment.product_type.as_str().to_string());
    metadata
        .entry("deployment_priority".to_string())
        .or_insert_with(|| deployment.priority.to_string());
    metadata
        .entry("deployment_cooldown_secs".to_string())
        .or_insert_with(|| deployment.cooldown_secs.to_string());
    if let Some(ts) = deployment.last_evaluated_at.as_ref() {
        metadata
            .entry("last_evaluated_at".to_string())
            .or_insert_with(|| ts.to_rfc3339());
    }
    if let Some(score) = deployment.last_evaluation_score {
        metadata
            .entry("last_evaluation_score".to_string())
            .or_insert_with(|| score.to_string());
    }

    if let MarketSelector::Static {
        symbol,
        series_id,
        market_slug,
    } = &deployment.market_selector
    {
        if let Some(v) = normalize_opt(symbol) {
            metadata
                .entry("symbol".to_string())
                .or_insert_with(|| v.to_string());
        }
        if let Some(v) = normalize_opt(series_id) {
            metadata
                .entry("series_id".to_string())
                .or_insert_with(|| v.to_string());
            metadata
                .entry("event_series_id".to_string())
                .or_insert_with(|| v.to_string());
        }
        if let Some(v) = normalize_opt(market_slug) {
            metadata
                .entry("selector_market_slug".to_string())
                .or_insert_with(|| v.to_string());
        }
    }
}

pub(crate) fn deployment_default_priority(deployment: &StrategyDeployment) -> OrderPriority {
    match deployment.priority {
        p if p >= 90 => OrderPriority::Critical,
        p if p >= 70 => OrderPriority::High,
        p if p <= 20 => OrderPriority::Low,
        _ => OrderPriority::Normal,
    }
}

fn external_critical_priority_allowed() -> bool {
    env_bool(&["PLOY_ALLOW_EXTERNAL_CRITICAL_PRIORITY"])
}

pub(crate) fn clamp_external_priority(priority: OrderPriority) -> OrderPriority {
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

pub(crate) fn broadcast_sidecar_activity(
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

pub(crate) fn parse_sidecar_domain(
    raw: Option<&str>,
    default_domain: Domain,
) -> std::result::Result<Domain, (StatusCode, String)> {
    Domain::parse_optional(raw, default_domain).map_err(|msg| (StatusCode::BAD_REQUEST, msg))
}

pub(crate) fn parse_binary_side(
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

pub(crate) fn parse_is_buy(
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

pub(crate) fn parse_order_priority(
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

pub(crate) fn map_coordinator_submit_error(prefix: &str, err: PloyError) -> (StatusCode, String) {
    match err {
        PloyError::Validation(msg) => (StatusCode::CONFLICT, format!("{}: {}", prefix, msg)),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {}", prefix, other),
        ),
    }
}
