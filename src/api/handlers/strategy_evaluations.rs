use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::api::{auth::ensure_admin_authorized, state::AppState};
use crate::platform::{
    StrategyEvaluationEvidence, StrategyEvaluationMetrics, StrategyEvaluationStage,
    StrategyLifecycleStage, StrategyProductType,
};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StrategyEvaluationsQuery {
    pub deployment_id: Option<String>,
    pub strategy: Option<String>,
    pub strategy_version: Option<String>,
    pub stage: Option<String>,
    pub lifecycle_stage: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateStrategyEvaluationRequest {
    #[serde(default)]
    pub evaluation_id: Option<String>,
    pub deployment_id: String,
    pub strategy: String,
    pub strategy_version: String,
    pub product_type: StrategyProductType,
    pub lifecycle_stage: StrategyLifecycleStage,
    pub stage: StrategyEvaluationStage,
    #[serde(default)]
    pub evaluated_at: Option<DateTime<Utc>>,
    pub evaluator: String,
    pub dataset_hash: String,
    #[serde(default)]
    pub model_hash: Option<String>,
    #[serde(default)]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub artifact_uri: Option<String>,
    pub metrics: StrategyEvaluationMetrics,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateStrategyEvaluationResponse {
    pub success: bool,
    pub item: StrategyEvaluationEvidence,
}

fn normalize_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn valid_strategy_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_hash_like(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty()
        && v.len() <= 256
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '/' | '.'))
}

fn validate_metrics(metrics: &StrategyEvaluationMetrics) -> std::result::Result<(), String> {
    if metrics.sample_size == 0 {
        return Err("metrics.sample_size must be > 0".to_string());
    }

    let check_ratio = |name: &str, value: Option<f64>| -> std::result::Result<(), String> {
        if let Some(v) = value {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{name} must be between 0 and 1"));
            }
        }
        Ok(())
    };

    check_ratio("metrics.win_rate", metrics.win_rate)?;
    check_ratio("metrics.max_drawdown_pct", metrics.max_drawdown_pct)?;
    check_ratio("metrics.fill_rate", metrics.fill_rate)?;
    Ok(())
}

/// GET /api/strategy-evaluations
pub async fn list_strategy_evaluations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StrategyEvaluationsQuery>,
) -> std::result::Result<Json<Vec<StrategyEvaluationEvidence>>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;

    let deployment_id = normalize_opt(&query.deployment_id);
    let strategy = normalize_opt(&query.strategy).map(|v| v.to_ascii_lowercase());
    let strategy_version = normalize_opt(&query.strategy_version);
    let stage = normalize_opt(&query.stage).map(|v| v.to_ascii_lowercase());
    let lifecycle_stage = normalize_opt(&query.lifecycle_stage).map(|v| v.to_ascii_lowercase());
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    let items = {
        let rows = state.strategy_evaluations.read().await;
        rows.iter()
            .filter(|row| {
                deployment_id
                    .as_deref()
                    .map(|v| row.deployment_id.eq_ignore_ascii_case(v))
                    .unwrap_or(true)
            })
            .filter(|row| {
                strategy
                    .as_deref()
                    .map(|v| row.strategy.to_ascii_lowercase() == v)
                    .unwrap_or(true)
            })
            .filter(|row| {
                strategy_version
                    .as_deref()
                    .map(|v| row.strategy_version.eq_ignore_ascii_case(v))
                    .unwrap_or(true)
            })
            .filter(|row| {
                stage
                    .as_deref()
                    .map(|v| row.stage.as_str() == v)
                    .unwrap_or(true)
            })
            .filter(|row| {
                lifecycle_stage
                    .as_deref()
                    .map(|v| row.lifecycle_stage.as_str() == v)
                    .unwrap_or(true)
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
    };

    Ok(Json(items))
}

/// GET /api/strategy-evaluations/:deployment_id/latest
pub async fn get_latest_strategy_evaluation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(deployment_id): Path<String>,
    Query(query): Query<StrategyEvaluationsQuery>,
) -> std::result::Result<Json<StrategyEvaluationEvidence>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let deployment_id = deployment_id.trim();
    if deployment_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment_id is required".to_string(),
        ));
    }

    let stage = normalize_opt(&query.stage).map(|v| v.to_ascii_lowercase());
    let strategy_version = normalize_opt(&query.strategy_version);

    let item = {
        let rows = state.strategy_evaluations.read().await;
        rows.iter()
            .find(|row| {
                row.deployment_id.eq_ignore_ascii_case(deployment_id)
                    && stage
                        .as_deref()
                        .map(|v| row.stage.as_str() == v)
                        .unwrap_or(true)
                    && strategy_version
                        .as_deref()
                        .map(|v| row.strategy_version.eq_ignore_ascii_case(v))
                        .unwrap_or(true)
            })
            .cloned()
    };

    let Some(item) = item else {
        return Err((
            StatusCode::NOT_FOUND,
            "strategy evaluation not found".to_string(),
        ));
    };
    Ok(Json(item))
}

