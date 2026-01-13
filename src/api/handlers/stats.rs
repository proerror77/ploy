use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use rust_decimal::Decimal;

use crate::api::{
    state::AppState,
    types::*,
};


/// GET /api/stats/today
pub async fn get_today_stats(
    State(state): State<AppState>,
) -> std::result::Result<Json<TodayStats>, (StatusCode, String)> {
    let store = &state.store;

    // Query today's stats from database
    let stats = sqlx::query!(
        r#"
        SELECT
            COUNT(*) as "total_trades!",
            COUNT(*) FILTER (WHERE state = 'COMPLETED') as "successful_trades!",
            COUNT(*) FILTER (WHERE state = 'FAILED') as "failed_trades!",
            COALESCE(SUM(leg1_shares * leg1_price + leg2_shares * leg2_price), 0) as "total_volume!: Decimal",
            COALESCE(SUM(pnl), 0) as "pnl!: Decimal",
            COALESCE(AVG(EXTRACT(EPOCH FROM (updated_at - created_at)) * 1000), 0) as "avg_trade_time_ms!: f64"
        FROM cycles
        WHERE created_at >= CURRENT_DATE
        "#
    )
    .fetch_one(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let active_positions = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM cycles
        WHERE state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
        "#
    )
    .fetch_one(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let win_rate = if stats.total_trades > 0 {
        stats.successful_trades as f64 / stats.total_trades as f64
    } else {
        0.0
    };

    Ok(Json(TodayStats {
        total_trades: stats.total_trades,
        successful_trades: stats.successful_trades,
        failed_trades: stats.failed_trades,
        total_volume: stats.total_volume,
        pnl: stats.pnl,
        win_rate,
        avg_trade_time_ms: stats.avg_trade_time_ms as i64,
        active_positions,
    }))
}

/// GET /api/stats/pnl?hours=24
pub async fn get_pnl_history(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> std::result::Result<Json<Vec<PnLDataPoint>>, (StatusCode, String)> {
    let hours: i32 = params
        .get("hours")
        .and_then(|h| h.parse().ok())
        .unwrap_or(24);

    let store = &state.store;

    let data_points = sqlx::query!(
        r#"
        SELECT
            date_trunc('hour', created_at) as "timestamp!",
            SUM(COALESCE(pnl, 0)) OVER (ORDER BY date_trunc('hour', created_at)) as "cumulative_pnl!: Decimal",
            COUNT(*) as "trade_count!"
        FROM cycles
        WHERE created_at > NOW() - ($1 || ' hours')::INTERVAL
          AND pnl IS NOT NULL
        GROUP BY date_trunc('hour', created_at)
        ORDER BY timestamp
        "#,
        hours
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let result = data_points
        .into_iter()
        .map(|row| PnLDataPoint {
            timestamp: row.timestamp,
            cumulative_pnl: row.cumulative_pnl,
            trade_count: row.trade_count,
        })
        .collect();

    Ok(Json(result))
}

/// GET /api/trades
pub async fn get_trades(
    State(state): State<AppState>,
    Query(query): Query<TradeQuery>,
) -> std::result::Result<Json<TradesListResponse>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(20).min(100);
    let offset = query.offset.unwrap_or(0);

    let store = &state.store;

    // Build WHERE clause
    let mut where_clauses = vec!["1=1".to_string()];
    if let Some(ref status) = query.status {
        where_clauses.push(format!("state = '{}'", status));
    }
    if let Some(start_time) = query.start_time {
        where_clauses.push(format!("created_at >= '{}'", start_time));
    }
    if let Some(end_time) = query.end_time {
        where_clauses.push(format!("created_at <= '{}'", end_time));
    }
    let where_clause = where_clauses.join(" AND ");

    // Get total count
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM cycles WHERE {}",
        where_clause
    ))
    .fetch_one(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Get trades
    let trades = sqlx::query!(
        r#"
        SELECT
            id,
            created_at,
            leg1_token_id,
            leg1_side,
            leg1_shares,
            leg1_price,
            leg2_price,
            pnl,
            state,
            error_message
        FROM cycles
        WHERE 1=1
        ORDER BY created_at DESC
        LIMIT $1 OFFSET $2
        "#,
        limit,
        offset
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let trade_responses: Vec<TradeResponse> = trades
        .into_iter()
        .map(|row| TradeResponse {
            id: row.id.to_string(),
            timestamp: row.created_at,
            token_id: row.leg1_token_id.clone(),
            token_name: row.leg1_token_id.split('-').next().unwrap_or("Unknown").to_string(),
            side: row.leg1_side.clone(),
            shares: row.leg1_shares,
            entry_price: row.leg1_price,
            exit_price: row.leg2_price,
            pnl: row.pnl,
            status: row.state.clone(),
            error_message: row.error_message,
        })
        .collect();

    Ok(Json(TradesListResponse {
        trades: trade_responses,
        total,
    }))
}

/// GET /api/trades/:id
pub async fn get_trade_by_id(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<Json<TradeResponse>, (StatusCode, String)> {
    let store = &state.store;

    let trade = sqlx::query!(
        r#"
        SELECT
            id,
            created_at,
            leg1_token_id,
            leg1_side,
            leg1_shares,
            leg1_price,
            leg2_price,
            pnl,
            state,
            error_message
        FROM cycles
        WHERE id = $1
        "#,
        uuid::Uuid::from_str(&id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    )
    .fetch_optional(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "Trade not found".to_string()))?;

    Ok(Json(TradeResponse {
        id: trade.id.to_string(),
        timestamp: trade.created_at,
        token_id: trade.leg1_token_id.clone(),
        token_name: trade.leg1_token_id.split('-').next().unwrap_or("Unknown").to_string(),
        side: trade.leg1_side.clone(),
        shares: trade.leg1_shares,
        entry_price: trade.leg1_price,
        exit_price: trade.leg2_price,
        pnl: trade.pnl,
        status: trade.state.clone(),
        error_message: trade.error_message,
    }))
}

/// GET /api/positions
pub async fn get_positions(
    State(state): State<AppState>,
) -> std::result::Result<Json<Vec<PositionResponse>>, (StatusCode, String)> {
    let store = &state.store;

    let positions = sqlx::query!(
        r#"
        SELECT
            id,
            created_at,
            leg1_token_id,
            leg1_side,
            leg1_shares,
            leg1_price,
            state
        FROM cycles
        WHERE state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let position_responses: Vec<PositionResponse> = positions
        .into_iter()
        .map(|row| {
            let duration = Utc::now().timestamp() - row.created_at.timestamp();
            // TODO: Get current price from market data
            let current_price = row.leg1_price; // Placeholder
            let unrealized_pnl = Decimal::ZERO; // Placeholder

            PositionResponse {
                token_id: row.leg1_token_id.clone(),
                token_name: row.leg1_token_id.split('-').next().unwrap_or("Unknown").to_string(),
                side: row.leg1_side.clone(),
                shares: row.leg1_shares,
                entry_price: row.leg1_price,
                current_price,
                unrealized_pnl,
                entry_time: row.created_at,
                duration_seconds: duration,
            }
        })
        .collect();

    Ok(Json(position_responses))
}
