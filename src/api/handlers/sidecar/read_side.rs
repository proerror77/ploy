use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use tracing::warn;

use crate::api::{
    auth::{ensure_sidecar_authorized, ensure_sidecar_or_admin_authorized},
    state::AppState,
};
use crate::config::AppConfig;
use crate::domain::market::Side;

use super::{
    table_has_account_scope, SidecarCircuitBreakerEvent, SidecarPosition, SidecarRiskPosition,
    SidecarRiskState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DailyMetricsScope {
    Scoped,
    Global,
}

fn resolve_risk_fallback_daily_metrics_scope(
    runtime_state_scoped: bool,
    daily_metrics_scoped: bool,
    positions_scoped: bool,
) -> std::result::Result<DailyMetricsScope, Vec<&'static str>> {
    let mut missing = Vec::new();
    if !runtime_state_scoped {
        missing.push("risk_runtime_state.account_id");
    }
    if !positions_scoped {
        missing.push("positions.account_id");
    }
    if !missing.is_empty() {
        return Err(missing);
    }

    Ok(if daily_metrics_scoped {
        DailyMetricsScope::Scoped
    } else {
        DailyMetricsScope::Global
    })
}

async fn load_daily_metrics_halted(
    pool: &sqlx::PgPool,
    account_id: &str,
    scope: DailyMetricsScope,
) -> bool {
    match scope {
        DailyMetricsScope::Scoped => {
            sqlx::query_scalar::<_, bool>(
                "SELECT COALESCE(halted, FALSE) FROM daily_metrics WHERE date = CURRENT_DATE AND account_id = $1",
            )
            .bind(account_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
        }
        DailyMetricsScope::Global => {
            sqlx::query_scalar!(
                "SELECT COALESCE(halted, FALSE) AS \"halted!\" FROM daily_metrics WHERE date = CURRENT_DATE"
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
        }
    }
}

async fn load_daily_metrics_total_pnl(
    pool: &sqlx::PgPool,
    account_id: &str,
    scope: DailyMetricsScope,
) -> Decimal {
    match scope {
        DailyMetricsScope::Scoped => {
            sqlx::query_scalar::<_, Decimal>(
                "SELECT COALESCE(total_pnl, 0) FROM daily_metrics WHERE date = CURRENT_DATE AND account_id = $1",
            )
            .bind(account_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(Decimal::ZERO)
        }
        DailyMetricsScope::Global => {
            sqlx::query_scalar!(
                "SELECT COALESCE(total_pnl, 0)::numeric AS \"total_pnl!: Decimal\" FROM daily_metrics WHERE date = CURRENT_DATE"
            )
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(Decimal::ZERO)
        }
    }
}

async fn load_daily_metrics_halt_event(
    pool: &sqlx::PgPool,
    account_id: &str,
    scope: DailyMetricsScope,
) -> Option<(bool, Option<String>, chrono::DateTime<Utc>)> {
    match scope {
        DailyMetricsScope::Scoped => {
            sqlx::query_as::<_, (bool, Option<String>, chrono::DateTime<Utc>)>(
                r#"
                SELECT halted, halt_reason, updated_at
                FROM daily_metrics
                WHERE date = CURRENT_DATE
                  AND account_id = $1
                "#,
            )
            .bind(account_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        }
        DailyMetricsScope::Global => sqlx::query!(
            r#"
                SELECT
                    COALESCE(halted, FALSE) AS "halted!",
                    halt_reason,
                    updated_at
                FROM daily_metrics
                WHERE date = CURRENT_DATE
                "#,
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|row| (row.halted, row.halt_reason, row.updated_at)),
    }
}

/// GET /api/sidecar/positions
///
/// Returns current open positions from the database.
pub async fn sidecar_get_positions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Vec<SidecarPosition>>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;

    if let Some(coordinator) = state.coordinator.as_ref() {
        let global = coordinator.read_state().await;
        let mut runtime_positions = global.positions.clone();
        runtime_positions.sort_by(|a, b| b.entry_time.cmp(&a.entry_time));

        let positions: Vec<SidecarPosition> = runtime_positions
            .into_iter()
            .enumerate()
            .map(|(idx, p)| {
                let pnl = p.unrealized_pnl().to_f64().unwrap_or(0.0);
                SidecarPosition {
                    id: idx as i64 + 1,
                    market_slug: p.market_slug,
                    token_id: p.token_id,
                    side: match p.side {
                        Side::Up => "Yes".to_string(),
                        Side::Down => "No".to_string(),
                    },
                    shares: p.shares as i64,
                    avg_price: p.entry_price.to_f64().unwrap_or(0.0),
                    current_value: p
                        .current_price
                        .map(|px| (px * Decimal::from(p.shares)).to_f64().unwrap_or(0.0)),
                    pnl: Some(pnl),
                    status: "OPEN".to_string(),
                    opened_at: p.entry_time.to_rfc3339(),
                }
            })
            .collect();
        return Ok(Json(positions));
    }

    if !table_has_account_scope(state.store.pool(), "positions").await {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sidecar fallback requires positions.account_id scope".to_string(),
        ));
    }

    let rows = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            String,
            i64,
            f64,
            Option<f64>,
            Option<f64>,
            String,
            chrono::DateTime<Utc>,
        ),
    >(
        r#"
        SELECT
            id,
            event_id as market_slug,
            token_id,
            market_side as side,
            shares,
            avg_entry_price::double precision as avg_price,
            amount_usd::double precision as current_value,
            pnl::double precision as pnl,
            status,
            opened_at
        FROM positions
        WHERE status = 'OPEN'
          AND account_id = $1
        ORDER BY opened_at DESC
        LIMIT 100
        "#,
    )
    .bind(state.account_id.as_str())
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| {
        warn!(error = %e, "failed to fetch positions for sidecar");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
    })?;

    let positions: Vec<SidecarPosition> = rows
        .into_iter()
        .map(
            |(
                id,
                market_slug,
                token_id,
                side,
                shares,
                avg_price,
                current_value,
                pnl,
                status,
                opened_at,
            )| {
                SidecarPosition {
                    id,
                    market_slug,
                    token_id,
                    side,
                    shares,
                    avg_price,
                    current_value,
                    pnl,
                    status,
                    opened_at: opened_at.to_rfc3339(),
                }
            },
        )
        .collect();

    Ok(Json(positions))
}

