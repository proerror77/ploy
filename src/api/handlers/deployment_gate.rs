use axum::http::StatusCode;
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::Row;

use crate::api::state::AppState;
use crate::platform::StrategyDeployment;

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

pub(crate) async fn ensure_required_strategy_evidence(
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