/// POST /api/strategy-evaluations
pub async fn create_strategy_evaluation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateStrategyEvaluationRequest>,
) -> std::result::Result<Json<CreateStrategyEvaluationResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;

    let deployment_id = req.deployment_id.trim();
    if deployment_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment_id is required".to_string(),
        ));
    }

    let strategy = req.strategy.trim();
    if strategy.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "strategy is required".to_string()));
    }

    let strategy_version = req.strategy_version.trim();
    if !valid_strategy_version(strategy_version) {
        return Err((
            StatusCode::BAD_REQUEST,
            "strategy_version must match [A-Za-z0-9._-] and be <= 64 chars".to_string(),
        ));
    }

    let evaluator = req.evaluator.trim();
    if evaluator.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "evaluator is required".to_string()));
    }

    let dataset_hash = req.dataset_hash.trim();
    if !valid_hash_like(dataset_hash) {
        return Err((
            StatusCode::BAD_REQUEST,
            "dataset_hash is required and must be hash-like".to_string(),
        ));
    }

    if let Some(model_hash) = req.model_hash.as_deref() {
        if !valid_hash_like(model_hash) {
            return Err((
                StatusCode::BAD_REQUEST,
                "model_hash must be hash-like".to_string(),
            ));
        }
    }
    if let Some(config_hash) = req.config_hash.as_deref() {
        if !valid_hash_like(config_hash) {
            return Err((
                StatusCode::BAD_REQUEST,
                "config_hash must be hash-like".to_string(),
            ));
        }
    }

    if matches!(req.stage, StrategyEvaluationStage::Live)
        && !matches!(req.lifecycle_stage, StrategyLifecycleStage::Live)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "live evaluation stage requires lifecycle_stage=live".to_string(),
        ));
    }

    validate_metrics(&req.metrics).map_err(|msg| (StatusCode::BAD_REQUEST, msg))?;

    let evaluation_id = req
        .evaluation_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if evaluation_id.len() > 128 {
        return Err((
            StatusCode::BAD_REQUEST,
            "evaluation_id must be <= 128 chars".to_string(),
        ));
    }

    let item = StrategyEvaluationEvidence {
        evaluation_id: evaluation_id.clone(),
        deployment_id: deployment_id.to_string(),
        strategy: strategy.to_string(),
        strategy_version: strategy_version.to_string(),
        product_type: req.product_type,
        lifecycle_stage: req.lifecycle_stage,
        stage: req.stage,
        evaluated_at: req.evaluated_at.unwrap_or_else(Utc::now),
        evaluator: evaluator.to_string(),
        dataset_hash: dataset_hash.to_string(),
        model_hash: normalize_opt(&req.model_hash),
        config_hash: normalize_opt(&req.config_hash),
        run_id: normalize_opt(&req.run_id),
        artifact_uri: normalize_opt(&req.artifact_uri),
        metrics: req.metrics,
        metadata: req.metadata,
    };

    {
        let mut rows = state.strategy_evaluations.write().await;
        rows.retain(|row| row.evaluation_id != evaluation_id);
        rows.push(item.clone());
        rows.sort_by(|a, b| {
            b.evaluated_at
                .cmp(&a.evaluated_at)
                .then_with(|| b.evaluation_id.cmp(&a.evaluation_id))
        });
    }
    state.persist_strategy_evaluations().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist strategy evaluations: {}", e),
        )
    })?;

    Ok(Json(CreateStrategyEvaluationResponse {
        success: true,
        item,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_metrics_rejects_out_of_range_values() {
        let metrics = StrategyEvaluationMetrics {
            sample_size: 10,
            win_rate: Some(1.2),
            pnl_usd: None,
            max_drawdown_pct: Some(0.2),
            sharpe: None,
            fill_rate: Some(0.8),
            avg_slippage_bps: None,
        };
        let err = validate_metrics(&metrics).expect_err("win_rate > 1 should fail");
        assert!(err.contains("win_rate"));
    }

    #[test]
    fn valid_hash_like_accepts_common_hash_formats() {
        assert!(valid_hash_like("sha256:abcd1234"));
        assert!(valid_hash_like("dataset/v1/abcde"));
        assert!(valid_hash_like("0xabc123"));
    }
}
