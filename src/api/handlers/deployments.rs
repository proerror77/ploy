use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::api::{auth::ensure_admin_authorized, state::AppState};
use crate::platform::StrategyDeployment;

#[derive(Debug, Deserialize)]
pub struct UpsertDeploymentsRequest {
    pub deployments: Vec<StrategyDeployment>,
    #[serde(default)]
    pub replace: bool,
}

#[derive(Debug, Serialize)]
pub struct UpsertDeploymentsResponse {
    pub success: bool,
    pub upserted: usize,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct DeploymentMutationResponse {
    pub success: bool,
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct DeploymentDeleteResponse {
    pub success: bool,
    pub id: String,
}

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn evidence_gate_required() -> bool {
    std::env::var("PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE")
        .ok()
        .map(|v| parse_boolish(&v))
        .unwrap_or(false)
}

fn required_evidence_stages() -> Vec<String> {
    let raw = std::env::var("PLOY_DEPLOYMENTS_REQUIRED_STAGES")
        .unwrap_or_else(|_| "backtest,paper".to_string());
    let mut out = Vec::new();
    for token in raw.split(',') {
        let stage = token.trim().to_ascii_lowercase();
        let normalized = match stage.as_str() {
            "backtest" => Some("BACKTEST"),
            "paper" => Some("PAPER"),
            "live" => Some("LIVE"),
            _ => None,
        };
        if let Some(stage) = normalized {
            if !out.iter().any(|v: &String| v == stage) {
                out.push(stage.to_string());
            }
        }
    }
    out
}

fn max_evidence_age_hours() -> i64 {
    std::env::var("PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(168)
}

async fn ensure_required_strategy_evidence(
    state: &AppState,
    deployment: &StrategyDeployment,
) -> std::result::Result<(), (StatusCode, String)> {
    if !evidence_gate_required() {
        return Ok(());
    }
    if !deployment.matches_account(state.account_id.as_str())
        || !deployment.matches_execution_mode(state.dry_run)
    {
        return Ok(());
    }

    let required_stages = required_evidence_stages();
    if required_stages.is_empty() {
        return Ok(());
    }
    let max_age = ChronoDuration::hours(max_evidence_age_hours());
    let deployment_domain = deployment.domain.to_string();
    let deployment_timeframe = deployment.timeframe.as_str().to_string();

    for stage in required_stages {
        let row = sqlx::query(
            r#"
            SELECT
                status,
                evaluated_at,
                NULLIF(BTRIM(evidence_ref), '') AS evidence_ref,
                NULLIF(BTRIM(evidence_hash), '') AS evidence_hash,
                (evidence_payload IS NOT NULL) AS has_payload
            FROM strategy_evaluations
            WHERE account_id = $1
              AND stage = $2
              AND (
                    deployment_id = $3
                 OR (
                        strategy_id = $4
                    AND UPPER(domain) = UPPER($5)
                    AND COALESCE(NULLIF(BTRIM(timeframe), ''), '__none__')
                        = COALESCE(NULLIF(BTRIM($6), ''), '__none__')
                 )
              )
            ORDER BY evaluated_at DESC
            LIMIT 1
            "#,
        )
        .bind(state.account_id.as_str())
        .bind(stage.as_str())
        .bind(deployment.id.as_str())
        .bind(deployment.strategy.as_str())
        .bind(deployment_domain.as_str())
        .bind(deployment_timeframe.as_str())
        .fetch_optional(state.store.pool())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to validate strategy evidence: {}", e),
            )
        })?;

        let Some(row) = row else {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "deployment '{}' cannot be enabled: missing {} evidence for strategy '{}'",
                    deployment.id, stage, deployment.strategy
                ),
            ));
        };

        let status: String = row.try_get("status").unwrap_or_default();
        let evaluated_at = row
            .try_get::<chrono::DateTime<Utc>, _>("evaluated_at")
            .unwrap_or_else(|_| Utc::now());
        let evidence_ref = row
            .try_get::<Option<String>, _>("evidence_ref")
            .ok()
            .flatten();
        let evidence_hash = row
            .try_get::<Option<String>, _>("evidence_hash")
            .ok()
            .flatten();
        let has_payload = row.try_get::<bool, _>("has_payload").unwrap_or(false);
        if !status.eq_ignore_ascii_case("PASS") {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "deployment '{}' cannot be enabled: latest {} evidence status is {} for strategy '{}'",
                    deployment.id, stage, status, deployment.strategy
                ),
            ));
        }
        let has_ref = evidence_ref
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_hash = evidence_hash
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if !(has_ref || has_hash || has_payload) {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "deployment '{}' cannot be enabled: latest {} evidence is missing traceable artifacts (evidence_ref/evidence_hash/evidence_payload)",
                    deployment.id, stage
                ),
            ));
        }
        if Utc::now().signed_duration_since(evaluated_at) > max_age {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "deployment '{}' cannot be enabled: latest {} evidence is stale ({})",
                    deployment.id,
                    stage,
                    evaluated_at.to_rfc3339()
                ),
            ));
        }
    }

    Ok(())
}

