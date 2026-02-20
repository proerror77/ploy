use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

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
    }

    let upserted = req.deployments.len();
    let total = {
        let mut deployments = state.deployments.write().await;
        if req.replace {
            deployments.clear();
        }
        for dep in req.deployments {
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

    let mut deployments = state.deployments.write().await;
    let Some(dep) = deployments.get_mut(key) else {
        return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
    };
    dep.enabled = enabled;
    let id = dep.id.clone();
    let enabled_now = dep.enabled;
    drop(deployments);
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
