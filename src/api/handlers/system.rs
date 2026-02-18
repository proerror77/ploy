use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use sqlx::{postgres::Postgres, QueryBuilder, Row};

use crate::api::{
    state::{AppState, SystemRunStatus},
    types::*,
};

#[derive(Debug, Deserialize)]
pub struct DomainControlRequest {
    pub domain: Option<String>,
}

/// GET /health -- lightweight liveness/readiness probe
pub async fn health_handler(
    State(state): State<AppState>,
) -> std::result::Result<Json<HealthResponse>, (StatusCode, Json<HealthResponse>)> {
    let db_status = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(state.store.pool())
        .await
    {
        Ok(_) => "connected".to_string(),
        Err(_) => "disconnected".to_string(),
    };

    let ok = db_status == "connected";
    let resp = HealthResponse {
        status: if ok { "ok".to_string() } else { "degraded".to_string() },
        db: db_status,
        uptime_secs: state.uptime_seconds(),
    };

    if ok {
        Ok(Json(resp))
    } else {
        Err((StatusCode::SERVICE_UNAVAILABLE, Json(resp)))
    }
}

/// GET /api/system/status
pub async fn get_system_status(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemStatus>, (StatusCode, String)> {
    let status_state = state.system_status.read().await;

    // Get error count from last hour
    let error_count = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::BIGINT as count
        FROM security_audit_log
        WHERE timestamp > NOW() - INTERVAL '1 hour'
          AND severity IN ('HIGH', 'CRITICAL')
        "#,
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
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_START', 'LOW', 'System started via API', '{}')
        "#,
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
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_STOP', 'LOW', 'System stopped via API', '{}')
        "#,
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
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_RESTART', 'MEDIUM', 'System restarted via API', '{}')
        "#,
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已重启".to_string(),
    }))
}

/// POST /api/system/pause
pub async fn pause_system(
    State(state): State<AppState>,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let _domain = req.and_then(|Json(r)| r.domain);
    if let Some(coordinator) = state.coordinator.as_ref() {
        coordinator
            .pause_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        // Fallback for standalone API mode: reflect pause in system status only.
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Stopped;
        drop(status_state);
        state.broadcast(WsMessage::Status(StatusUpdate {
            status: "stopped".to_string(),
        }));
    }

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已暂停所有策略".to_string(),
    }))
}

/// POST /api/system/resume
pub async fn resume_system(
    State(state): State<AppState>,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let _domain = req.and_then(|Json(r)| r.domain);
    if let Some(coordinator) = state.coordinator.as_ref() {
        coordinator
            .resume_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        // Fallback for standalone API mode: reflect resume in system status only.
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Running;
        drop(status_state);
        state.broadcast(WsMessage::Status(StatusUpdate {
            status: "running".to_string(),
        }));
    }

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已恢复所有策略".to_string(),
    }))
}

/// POST /api/system/halt
///
/// Force-close all positions and mark the system as stopped.
pub async fn halt_system(
    State(state): State<AppState>,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    let _domain = req.and_then(|Json(r)| r.domain);
    if let Some(coordinator) = state.coordinator.as_ref() {
        coordinator
            .force_close_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Update system status and broadcast
    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Stopped;
    }
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已紧急停止并强制平仓".to_string(),
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
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('CONFIG_UPDATE', 'MEDIUM', 'Strategy config updated via API', $1)
        "#,
    )
    .bind(serde_json::to_value(&*config).unwrap())
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

    let mut qb = QueryBuilder::<Postgres>::new(
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
        "#,
    );

    if let Some(ref severity) = query.severity {
        qb.push(" AND severity = ").push_bind(severity);
    }
    if let Some(start_time) = query.start_time {
        qb.push(" AND timestamp >= ").push_bind(start_time);
    }
    qb.push(" ORDER BY timestamp DESC LIMIT ").push_bind(limit);

    let rows = qb
        .build()
        .fetch_all(state.store.pool())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut security_events = Vec::with_capacity(rows.len());
    for row in rows {
        let id = if let Ok(v) = row.try_get::<uuid::Uuid, _>("id") {
            v.to_string()
        } else if let Ok(v) = row.try_get::<i64, _>("id") {
            v.to_string()
        } else if let Ok(v) = row.try_get::<String, _>("id") {
            v
        } else {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "unsupported security_audit_log.id type".to_string(),
            ));
        };
        let timestamp = row
            .try_get("timestamp")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let event_type = row
            .try_get("event_type")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let severity = row
            .try_get("severity")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let details = row
            .try_get("details")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let metadata = row.try_get("metadata").ok();
        security_events.push(SecurityEvent {
            id,
            timestamp,
            event_type,
            severity,
            details,
            metadata,
        });
    }

    Ok(Json(security_events))
}
