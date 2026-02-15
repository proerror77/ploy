use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use rust_decimal::Decimal;
use sqlx::{postgres::Postgres, QueryBuilder, Row};
use std::str::FromStr;
use uuid::Uuid;

use crate::api::{state::AppState, types::*};

/// GET /api/stats/today
pub async fn get_today_stats(
    State(state): State<AppState>,
) -> std::result::Result<Json<TodayStats>, (StatusCode, String)> {
    let store = &state.store;

    let stats = sqlx::query(
        r#"
        SELECT
            COUNT(*)::BIGINT as total_trades,
            COUNT(*) FILTER (WHERE state = 'COMPLETED')::BIGINT as successful_trades,
            COUNT(*) FILTER (WHERE state = 'FAILED')::BIGINT as failed_trades,
            COALESCE(SUM(leg1_shares * leg1_price + leg2_shares * leg2_price), 0)::numeric as total_volume,
            COALESCE(SUM(pnl), 0)::numeric as pnl,
            COALESCE(AVG(EXTRACT(EPOCH FROM (updated_at - created_at)) * 1000), 0)::double precision as avg_trade_time_ms
        FROM cycles
        WHERE created_at >= CURRENT_DATE
        "#
    )
    .fetch_one(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total_trades: i64 = stats.try_get("total_trades").unwrap_or(0);
    let successful_trades: i64 = stats.try_get("successful_trades").unwrap_or(0);
    let failed_trades: i64 = stats.try_get("failed_trades").unwrap_or(0);
    let total_volume: Decimal = stats.try_get("total_volume").unwrap_or(Decimal::ZERO);
    let pnl: Decimal = stats.try_get("pnl").unwrap_or(Decimal::ZERO);
    let avg_trade_time_ms: f64 = stats.try_get("avg_trade_time_ms").unwrap_or(0.0);

    let active_positions: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::BIGINT as count
        FROM cycles
        WHERE state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
        "#,
    )
    .fetch_one(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let win_rate = if total_trades > 0 {
        successful_trades as f64 / total_trades as f64
    } else {
        0.0
    };

    Ok(Json(TodayStats {
        total_trades,
        successful_trades,
        failed_trades,
        total_volume,
        pnl,
        win_rate,
        avg_trade_time_ms: avg_trade_time_ms as i64,
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

    let data_points = sqlx::query(
        r#"
        SELECT
            date_trunc('hour', created_at) as timestamp,
            SUM(COALESCE(pnl, 0)) OVER (ORDER BY date_trunc('hour', created_at)) as cumulative_pnl,
            COUNT(*)::BIGINT as trade_count
        FROM cycles
        WHERE created_at > NOW() - ($1 || ' hours')::INTERVAL
          AND pnl IS NOT NULL
        GROUP BY date_trunc('hour', created_at)
        ORDER BY timestamp
        "#,
    )
    .bind(hours)
    .fetch_all(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut result = Vec::with_capacity(data_points.len());
    for row in data_points {
        let timestamp = row
            .try_get("timestamp")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let cumulative_pnl = row.try_get("cumulative_pnl").unwrap_or(Decimal::ZERO);
        let trade_count = row.try_get("trade_count").unwrap_or(0_i64);
        result.push(PnLDataPoint {
            timestamp,
            cumulative_pnl,
            trade_count,
        });
    }

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

    let mut count_qb =
        QueryBuilder::<Postgres>::new("SELECT COUNT(*)::BIGINT as total FROM cycles WHERE 1=1");
    if let Some(ref status) = query.status {
        count_qb.push(" AND state = ").push_bind(status);
    }
    if let Some(start_time) = query.start_time {
        count_qb.push(" AND created_at >= ").push_bind(start_time);
    }
    if let Some(end_time) = query.end_time {
        count_qb.push(" AND created_at <= ").push_bind(end_time);
    }
    let total_row = count_qb
        .build()
        .fetch_one(store.pool())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let total: i64 = total_row.try_get("total").unwrap_or(0);

    let mut trades_qb = QueryBuilder::<Postgres>::new(
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
        "#,
    );
    if let Some(ref status) = query.status {
        trades_qb.push(" AND state = ").push_bind(status);
    }
    if let Some(start_time) = query.start_time {
        trades_qb.push(" AND created_at >= ").push_bind(start_time);
    }
    if let Some(end_time) = query.end_time {
        trades_qb.push(" AND created_at <= ").push_bind(end_time);
    }
    trades_qb
        .push(" ORDER BY created_at DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);

    let trades = trades_qb
        .build()
        .fetch_all(store.pool())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut trade_responses = Vec::with_capacity(trades.len());
    for row in trades {
        let id: Uuid = row
            .try_get("id")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let timestamp = row
            .try_get("created_at")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let token_id: String = row
            .try_get("leg1_token_id")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let side: String = row
            .try_get("leg1_side")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let shares: i32 = row
            .try_get("leg1_shares")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let entry_price: Decimal = row
            .try_get("leg1_price")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let exit_price: Option<Decimal> = row.try_get("leg2_price").ok();
        let pnl: Option<Decimal> = row.try_get("pnl").ok();
        let status: String = row
            .try_get("state")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let error_message: Option<String> = row.try_get("error_message").ok();

        trade_responses.push(TradeResponse {
            id: id.to_string(),
            timestamp,
            token_id: token_id.clone(),
            token_name: token_id.split('-').next().unwrap_or("Unknown").to_string(),
            side,
            shares,
            entry_price,
            exit_price,
            pnl,
            status,
            error_message,
        });
    }

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
    let trade_id = Uuid::from_str(&id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let trade = sqlx::query(
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
    )
    .bind(trade_id)
    .fetch_optional(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| (StatusCode::NOT_FOUND, "Trade not found".to_string()))?;

    let id: Uuid = trade
        .try_get("id")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let timestamp = trade
        .try_get("created_at")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let token_id: String = trade
        .try_get("leg1_token_id")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let side: String = trade
        .try_get("leg1_side")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let shares: i32 = trade
        .try_get("leg1_shares")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let entry_price: Decimal = trade
        .try_get("leg1_price")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let exit_price: Option<Decimal> = trade.try_get("leg2_price").ok();
    let pnl: Option<Decimal> = trade.try_get("pnl").ok();
    let status: String = trade
        .try_get("state")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let error_message: Option<String> = trade.try_get("error_message").ok();

    Ok(Json(TradeResponse {
        id: id.to_string(),
        timestamp,
        token_id: token_id.clone(),
        token_name: token_id.split('-').next().unwrap_or("Unknown").to_string(),
        side,
        shares,
        entry_price,
        exit_price,
        pnl,
        status,
        error_message,
    }))
}

/// GET /api/positions
pub async fn get_positions(
    State(state): State<AppState>,
) -> std::result::Result<Json<Vec<PositionResponse>>, (StatusCode, String)> {
    let store = &state.store;

    let positions = sqlx::query(
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
        "#,
    )
    .fetch_all(store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut position_responses = Vec::with_capacity(positions.len());
    for row in positions {
        let entry_time: chrono::DateTime<chrono::Utc> = row
            .try_get("created_at")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let token_id: String = row
            .try_get("leg1_token_id")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let side: String = row
            .try_get("leg1_side")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let shares: i32 = row
            .try_get("leg1_shares")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let entry_price: Decimal = row
            .try_get("leg1_price")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let duration = Utc::now().timestamp() - entry_time.timestamp();
        let current_price = entry_price;
        let unrealized_pnl = Decimal::ZERO;

        position_responses.push(PositionResponse {
            token_id: token_id.clone(),
            token_name: token_id.split('-').next().unwrap_or("Unknown").to_string(),
            side,
            shares,
            entry_price,
            current_price,
            unrealized_pnl,
            entry_time,
            duration_seconds: duration,
        });
    }

    Ok(Json(position_responses))
}
