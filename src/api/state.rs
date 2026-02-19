use crate::adapters::PostgresStore;
use crate::agent::grok::GrokClient;
use crate::api::types::WsMessage;
use crate::coordinator::CoordinatorHandle;
use crate::platform::StrategyDeployment;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Shared application state for API handlers
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool
    pub store: Arc<PostgresStore>,

    /// WebSocket broadcast channel
    pub ws_tx: broadcast::Sender<WsMessage>,

    /// System status
    pub system_status: Arc<RwLock<SystemStatusState>>,

    /// Strategy configuration
    pub config: Arc<RwLock<StrategyConfigState>>,

    /// Application start time
    pub start_time: DateTime<Utc>,

    /// Coordinator handle for sidecar order submission (optional — only set when platform is running)
    pub coordinator: Option<CoordinatorHandle>,

    /// Grok client for sidecar unified decisions (optional — only set when GROK_API_KEY is present)
    pub grok_client: Option<Arc<GrokClient>>,

    /// Strategy deployment matrix (control-plane first-class resource).
    pub deployments: Arc<RwLock<HashMap<String, StrategyDeployment>>>,
}

#[derive(Debug, Clone)]
pub struct SystemStatusState {
    pub status: SystemRunStatus,
    pub last_trade_time: Option<DateTime<Utc>>,
    pub websocket_connected: bool,
    pub database_connected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRunStatus {
    Running,
    Stopped,
    Error,
}

impl SystemRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyConfigState {
    pub symbols: Vec<String>,
    pub min_move: f64,
    pub max_entry: f64,
    pub shares: i32,
    pub predictive: bool,
    /// Binary-options exit threshold based on modeled edge deterioration.
    pub exit_edge_floor: Option<f64>,
    /// Binary-options exit threshold based on adverse price move band.
    pub exit_price_band: Option<f64>,
    /// Optional minimum seconds before scheduled resolution to force time-based exit.
    pub time_decay_exit_secs: Option<u64>,
    /// Optional max spread (bps) for liquidity-based forced exit.
    pub liquidity_exit_spread_bps: Option<u32>,
}

impl AppState {
    fn load_deployments_from_env() -> HashMap<String, StrategyDeployment> {
        let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
            .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
            .unwrap_or_default();
        if raw.trim().is_empty() {
            return HashMap::new();
        }

        let mut out = HashMap::new();
        if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(&raw) {
            for deployment in items {
                let id = deployment.id.trim();
                if id.is_empty() {
                    continue;
                }
                out.insert(id.to_string(), deployment);
            }
        }
        out
    }

    pub fn new(store: Arc<PostgresStore>, config: StrategyConfigState) -> Self {
        let (ws_tx, _) = broadcast::channel(1000);

        Self {
            store,
            ws_tx,
            system_status: Arc::new(RwLock::new(SystemStatusState {
                status: SystemRunStatus::Stopped,
                last_trade_time: None,
                websocket_connected: false,
                database_connected: true,
            })),
            config: Arc::new(RwLock::new(config)),
            start_time: Utc::now(),
            coordinator: None,
            grok_client: None,
            deployments: Arc::new(RwLock::new(Self::load_deployments_from_env())),
        }
    }

    /// Create AppState with coordinator and Grok client (for platform mode)
    pub fn with_platform_services(
        store: Arc<PostgresStore>,
        config: StrategyConfigState,
        coordinator: Option<CoordinatorHandle>,
        grok_client: Option<Arc<GrokClient>>,
    ) -> Self {
        let (ws_tx, _) = broadcast::channel(1000);

        Self {
            store,
            ws_tx,
            system_status: Arc::new(RwLock::new(SystemStatusState {
                status: SystemRunStatus::Running, // Platform mode starts as Running
                last_trade_time: None,
                websocket_connected: false,
                database_connected: true,
            })),
            config: Arc::new(RwLock::new(config)),
            start_time: Utc::now(),
            coordinator,
            grok_client,
            deployments: Arc::new(RwLock::new(Self::load_deployments_from_env())),
        }
    }

    /// Broadcast a WebSocket message to all connected clients
    pub fn broadcast(&self, msg: WsMessage) {
        let _ = self.ws_tx.send(msg);
    }

    /// Get system uptime in seconds
    pub fn uptime_seconds(&self) -> i64 {
        (Utc::now() - self.start_time).num_seconds()
    }
}
