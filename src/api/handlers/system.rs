use axum::{
    extract::State,
    http::StatusCode,
    Json,
};


use crate::api::{
    state::{AppState, SystemRunStatus},
    types::*,
};


/// GET /api/system/status
pub async fn get_system_status(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemStatus>, (StatusCode, String)> {
    let status_state = state.system_status.read().await;

    // Get error count from last hour
    let error_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM security_audit_log
        WHERE timestamp > NOW() - INTERVAL '1 hour'
          AND severity IN ('HIGH', 'CRITICAL')
        "#
    )
    .fetch_one(state.store.pool())
    .await
    .unwrap_or(0);

    Ok(Json(SystemStatus {
        status: status_state.status.as_str().to_string(),
        uptime_seconds: state.uptime_seconds(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        strategy: "momentum".to_string(), // TODO: Get from config
        last_trade_time: status_state.last_trade_time,
        websocket_connected: status_state.websocket_connected,
        database_connected: status_state.database_connected,
        error_count_1h: error_count,
    }))
}

/// POST /api/system/start
pub async fn start_system(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let mut status_state = state.system_status.write().await;

    if status_state.status == SystemRunStatus::Running {
        return Ok(Json(SystemControlResponse {
            success: false,
            message: "系统已在运行中".to_string(),
        }));
    }

    status_state.status = SystemRunStatus::Running;

    // Broadcast status update
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "running".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query!(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_START', 'LOW', 'System started via API', '{}')
        "#
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已启动".to_string(),
    }))
}

/// POST /api/system/stop
pub async fn stop_system(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let mut status_state = state.system_status.write().await;

    if status_state.status == SystemRunStatus::Stopped {
        return Ok(Json(SystemControlResponse {
            success: false,
            message: "系统已停止".to_string(),
        }));
    }

    status_state.status = SystemRunStatus::Stopped;

    // Broadcast status update
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query!(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_STOP', 'LOW', 'System stopped via API', '{}')
        "#
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已停止".to_string(),
    }))
}

/// POST /api/system/restart
pub async fn restart_system(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let mut status_state = state.system_status.write().await;

    // Stop first
    status_state.status = SystemRunStatus::Stopped;
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    // Wait a moment
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Start again
    status_state.status = SystemRunStatus::Running;
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "running".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query!(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_RESTART', 'MEDIUM', 'System restarted via API', '{}')
        "#
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已重启".to_string(),
    }))
}

/// GET /api/config
pub async fn get_config(
    State(state): State<AppState>,
) -> std::result::Result<Json<StrategyConfig>, (StatusCode, String)> {
    let config = state.config.read().await;

    Ok(Json(StrategyConfig {
        symbols: config.symbols.clone(),
        min_move: config.min_move,
        max_entry: config.max_entry,
        shares: config.shares,
        predictive: config.predictive,
        take_profit: config.take_profit,
        stop_loss: config.stop_loss,
    }))
}

/// PUT /api/config
pub async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<StrategyConfig>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut config = state.config.write().await;

    // Update config
    config.symbols = new_config.symbols;
    config.min_move = new_config.min_move;
    config.max_entry = new_config.max_entry;
    config.shares = new_config.shares;
    config.predictive = new_config.predictive;
    config.take_profit = new_config.take_profit;
    config.stop_loss = new_config.stop_loss;

    // Log to audit
    let _ = sqlx::query!(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('CONFIG_UPDATE', 'MEDIUM', 'Strategy config updated via API', $1)
        "#,
        serde_json::to_value(&*config).unwrap()
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// GET /api/security/events
pub async fn get_security_events(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<SecurityEventQuery>,
) -> std::result::Result<Json<Vec<SecurityEvent>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(100).min(500);

    let mut where_clauses = vec!["1=1".to_string()];
    if let Some(ref severity) = query.severity {
        where_clauses.push(format!("severity = '{}'", severity));
    }
    if let Some(start_time) = query.start_time {
        where_clauses.push(format!("timestamp >= '{}'", start_time));
    }
    let where_clause = where_clauses.join(" AND ");

    let events = sqlx::query!(
        r#"
        SELECT
            id,
            timestamp,
            event_type,
            severity,
            details,
            metadata
        FROM security_audit_log
        WHERE 1=1
        ORDER BY timestamp DESC
        LIMIT $1
        "#,
        limit
    )
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let security_events: Vec<SecurityEvent> = events
        .into_iter()
        .map(|row| SecurityEvent {
            id: row.id.to_string(),
            timestamp: row.timestamp,
            event_type: row.event_type,
            severity: row.severity,
            details: row.details,
            metadata: row.metadata,
        })
        .collect();

    Ok(Json(security_events))
}