/// GET /api/deployments
pub async fn list_deployments(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Vec<StrategyDeployment>>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let deployments = state.deployments.read().await;
    let mut items: Vec<StrategyDeployment> = deployments.values().cloned().collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(items))
}

/// GET /api/deployments/:id
pub async fn get_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<StrategyDeployment>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let deployments = state.deployments.read().await;
    let key = id.trim();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment id is required".to_string(),
        ));
    }
    let Some(item) = deployments.get(key).cloned() else {
        return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
    };
    Ok(Json(item))
}

/// PUT /api/deployments
///
/// Bulk upsert; `replace=true` will replace the entire matrix.
pub async fn upsert_deployments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertDeploymentsRequest>,
) -> std::result::Result<Json<UpsertDeploymentsResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    if req.deployments.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployments cannot be empty".to_string(),
        ));
    }

    for dep in &req.deployments {
        if dep.id.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "deployment.id is required".to_string(),
            ));
        }
        if dep.account_ids.iter().any(|v| v.trim().is_empty()) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("deployment {} contains empty account_ids entry", dep.id),
            ));
        }
        if dep.enabled {
            ensure_required_strategy_evidence(&state, dep).await?;
        }
    }

    let upserted = req.deployments.len();
    let total = {
        let mut deployments = state.deployments.write().await;
        if req.replace {
            deployments.clear();
        }
        for mut dep in req.deployments {
            dep.normalize_account_ids_in_place();
            deployments.insert(dep.id.clone(), dep);
        }
        deployments.len()
    };
    state.persist_deployments().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist deployments: {}", e),
        )
    })?;

    Ok(Json(UpsertDeploymentsResponse {
        success: true,
        upserted,
        total,
    }))
}

/// POST /api/deployments/:id/enable
pub async fn enable_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<DeploymentMutationResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    set_deployment_enabled(state, id, true).await
}

/// POST /api/deployments/:id/disable
pub async fn disable_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<DeploymentMutationResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    set_deployment_enabled(state, id, false).await
}

async fn set_deployment_enabled(
    state: AppState,
    id: String,
    enabled: bool,
) -> std::result::Result<Json<DeploymentMutationResponse>, (StatusCode, String)> {
    let key = id.trim();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment id is required".to_string(),
        ));
    }

    if enabled {
        let dep_snapshot = {
            let deployments = state.deployments.read().await;
            deployments.get(key).cloned()
        };
        let Some(dep_snapshot) = dep_snapshot else {
            return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
        };
        ensure_required_strategy_evidence(&state, &dep_snapshot).await?;
    }

    let (id, enabled_now) = {
        let mut deployments = state.deployments.write().await;
        let Some(dep) = deployments.get_mut(key) else {
            return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
        };
        dep.enabled = enabled;
        (dep.id.clone(), dep.enabled)
    };

    state.persist_deployments().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist deployments: {}", e),
        )
    })?;

    Ok(Json(DeploymentMutationResponse {
        success: true,
        id,
        enabled: enabled_now,
    }))
}

/// DELETE /api/deployments/:id
pub async fn delete_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Json<DeploymentDeleteResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let key = id.trim();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment id is required".to_string(),
        ));
    }

    {
        let mut deployments = state.deployments.write().await;
        let Some(_removed) = deployments.remove(key) else {
            return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
        };
    }
    state.persist_deployments().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist deployments: {}", e),
        )
    })?;

    Ok(Json(DeploymentDeleteResponse {
        success: true,
        id: key.to_string(),
    }))
}