/// GET /api/sidecar/risk
///
/// Returns risk state from the Coordinator's GlobalState.
pub async fn sidecar_get_risk(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<SidecarRiskState>, (StatusCode, String)> {
    ensure_sidecar_or_admin_authorized(&headers)?;
    match state.coordinator.as_ref() {
        Some(coordinator) => {
            let global = coordinator.read_state().await;

            let mut by_market: HashMap<(String, String), (f64, f64)> = HashMap::new();
            for p in &global.positions {
                let side = match p.side {
                    Side::Up => "Yes",
                    Side::Down => "No",
                }
                .to_string();

                let key = (p.market_slug.clone(), side);
                let size = p.notional_value().to_f64().unwrap_or(0.0);
                let pnl = p.unrealized_pnl().to_f64().unwrap_or(0.0);

                by_market
                    .entry(key)
                    .and_modify(|(s, pl)| {
                        *s += size;
                        *pl += pnl;
                    })
                    .or_insert((size, pnl));
            }

            let mut positions: Vec<SidecarRiskPosition> = by_market
                .into_iter()
                .map(|((market, side), (size, pnl_usd))| SidecarRiskPosition {
                    market,
                    side,
                    size,
                    pnl_usd,
                })
                .collect();
            positions.sort_by(|a, b| a.market.cmp(&b.market).then_with(|| a.side.cmp(&b.side)));

            let circuit_breaker_events = global
                .circuit_breaker_events
                .iter()
                .rev()
                .take(50)
                .map(|e| SidecarCircuitBreakerEvent {
                    timestamp: e.timestamp.to_rfc3339(),
                    reason: e.reason.clone(),
                    state: format!("{:?}", e.state),
                })
                .collect();

            Ok(Json(SidecarRiskState {
                risk_state: format!("{:?}", global.risk_state),
                daily_pnl_usd: global.daily_pnl.to_f64().unwrap_or(0.0),
                daily_loss_limit_usd: global.daily_loss_limit.to_f64().unwrap_or(0.0),
                current_drawdown_usd: global.current_drawdown.to_f64().unwrap_or(0.0),
                max_drawdown_observed_usd: global.max_drawdown_observed.to_f64().unwrap_or(0.0),
                drawdown_limit_usd: global.max_drawdown_limit.and_then(|v| v.to_f64()),
                queue_depth: global.queue_stats.current_size,
                positions,
                circuit_breaker_events,
            }))
        }
        None => {
            let runtime_state_scoped =
                table_has_account_scope(state.store.pool(), "risk_runtime_state").await;
            let daily_metrics_scoped =
                table_has_account_scope(state.store.pool(), "daily_metrics").await;
            let positions_scoped = table_has_account_scope(state.store.pool(), "positions").await;

            let daily_metrics_scope = match resolve_risk_fallback_daily_metrics_scope(
                runtime_state_scoped,
                daily_metrics_scoped,
                positions_scoped,
            ) {
                Ok(scope) => scope,
                Err(missing) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!(
                            "sidecar risk fallback requires account scope on: {}",
                            missing.join(", ")
                        ),
                    ));
                }
            };

            let (daily_metrics_scope_label, daily_metrics_filter_label) = match daily_metrics_scope
            {
                DailyMetricsScope::Scoped => ("account-scoped", "daily_metrics.account_id"),
                DailyMetricsScope::Global => ("global", "daily_metrics.date"),
            };

            if runtime_state_scoped && positions_scoped {
                tracing::debug!(
                    daily_metrics_scope = daily_metrics_scope_label,
                    daily_metrics_filter = daily_metrics_filter_label,
                    "sidecar risk fallback scope resolved"
                );
            }

            let runtime_row = sqlx::query_as::<
                _,
                (
                    String,
                    Option<chrono::NaiveDate>,
                    Decimal,
                    Decimal,
                    Decimal,
                    Decimal,
                    chrono::DateTime<Utc>,
                ),
            >(
                r#"
                SELECT
                    risk_state,
                    daily_date,
                    daily_pnl,
                    daily_loss_limit,
                    current_drawdown,
                    max_drawdown_observed,
                    updated_at
                FROM risk_runtime_state
                WHERE account_id = $1
                "#,
            )
            .bind(state.account_id.as_str())
            .fetch_optional(state.store.pool())
            .await
            .ok()
            .flatten();

            let (
                risk_state,
                daily_pnl,
                daily_loss_limit,
                current_drawdown,
                max_drawdown_observed,
                runtime_event,
            ) = if let Some((
                risk_state,
                daily_date,
                daily_pnl,
                daily_loss_limit,
                current_drawdown,
                max_drawdown_observed,
                updated_at,
            )) = runtime_row
            {
                let daily_pnl = if daily_date == Some(Utc::now().date_naive()) {
                    daily_pnl
                } else {
                    Decimal::ZERO
                };
                let runtime_event = if risk_state.eq_ignore_ascii_case("halted") {
                    Some(SidecarCircuitBreakerEvent {
                        timestamp: updated_at.to_rfc3339(),
                        reason: "restored runtime risk state".to_string(),
                        state: "Halted".to_string(),
                    })
                } else {
                    None
                };
                (
                    risk_state,
                    daily_pnl,
                    daily_loss_limit,
                    current_drawdown,
                    max_drawdown_observed,
                    runtime_event,
                )
            } else {
                let halted = load_daily_metrics_halted(
                    state.store.pool(),
                    state.account_id.as_str(),
                    daily_metrics_scope,
                )
                .await;
                let daily_pnl = load_daily_metrics_total_pnl(
                    state.store.pool(),
                    state.account_id.as_str(),
                    daily_metrics_scope,
                )
                .await;
                (
                    if halted {
                        "Halted".to_string()
                    } else {
                        "Normal".to_string()
                    },
                    daily_pnl,
                    AppConfig::load()
                        .ok()
                        .map(|c| c.risk.daily_loss_limit_usd)
                        .unwrap_or(Decimal::ZERO),
                    Decimal::ZERO,
                    Decimal::ZERO,
                    None,
                )
            };

            let rows = sqlx::query_as::<_, (String, String, f64, Option<f64>)>(
                r#"
                SELECT
                    event_id as market,
                    market_side as side,
                    SUM(amount_usd)::double precision as size,
                    SUM(pnl)::double precision as pnl_usd
                FROM positions
                WHERE status = 'OPEN'
                  AND account_id = $1
                GROUP BY event_id, market_side
                ORDER BY market, side
                "#,
            )
            .bind(state.account_id.as_str())
            .fetch_all(state.store.pool())
            .await
            .unwrap_or_default();

            let positions = rows
                .into_iter()
                .map(|(market, side, size, pnl_usd)| SidecarRiskPosition {
                    market,
                    side: if side == "UP" { "Yes" } else { "No" }.to_string(),
                    size,
                    pnl_usd: pnl_usd.unwrap_or(0.0),
                })
                .collect();

            let row = load_daily_metrics_halt_event(
                state.store.pool(),
                state.account_id.as_str(),
                daily_metrics_scope,
            )
            .await;

            let mut circuit_breaker_events = match row {
                Some((true, reason, updated_at)) => vec![SidecarCircuitBreakerEvent {
                    timestamp: updated_at.to_rfc3339(),
                    reason: reason.unwrap_or_else(|| "halted".to_string()),
                    state: "Halted".to_string(),
                }],
                _ => Vec::new(),
            };
            if let Some(runtime_event) = runtime_event {
                circuit_breaker_events.push(runtime_event);
            }

            Ok(Json(SidecarRiskState {
                risk_state,
                daily_pnl_usd: daily_pnl.to_f64().unwrap_or(0.0),
                daily_loss_limit_usd: daily_loss_limit.to_f64().unwrap_or(0.0),
                current_drawdown_usd: current_drawdown.to_f64().unwrap_or(0.0),
                max_drawdown_observed_usd: max_drawdown_observed.to_f64().unwrap_or(0.0),
                drawdown_limit_usd: std::env::var("PLOY_RISK__MAX_DRAWDOWN_USD")
                    .ok()
                    .and_then(|v| Decimal::from_str(v.trim()).ok())
                    .and_then(|v| v.to_f64()),
                queue_depth: 0,
                positions,
                circuit_breaker_events,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_risk_fallback_daily_metrics_scope, DailyMetricsScope};

    #[test]
    fn risk_fallback_scope_allows_global_daily_metrics_when_other_tables_are_scoped() {
        assert_eq!(
            resolve_risk_fallback_daily_metrics_scope(true, false, true),
            Ok(DailyMetricsScope::Global)
        );
    }

    #[test]
    fn risk_fallback_scope_requires_runtime_state_and_positions_account_scope() {
        assert_eq!(
            resolve_risk_fallback_daily_metrics_scope(false, true, false),
            Err(vec![
                "risk_runtime_state.account_id",
                "positions.account_id"
            ])
        );
    }
}
