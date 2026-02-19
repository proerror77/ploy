use axum::{extract::State, http::StatusCode, Json};
use rust_decimal::prelude::ToPrimitive;

use crate::api::{state::AppState, types::RunningStrategy};
use crate::platform::{AgentStatus, Domain};

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
