use axum::{extract::State, http::HeaderMap, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use std::collections::HashMap;

use crate::api::{auth::ensure_admin_authorized, state::AppState, types::RunningStrategy};
use crate::platform::{AgentStatus, Domain, MarketSelector, StrategyDeployment};

#[derive(Debug, Clone, Serialize)]
pub struct StrategyControlEntry {
    pub deployment_id: String,
    pub strategy: String,
    pub domain: String,
    pub enabled: bool,
    pub timeframe: String,
    pub market_selector_mode: String,
    pub allocator_profile: String,
    pub risk_profile: String,
    pub priority: i32,
    pub cooldown_secs: u64,
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
                    domain: domain.clone(),
                    enabled: dep.enabled,
                    timeframe: dep.timeframe.as_str().to_string(),
                    market_selector_mode: selector_mode(&dep.market_selector).to_string(),
                    allocator_profile: dep.allocator_profile.clone(),
                    risk_profile: dep.risk_profile.clone(),
                    priority: dep.priority,
                    cooldown_secs: dep.cooldown_secs,
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
