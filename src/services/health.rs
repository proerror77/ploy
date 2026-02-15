//! Health check HTTP server for 24/7 production monitoring
//!
//! Provides liveness and readiness probes for process supervision (systemd/launchd)
//! and Prometheus metrics endpoint.

use crate::domain::StrategyState;
use crate::services::Metrics;
use crate::strategy::RiskManager;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Health status for a component
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

/// Component health check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check: Option<DateTime<Utc>>,
}

/// Overall system health response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub timestamp: DateTime<Utc>,
    pub uptime_seconds: u64,
    pub components: Vec<ComponentHealth>,
    pub strategy_state: String,
    pub risk_state: String,
}

/// Shared state for health server
pub struct HealthState {
    /// When the server started
    pub started_at: DateTime<Utc>,
    /// Is WebSocket connected
    pub ws_connected: AtomicBool,
    /// Last WebSocket message timestamp
    pub last_ws_message: RwLock<Option<DateTime<Utc>>>,
    /// Is database connected
    pub db_connected: AtomicBool,
    /// Last database check timestamp
    pub last_db_check: RwLock<Option<DateTime<Utc>>>,
    /// Current strategy state
    pub strategy_state: RwLock<StrategyState>,
    /// Risk manager reference
    pub risk_manager: Option<Arc<RiskManager>>,
    /// Metrics reference
    pub metrics: Option<Arc<Metrics>>,
    /// Quote staleness threshold in seconds
    pub quote_staleness_threshold: u64,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            started_at: Utc::now(),
            ws_connected: AtomicBool::new(false),
            last_ws_message: RwLock::new(None),
            db_connected: AtomicBool::new(false),
            last_db_check: RwLock::new(None),
            strategy_state: RwLock::new(StrategyState::Idle),
            risk_manager: None,
            metrics: None,
            quote_staleness_threshold: 30, // 30 seconds default
        }
    }

    pub fn with_risk_manager(mut self, rm: Arc<RiskManager>) -> Self {
        self.risk_manager = Some(rm);
        self
    }

    pub fn with_metrics(mut self, m: Arc<Metrics>) -> Self {
        self.metrics = Some(m);
        self
    }

    /// Update WebSocket connection status
    pub fn set_ws_connected(&self, connected: bool) {
        self.ws_connected.store(connected, Ordering::SeqCst);
    }

    /// Record WebSocket message received
    pub async fn record_ws_message(&self) {
        *self.last_ws_message.write().await = Some(Utc::now());
        self.ws_connected.store(true, Ordering::SeqCst);
    }

    /// Update database connection status
    pub fn set_db_connected(&self, connected: bool) {
        self.db_connected.store(connected, Ordering::SeqCst);
    }

    /// Record database check
    pub async fn record_db_check(&self, success: bool) {
        *self.last_db_check.write().await = Some(Utc::now());
        self.db_connected.store(success, Ordering::SeqCst);
    }

    /// Update strategy state
    pub async fn set_strategy_state(&self, state: StrategyState) {
        *self.strategy_state.write().await = state;
    }

    /// Check if quotes are stale
    pub async fn is_quote_stale(&self) -> bool {
        if let Some(last) = *self.last_ws_message.read().await {
            let elapsed = (Utc::now() - last).num_seconds() as u64;
            elapsed > self.quote_staleness_threshold
        } else {
            true // No messages received yet
        }
    }

    /// Get overall health status
    pub async fn get_health(&self) -> HealthResponse {
        let mut components = Vec::new();
        let mut overall_status = HealthStatus::Healthy;

        // WebSocket health
        let ws_connected = self.ws_connected.load(Ordering::SeqCst);
        let quote_stale = self.is_quote_stale().await;
        let ws_status = if ws_connected && !quote_stale {
            HealthStatus::Healthy
        } else if ws_connected && quote_stale {
            HealthStatus::Degraded
        } else {
            HealthStatus::Unhealthy
        };
        if ws_status != HealthStatus::Healthy {
            overall_status = ws_status;
        }
        components.push(ComponentHealth {
            name: "websocket".to_string(),
            status: ws_status,
            message: if quote_stale {
                Some("Quotes are stale".to_string())
            } else if !ws_connected {
                Some("Disconnected".to_string())
            } else {
                None
            },
            last_check: *self.last_ws_message.read().await,
        });

        // Database health
        let db_connected = self.db_connected.load(Ordering::SeqCst);
        let db_status = if db_connected {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        };
        if db_status == HealthStatus::Unhealthy && overall_status == HealthStatus::Healthy {
            overall_status = HealthStatus::Degraded; // DB can be optional
        }
        components.push(ComponentHealth {
            name: "database".to_string(),
            status: db_status,
            message: if !db_connected {
                Some("Disconnected".to_string())
            } else {
                None
            },
            last_check: *self.last_db_check.read().await,
        });

        // Risk state
        let risk_status = if let Some(ref rm) = self.risk_manager {
            let state = rm.state().await;
            let status = match state {
                crate::domain::RiskState::Normal => HealthStatus::Healthy,
                crate::domain::RiskState::Elevated => HealthStatus::Degraded,
                crate::domain::RiskState::Halted => HealthStatus::Unhealthy,
            };
            if status == HealthStatus::Unhealthy {
                overall_status = HealthStatus::Unhealthy;
            } else if status == HealthStatus::Degraded && overall_status == HealthStatus::Healthy {
                overall_status = HealthStatus::Degraded;
            }
            (status, state.to_string())
        } else {
            (HealthStatus::Healthy, "unknown".to_string())
        };
        components.push(ComponentHealth {
            name: "risk_management".to_string(),
            status: risk_status.0,
            message: Some(risk_status.1.clone()),
            last_check: Some(Utc::now()),
        });

        let strategy_state = self.strategy_state.read().await;
        let uptime = (Utc::now() - self.started_at).num_seconds() as u64;

        HealthResponse {
            status: overall_status,
            timestamp: Utc::now(),
            uptime_seconds: uptime,
            components,
            strategy_state: strategy_state.to_string(),
            risk_state: risk_status.1,
        }
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

/// Health check server
pub struct HealthServer {
    state: Arc<HealthState>,
    port: u16,
}

impl HealthServer {
    pub fn new(state: Arc<HealthState>, port: u16) -> Self {
        Self { state, port }
    }

    /// Start the health server
    pub async fn run(&self) -> crate::Result<()> {
        let state = Arc::clone(&self.state);

        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/healthz", get(liveness_handler))
            .route("/readyz", get(readiness_handler))
            .route("/metrics", get(metrics_handler))
            .with_state(state);

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        info!("Starting health server on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app)
            .await
            .map_err(|e| crate::PloyError::Internal(format!("Health server error: {}", e)))?;

        Ok(())
    }

    /// Get shared state for updating from other components
    pub fn state(&self) -> Arc<HealthState> {
        Arc::clone(&self.state)
    }
}

/// Full health check endpoint
async fn health_handler(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    let health = state.get_health().await;
    let status_code = match health.status {
        HealthStatus::Healthy => StatusCode::OK,
        HealthStatus::Degraded => StatusCode::OK, // Still return 200 for degraded
        HealthStatus::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status_code, Json(health))
}

/// Kubernetes liveness probe - is the process alive?
async fn liveness_handler() -> impl IntoResponse {
    StatusCode::OK
}

/// Kubernetes readiness probe - is the service ready to handle traffic?
async fn readiness_handler(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    let health = state.get_health().await;
    match health.status {
        HealthStatus::Healthy | HealthStatus::Degraded => StatusCode::OK,
        HealthStatus::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Prometheus metrics endpoint
async fn metrics_handler(State(state): State<Arc<HealthState>>) -> impl IntoResponse {
    let health = state.get_health().await;
    let uptime = health.uptime_seconds;
    let ws_connected = if state.ws_connected.load(Ordering::SeqCst) {
        1
    } else {
        0
    };
    let db_connected = if state.db_connected.load(Ordering::SeqCst) {
        1
    } else {
        0
    };

    // Get metrics from Metrics struct if available
    let (quote_updates, orders_submitted, orders_filled, ws_reconnections) =
        if let Some(ref m) = state.metrics {
            (
                m.quote_updates.load(Ordering::Relaxed),
                m.orders_submitted.load(Ordering::Relaxed),
                m.orders_filled.load(Ordering::Relaxed),
                m.ws_reconnections.load(Ordering::Relaxed),
            )
        } else {
            (0, 0, 0, 0)
        };

    // Get risk metrics
    let (daily_pnl, cycle_count, consecutive_failures) = if let Some(ref rm) = state.risk_manager {
        let (pnl, cycles, _) = rm.daily_stats().await;
        (pnl.to_string(), cycles, rm.consecutive_failures())
    } else {
        ("0".to_string(), 0, 0)
    };

    let health_status = match health.status {
        HealthStatus::Healthy => 1,
        HealthStatus::Degraded => 0,
        HealthStatus::Unhealthy => -1,
    };

    let metrics = format!(
        r#"# HELP ploy_up Health status (1=healthy, 0=degraded, -1=unhealthy)
# TYPE ploy_up gauge
ploy_up {}

# HELP ploy_uptime_seconds Uptime in seconds
# TYPE ploy_uptime_seconds counter
ploy_uptime_seconds {}

# HELP ploy_websocket_connected WebSocket connection status
# TYPE ploy_websocket_connected gauge
ploy_websocket_connected {}

# HELP ploy_database_connected Database connection status
# TYPE ploy_database_connected gauge
ploy_database_connected {}

# HELP ploy_quote_updates_total Total quote updates processed
# TYPE ploy_quote_updates_total counter
ploy_quote_updates_total {}

# HELP ploy_orders_submitted_total Total orders submitted
# TYPE ploy_orders_submitted_total counter
ploy_orders_submitted_total {}

# HELP ploy_orders_filled_total Total orders filled
# TYPE ploy_orders_filled_total counter
ploy_orders_filled_total {}

# HELP ploy_ws_reconnections_total WebSocket reconnections
# TYPE ploy_ws_reconnections_total counter
ploy_ws_reconnections_total {}

# HELP ploy_daily_pnl_usd Daily profit/loss in USD
# TYPE ploy_daily_pnl_usd gauge
ploy_daily_pnl_usd {}

# HELP ploy_daily_cycles_total Daily cycle count
# TYPE ploy_daily_cycles_total counter
ploy_daily_cycles_total {}

# HELP ploy_consecutive_failures Current consecutive failures
# TYPE ploy_consecutive_failures gauge
ploy_consecutive_failures {}
"#,
        health_status,
        uptime,
        ws_connected,
        db_connected,
        quote_updates,
        orders_submitted,
        orders_filled,
        ws_reconnections,
        daily_pnl,
        cycle_count,
        consecutive_failures,
    );

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        metrics,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_state_new() {
        let state = HealthState::new();
        assert!(!state.ws_connected.load(Ordering::SeqCst));
        assert!(!state.db_connected.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_quote_staleness() {
        let state = HealthState::new();
        assert!(state.is_quote_stale().await);

        state.record_ws_message().await;
        assert!(!state.is_quote_stale().await);
    }

    #[tokio::test]
    async fn test_overall_health() {
        let state = HealthState::new();
        let health = state.get_health().await;

        // Should be unhealthy when WS is not connected
        assert_eq!(health.status, HealthStatus::Unhealthy);
    }
}
