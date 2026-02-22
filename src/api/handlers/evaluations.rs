use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::api::{auth::ensure_sidecar_or_admin_authorized, state::AppState};
use crate::platform::Domain;

#[derive(Debug, Deserialize)]
pub struct UpsertStrategyEvaluationRequest {
    pub account_id: Option<String>,
    pub strategy_id: String,
    pub deployment_id: Option<String>,
    pub domain: String,
    pub stage: String,  // backtest | paper | live
    pub status: String, // pass | fail | warn | unknown
    pub evaluated_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub timeframe: Option<String>,
    pub sample_size: Option<i64>,
    pub pnl_usd: Option<f64>,
    pub win_rate: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_drawdown_pct: Option<f64>,
    pub max_drawdown_usd: Option<f64>,
    pub evidence_kind: Option<String>,
    pub evidence_ref: Option<String>,
    pub evidence_hash: Option<String>,
    pub evidence_payload: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct UpsertStrategyEvaluationResponse {
    pub success: bool,
    pub evaluation_id: i64,
    pub deduped: bool,
    pub account_id: String,
    pub strategy_id: String,
    pub stage: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct StrategyEvaluationsQuery {
    pub strategy_id: Option<String>,
    pub deployment_id: Option<String>,
    pub stage: Option<String>,
    pub status: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct StrategyEvaluationRecord {
    pub id: i64,
    pub account_id: String,
    pub strategy_id: String,
    pub deployment_id: Option<String>,
    pub domain: String,
    pub stage: String,
    pub status: String,
    pub evaluated_at: String,
    pub score: Option<f64>,
    pub timeframe: Option<String>,
    pub sample_size: Option<i64>,
    pub pnl_usd: Option<f64>,
    pub win_rate: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_drawdown_pct: Option<f64>,
    pub max_drawdown_usd: Option<f64>,
    pub evidence_kind: String,
    pub evidence_ref: Option<String>,
    pub evidence_hash: Option<String>,
    pub evidence_payload: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}

fn normalize_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn normalize_account(
    value: Option<&str>,
    runtime_account: &str,
) -> std::result::Result<String, (StatusCode, String)> {
    if let Some(account) = normalize_text(value) {
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
    Ok(runtime_account.to_string())
}

fn normalize_stage(raw: &str) -> std::result::Result<String, (StatusCode, String)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "backtest" => Ok("BACKTEST".to_string()),
        "paper" => Ok("PAPER".to_string()),
        "live" => Ok("LIVE".to_string()),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("invalid stage '{}', expected backtest|paper|live", raw),
        )),
    }
}

fn normalize_status(raw: &str) -> std::result::Result<String, (StatusCode, String)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pass" => Ok("PASS".to_string()),
        "fail" => Ok("FAIL".to_string()),
        "warn" => Ok("WARN".to_string()),
        "unknown" => Ok("UNKNOWN".to_string()),
        _ => Err((
            StatusCode::BAD_REQUEST,
            format!("invalid status '{}', expected pass|fail|warn|unknown", raw),
        )),
    }
}

fn normalize_domain(raw: &str) -> std::result::Result<String, (StatusCode, String)> {
    Domain::parse_optional(Some(raw), Domain::Crypto)
        .map(|d| d.to_string())
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "invalid domain '{}', expected crypto|sports|politics|economics|custom:<id>",
                    raw
                ),
            )
        })
}

fn decimal_opt(value: Option<f64>) -> Option<Decimal> {
    value.and_then(Decimal::from_f64_retain)
}

/// POST /api/sidecar/strategy-evaluations
///
/// Persist strategy-level evaluation evidence (backtest/paper/live).
pub async fn upsert_strategy_evaluation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertStrategyEvaluationRequest>,
) -> std::result::Result<Json<UpsertStrategyEvaluationResponse>, (StatusCode, String)> {
    ensure_sidecar_or_admin_authorized(&headers)?;

    let runtime_account = state.account_id.trim();
    let account_id = normalize_account(req.account_id.as_deref(), runtime_account)?;

    let strategy_id = req.strategy_id.trim().to_string();
    if strategy_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "strategy_id is required".to_string(),
        ));
    }

    if let Some(sample_size) = req.sample_size {
        if sample_size < 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                "sample_size must be >= 0".to_string(),
            ));
        }
    }

    let domain = normalize_domain(req.domain.as_str())?;
    let stage = normalize_stage(req.stage.as_str())?;
    let status = normalize_status(req.status.as_str())?;

    let deployment_id = normalize_text(req.deployment_id.as_deref());
    let timeframe = normalize_text(req.timeframe.as_deref());
    let evidence_kind =
        normalize_text(req.evidence_kind.as_deref()).unwrap_or_else(|| "report".to_string());
    let evidence_ref = normalize_text(req.evidence_ref.as_deref());
    let evidence_hash = normalize_text(req.evidence_hash.as_deref());

    if let Some(hash) = evidence_hash.as_ref() {
        let existing = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT id
            FROM strategy_evaluations
            WHERE account_id = $1
              AND strategy_id = $2
              AND stage = $3
              AND evidence_hash = $4
            ORDER BY evaluated_at DESC
            LIMIT 1
            "#,
        )
        .bind(account_id.as_str())
        .bind(strategy_id.as_str())
        .bind(stage.as_str())
        .bind(hash.as_str())
        .fetch_optional(state.store.pool())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to dedupe strategy evaluation: {}", e),
            )
        })?;

        if let Some(existing_id) = existing {
            return Ok(Json(UpsertStrategyEvaluationResponse {
                success: true,
                evaluation_id: existing_id,
                deduped: true,
                account_id,
                strategy_id,
                stage,
                status,
            }));
        }
    }

    let evaluation_id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO strategy_evaluations (
            account_id,
            evaluated_at,
            strategy_id,
            deployment_id,
            domain,
            stage,
            status,
            score,
            timeframe,
            sample_size,
            pnl_usd,
            win_rate,
            sharpe,
            max_drawdown_pct,
            max_drawdown_usd,
            evidence_kind,
            evidence_ref,
            evidence_hash,
            evidence_payload,
            metadata
        )
        VALUES (
            $1, COALESCE($2, NOW()), $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
        )
        RETURNING id
        "#,
    )
    .bind(account_id.as_str())
    .bind(req.evaluated_at)
    .bind(strategy_id.as_str())
    .bind(deployment_id.as_deref())
    .bind(domain.as_str())
    .bind(stage.as_str())
    .bind(status.as_str())
    .bind(decimal_opt(req.score))
    .bind(timeframe.as_deref())
    .bind(req.sample_size)
    .bind(decimal_opt(req.pnl_usd))
    .bind(decimal_opt(req.win_rate))
    .bind(decimal_opt(req.sharpe))
    .bind(decimal_opt(req.max_drawdown_pct))
    .bind(decimal_opt(req.max_drawdown_usd))
    .bind(evidence_kind.as_str())
    .bind(evidence_ref.as_deref())
    .bind(evidence_hash.as_deref())
    .bind(req.evidence_payload.map(sqlx::types::Json))
    .bind(req.metadata.map(sqlx::types::Json))
    .fetch_one(state.store.pool())
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist strategy evaluation: {}", e),
        )
    })?;

    Ok(Json(UpsertStrategyEvaluationResponse {
        success: true,
        evaluation_id,
        deduped: false,
        account_id,
        strategy_id,
        stage,
        status,
    }))
}

