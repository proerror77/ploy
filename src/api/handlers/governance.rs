use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::api::{auth::ensure_admin_authorized, state::AppState};
use crate::coordinator::{
    GovernancePolicyHistoryEntry, GovernancePolicySnapshot, GovernancePolicyUpdate,
    GovernanceStatusSnapshot,
};
use crate::error::PloyError;

#[derive(Debug, Deserialize)]
pub struct GovernancePolicyUpdateRequest {
    pub block_new_intents: bool,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    pub max_intent_notional_usd: Option<Decimal>,
    pub max_total_notional_usd: Option<Decimal>,
    #[serde(default)]
    pub updated_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GovernancePolicyHistoryQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /api/governance/policy
pub async fn get_governance_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<GovernancePolicySnapshot>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    Ok(Json(coordinator.governance_policy().await))
}

/// GET /api/governance/status
pub async fn get_governance_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<GovernanceStatusSnapshot>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    Ok(Json(coordinator.governance_status().await))
}

/// GET /api/governance/policy/history?limit=100
pub async fn get_governance_policy_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<GovernancePolicyHistoryQuery>,
) -> std::result::Result<Json<Vec<GovernancePolicyHistoryEntry>>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows = coordinator
        .governance_policy_history(limit)
        .await
        .map_err(|e| match e {
            PloyError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        })?;
    Ok(Json(rows))
}

/// PUT /api/governance/policy
pub async fn put_governance_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GovernancePolicyUpdateRequest>,
) -> std::result::Result<Json<GovernancePolicySnapshot>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };

    let update = GovernancePolicyUpdate {
        block_new_intents: req.block_new_intents,
        blocked_domains: req.blocked_domains,
        max_intent_notional_usd: req.max_intent_notional_usd,
        max_total_notional_usd: req.max_total_notional_usd,
        updated_by: req
            .updated_by
            .unwrap_or_else(|| "api.admin".to_string())
            .trim()
            .to_string(),
        reason: req.reason,
        metadata: Default::default(),
    };

    let snapshot = coordinator
        .update_governance_policy(update)
        .await
        .map_err(|e| match e {
            PloyError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        })?;

    Ok(Json(snapshot))
}
