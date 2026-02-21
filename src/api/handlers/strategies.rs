use axum::{
    extract::{Path, State},
    http::HeaderMap,
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use std::collections::HashMap;

use crate::api::{auth::ensure_admin_authorized, state::AppState, types::RunningStrategy};
use crate::platform::{
    AgentStatus, Domain, MarketSelector, StrategyDeployment, StrategyLifecycleStage,
    StrategyProductType,
};

#[derive(Debug, Clone, Serialize)]
pub struct StrategyControlEntry {
    pub deployment_id: String,
    pub strategy: String,
    pub strategy_version: String,
    pub domain: String,
    pub enabled: bool,
    pub timeframe: String,
    pub lifecycle_stage: String,
    pub product_type: String,
    pub market_selector_mode: String,
    pub allocator_profile: String,
    pub risk_profile: String,
    pub priority: i32,
    pub cooldown_secs: u64,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_evaluation_score: Option<f64>,
    pub domain_ingress_mode: String,
    pub running_agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategiesControlResponse {
    pub account_id: Option<String>,
    pub ingress_mode: Option<String>,
    pub items: Vec<StrategyControlEntry>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyControlMutationResponse {
    pub success: bool,
    pub deployment_id: String,
    pub strategy_version: String,
    pub enabled: bool,
    pub priority: i32,
    pub cooldown_secs: u64,
    pub lifecycle_stage: String,
    pub product_type: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_evaluation_score: Option<f64>,
    pub allocator_profile: String,
    pub risk_profile: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateStrategyControlRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
    #[serde(default)]
    pub allocator_profile: Option<String>,
    #[serde(default)]
    pub risk_profile: Option<String>,
    #[serde(default)]
    pub strategy_version: Option<String>,
    #[serde(default)]
    pub lifecycle_stage: Option<StrategyLifecycleStage>,
    #[serde(default)]
    pub product_type: Option<StrategyProductType>,
    #[serde(default)]
    pub last_evaluation_score: Option<f64>,
}

fn valid_strategy_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn domain_key(domain: Domain) -> String {
    match domain {
        Domain::Crypto => "crypto".to_string(),
        Domain::Sports => "sports".to_string(),
        Domain::Politics => "politics".to_string(),
        Domain::Economics => "economics".to_string(),
        Domain::Custom(id) => format!("custom:{}", id),
    }
}

fn selector_mode(selector: &MarketSelector) -> &'static str {
    match selector {
        MarketSelector::Static { .. } => "static",
        MarketSelector::Dynamic { .. } => "dynamic",
    }
}

/// GET /api/strategies/running
///
/// Returns a best-effort list derived from the coordinator's GlobalState.
pub async fn get_running_strategies(
    State(state): State<AppState>,
) -> std::result::Result<Json<Vec<RunningStrategy>>, (StatusCode, String)> {
    let Some(coordinator) = state.coordinator.as_ref() else {
        // Standalone API mode (no platform coordinator) — synthesize a single strategy status
        // from in-memory system_status + DB daily_metrics.
        let status = {
            let s = state.system_status.read().await;
            match s.status {
                crate::api::state::SystemRunStatus::Running => "running",
                crate::api::state::SystemRunStatus::Stopped => "paused",
                crate::api::state::SystemRunStatus::Error => "error",
            }
            .to_string()
        };

        let (pnl_usd, order_count) = sqlx::query_as::<_, (rust_decimal::Decimal, i64)>(
            r#"
            SELECT
                COALESCE(total_pnl, 0) as total_pnl,
                COALESCE(total_cycles, 0)::BIGINT as total_cycles
            FROM daily_metrics
            WHERE date = CURRENT_DATE
            "#,
        )
        .fetch_optional(state.store.pool())
        .await
        .ok()
        .flatten()
        .map(|(pnl, cycles)| (pnl.to_f64().unwrap_or(0.0), cycles.max(0) as u64))
        .unwrap_or((0.0, 0));

        return Ok(Json(vec![RunningStrategy {
            name: "standalone".to_string(),
            status,
            pnl_usd,
            order_count,
            domain: "crypto".to_string(),
            win_rate: None,
            loss_streak: None,
            size_multiplier: None,
            settled_trades: None,
            daily_realized_pnl_usd: None,
        }]));
    };

    let global = coordinator.read_state().await;

    let mut strategies: Vec<RunningStrategy> = global
        .agents
        .values()
        .map(|snap| {
            let status = match snap.status {
                AgentStatus::Paused => "paused",
                AgentStatus::Error => "error",
                AgentStatus::Stopped => "paused",
                AgentStatus::Initializing | AgentStatus::Running | AgentStatus::Observing => {
                    "running"
                }
            }
            .to_string();

            let domain = match snap.domain {
                Domain::Crypto => "crypto",
                Domain::Sports => "sports",
                Domain::Politics => "politics",
                Domain::Economics => "economics",
                Domain::Custom(_) => "custom",
            }
            .to_string();

            let parse_f64 = |key: &str| snap.metrics.get(key).and_then(|v| v.parse::<f64>().ok());
            let parse_u32 = |key: &str| snap.metrics.get(key).and_then(|v| v.parse::<u32>().ok());
            let parse_u64 = |key: &str| snap.metrics.get(key).and_then(|v| v.parse::<u64>().ok());

            RunningStrategy {
                name: snap.name.clone(),
                status,
                pnl_usd: snap.daily_pnl.to_f64().unwrap_or(0.0),
                order_count: snap.position_count as u64,
                domain,
                win_rate: parse_f64("sports_win_rate"),
                loss_streak: parse_u32("sports_loss_streak"),
                size_multiplier: parse_f64("sports_size_multiplier"),
                settled_trades: parse_u64("sports_settled_trades"),
                daily_realized_pnl_usd: parse_f64("sports_daily_realized_pnl_usd"),
            }
        })
        .collect();

    strategies.sort_by(|a, b| a.domain.cmp(&b.domain).then_with(|| a.name.cmp(&b.name)));

    Ok(Json(strategies))
}

/// GET /api/strategies/control
///
/// Control-plane view that joins deployment matrix with runtime ingress/agent state.
pub async fn get_strategies_control(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<StrategiesControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;

    let (account_id, ingress_mode, domain_modes, domain_agents) =
        if let Some(coordinator) = state.coordinator.as_ref() {
            let snapshot = coordinator.governance_status().await;
            let mut domain_modes = HashMap::new();
            for row in &snapshot.domain_ingress_modes {
                domain_modes.insert(row.domain.clone(), row.mode.clone());
            }
            let mut domain_agents: HashMap<String, Vec<String>> = HashMap::new();
            for agent in &snapshot.agents {
                if agent.status == "running" || agent.status == "observing" {
                    domain_agents
                        .entry(agent.domain.clone())
                        .or_default()
                        .push(agent.name.clone());
                }
            }
            for names in domain_agents.values_mut() {
                names.sort();
                names.dedup();
            }

            (
                Some(snapshot.account_id),
                Some(snapshot.ingress_mode),
                domain_modes,
                domain_agents,
            )
        } else {
            (None, None, HashMap::new(), HashMap::new())
        };

    let items = {
        let deployments = state.deployments.read().await;
        let mut entries: Vec<StrategyControlEntry> = deployments
            .values()
            .map(|dep: &StrategyDeployment| {
                let domain = domain_key(dep.domain);
                let running_agents = domain_agents.get(&domain).cloned().unwrap_or_default();
                StrategyControlEntry {
                    deployment_id: dep.id.clone(),
                    strategy: dep.strategy.clone(),
                    strategy_version: dep.strategy_version.clone(),
                    domain: domain.clone(),
                    enabled: dep.enabled,
                    timeframe: dep.timeframe.as_str().to_string(),
                    lifecycle_stage: dep.lifecycle_stage.as_str().to_string(),
                    product_type: dep.product_type.as_str().to_string(),
                    market_selector_mode: selector_mode(&dep.market_selector).to_string(),
                    allocator_profile: dep.allocator_profile.clone(),
                    risk_profile: dep.risk_profile.clone(),
                    priority: dep.priority,
                    cooldown_secs: dep.cooldown_secs,
                    last_evaluated_at: dep.last_evaluated_at,
                    last_evaluation_score: dep.last_evaluation_score,
                    domain_ingress_mode: domain_modes
                        .get(&domain)
                        .cloned()
                        .unwrap_or_else(|| "running".to_string()),
                    running_agents,
                }
            })
            .collect();
        entries.sort_by(|a, b| a.deployment_id.cmp(&b.deployment_id));
        entries
    };

    Ok(Json(StrategiesControlResponse {
        account_id,
        ingress_mode,
        items,
        updated_at: Utc::now(),
    }))
}

/// PUT /api/strategies/control/:id
///
/// Targeted deployment control patch for AI scheduler/control-plane.
pub async fn update_strategy_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateStrategyControlRequest>,
) -> std::result::Result<Json<StrategyControlMutationResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;

    let key = id.trim();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment id is required".to_string(),
        ));
    }
    if let Some(priority) = req.priority {
        if !(-10_000..=10_000).contains(&priority) {
            return Err((
                StatusCode::BAD_REQUEST,
                "priority must be between -10000 and 10000".to_string(),
            ));
        }
    }
    if let Some(score) = req.last_evaluation_score {
        if !(0.0..=1.0).contains(&score) {
            return Err((
                StatusCode::BAD_REQUEST,
                "last_evaluation_score must be between 0 and 1".to_string(),
            ));
        }
    }
    if let Some(version) = req.strategy_version.as_deref().map(str::trim) {
        if !valid_strategy_version(version) {
            return Err((
                StatusCode::BAD_REQUEST,
                "strategy_version must match [A-Za-z0-9._-] and be <= 64 chars".to_string(),
            ));
        }
    }

    let response = {
        let mut deployments = state.deployments.write().await;
        let Some(dep) = deployments.get_mut(key) else {
            return Err((StatusCode::NOT_FOUND, "deployment not found".to_string()));
        };

        if let Some(enabled) = req.enabled {
            dep.enabled = enabled;
        }
        if let Some(priority) = req.priority {
            dep.priority = priority;
        }
        if let Some(cooldown_secs) = req.cooldown_secs {
            dep.cooldown_secs = cooldown_secs;
        }
        if let Some(allocator_profile) = req
            .allocator_profile
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            dep.allocator_profile = allocator_profile.to_string();
        }
        if let Some(risk_profile) = req
            .risk_profile
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            dep.risk_profile = risk_profile.to_string();
        }
        if let Some(strategy_version) = req
            .strategy_version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            dep.strategy_version = strategy_version.to_string();
            dep.last_evaluated_at = Some(Utc::now());
        }
        if let Some(lifecycle_stage) = req.lifecycle_stage {
            dep.lifecycle_stage = lifecycle_stage;
            dep.last_evaluated_at = Some(Utc::now());
        }
        if let Some(product_type) = req.product_type {
            dep.product_type = product_type;
        }
        if let Some(last_evaluation_score) = req.last_evaluation_score {
            dep.last_evaluation_score = Some(last_evaluation_score);
            dep.last_evaluated_at = Some(Utc::now());
        }

        StrategyControlMutationResponse {
            success: true,
            deployment_id: dep.id.clone(),
            strategy_version: dep.strategy_version.clone(),
            enabled: dep.enabled,
            priority: dep.priority,
            cooldown_secs: dep.cooldown_secs,
            lifecycle_stage: dep.lifecycle_stage.as_str().to_string(),
            product_type: dep.product_type.as_str().to_string(),
            last_evaluated_at: dep.last_evaluated_at,
            last_evaluation_score: dep.last_evaluation_score,
            allocator_profile: dep.allocator_profile.clone(),
            risk_profile: dep.risk_profile.clone(),
        }
    };

    state.persist_deployments().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist deployments: {}", e),
        )
    })?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::valid_strategy_version;

    #[test]
    fn strategy_version_validation_allows_semver_like_values() {
        assert!(valid_strategy_version("v1"));
        assert!(valid_strategy_version("v2.3.1"));
        assert!(valid_strategy_version("alpha_2026-02"));
    }

    #[test]
    fn strategy_version_validation_rejects_invalid_values() {
        assert!(!valid_strategy_version(""));
        assert!(!valid_strategy_version("bad version"));
        assert!(!valid_strategy_version("v1/../../"));
    }
}
