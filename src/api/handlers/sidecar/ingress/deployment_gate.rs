use axum::http::StatusCode;
use std::collections::HashMap;

use crate::api::state::AppState;
use crate::control_plane::{MarketSelector, StrategyDeployment};
use crate::coordinator::OrderPriority;
use crate::domain::Domain;

use super::env_bool;

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

fn allow_non_live_deployment_ingress() -> bool {
    env_bool(&["PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS"])
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

pub(super) async fn table_has_account_scope(pool: &sqlx::PgPool, table_name: &str) -> bool {
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

pub(super) fn resolve_request_account_scope(
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

pub(super) fn validate_account_scope(
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

pub(super) fn ensure_domain_allowed(
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

pub(super) fn ensure_deployment_accepts_live_ingress(
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

pub(super) async fn ensure_agent_authorized(
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

pub(super) async fn resolve_intent_deployment(
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

pub(super) fn validate_deployment_binding(
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

pub(super) fn apply_deployment_metadata(
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

pub(super) fn deployment_default_priority(deployment: &StrategyDeployment) -> OrderPriority {
    match deployment.priority {
        p if p >= 90 => OrderPriority::Critical,
        p if p >= 70 => OrderPriority::High,
        p if p <= 20 => OrderPriority::Low,
        _ => OrderPriority::Normal,
    }
}