/// GET /api/sidecar/strategy-evaluations
///
/// Query strategy evidence records under the runtime account scope.
pub async fn list_strategy_evaluations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StrategyEvaluationsQuery>,
) -> std::result::Result<Json<Vec<StrategyEvaluationRecord>>, (StatusCode, String)> {
    ensure_sidecar_or_admin_authorized(&headers)?;

    let strategy_id = normalize_text(query.strategy_id.as_deref());
    let deployment_id = normalize_text(query.deployment_id.as_deref());
    let stage = query.stage.as_deref().map(normalize_stage).transpose()?;
    let status = query.status.as_deref().map(normalize_status).transpose()?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200) as i64;

    let rows = sqlx::query(
        r#"
        SELECT
            id,
            account_id,
            strategy_id,
            deployment_id,
            domain,
            stage,
            status,
            evaluated_at,
            score,
            timeframe,
            sample_size,
            pnl_usd,
            win_rate,
            sharpe,
            max_drawdown_pct,
            max_drawdown_usd,
            evidence_kind,
            evidence_ref,
            evidence_hash,
            evidence_payload,
            metadata
        FROM strategy_evaluations
        WHERE account_id = $1
          AND ($2::text IS NULL OR strategy_id = $2)
          AND ($3::text IS NULL OR deployment_id = $3)
          AND ($4::text IS NULL OR stage = $4)
          AND ($5::text IS NULL OR status = $5)
        ORDER BY evaluated_at DESC, id DESC
        LIMIT $6
        "#,
    )
    .bind(state.account_id.as_str())
    .bind(strategy_id.as_deref())
    .bind(deployment_id.as_deref())
    .bind(stage.as_deref())
    .bind(status.as_deref())
    .bind(limit)
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to query strategy evaluations: {}", e),
        )
    })?;

    let records = rows
        .into_iter()
        .map(|row| StrategyEvaluationRecord {
            id: row.try_get("id").unwrap_or_default(),
            account_id: row.try_get("account_id").unwrap_or_default(),
            strategy_id: row.try_get("strategy_id").unwrap_or_default(),
            deployment_id: row.try_get("deployment_id").ok(),
            domain: row.try_get("domain").unwrap_or_default(),
            stage: row.try_get("stage").unwrap_or_default(),
            status: row.try_get("status").unwrap_or_default(),
            evaluated_at: row
                .try_get::<DateTime<Utc>, _>("evaluated_at")
                .unwrap_or_else(|_| Utc::now())
                .to_rfc3339(),
            score: row
                .try_get::<Decimal, _>("score")
                .ok()
                .and_then(|v| v.to_f64()),
            timeframe: row.try_get("timeframe").ok(),
            sample_size: row.try_get("sample_size").ok(),
            pnl_usd: row
                .try_get::<Decimal, _>("pnl_usd")
                .ok()
                .and_then(|v| v.to_f64()),
            win_rate: row
                .try_get::<Decimal, _>("win_rate")
                .ok()
                .and_then(|v| v.to_f64()),
            sharpe: row
                .try_get::<Decimal, _>("sharpe")
                .ok()
                .and_then(|v| v.to_f64()),
            max_drawdown_pct: row
                .try_get::<Decimal, _>("max_drawdown_pct")
                .ok()
                .and_then(|v| v.to_f64()),
            max_drawdown_usd: row
                .try_get::<Decimal, _>("max_drawdown_usd")
                .ok()
                .and_then(|v| v.to_f64()),
            evidence_kind: row.try_get("evidence_kind").unwrap_or_default(),
            evidence_ref: row.try_get("evidence_ref").ok(),
            evidence_hash: row.try_get("evidence_hash").ok(),
            evidence_payload: row
                .try_get::<sqlx::types::Json<serde_json::Value>, _>("evidence_payload")
                .ok()
                .map(|v| v.0),
            metadata: row
                .try_get::<sqlx::types::Json<serde_json::Value>, _>("metadata")
                .ok()
                .map(|v| v.0),
        })
        .collect::<Vec<_>>();

    Ok(Json(records))
}
