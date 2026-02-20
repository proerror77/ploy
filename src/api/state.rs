use crate::adapters::PostgresStore;
use crate::agent::grok::GrokClient;
use crate::api::types::WsMessage;
use crate::coordinator::CoordinatorHandle;
use crate::platform::StrategyDeployment;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// Persistence path for deployment matrix state.
    pub deployments_path: Arc<PathBuf>,
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
    fn parse_deployments(raw: &str) -> HashMap<String, StrategyDeployment> {
        let mut out = HashMap::new();
        if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
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

    fn deployments_state_path() -> PathBuf {
        std::env::var("PLOY_DEPLOYMENTS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/state/deployments.json"))
    }

    fn load_deployments(path: &Path) -> HashMap<String, StrategyDeployment> {
        let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
            .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
            .unwrap_or_default();
        if !raw.trim().is_empty() {
            return Self::parse_deployments(&raw);
        }

        if let Ok(contents) = std::fs::read_to_string(path) {
            return Self::parse_deployments(&contents);
        }

        HashMap::new()
    }

    pub async fn persist_deployments(&self) -> std::result::Result<(), String> {
        let mut items = {
            let deployments = self.deployments.read().await;
            deployments.values().cloned().collect::<Vec<_>>()
        };
        items.sort_by(|a, b| a.id.cmp(&b.id));

        let payload = serde_json::to_vec_pretty(&items)
            .map_err(|e| format!("failed to serialize deployments: {}", e))?;
        let path = self.deployments_path.as_ref().clone();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("failed to create deployment state dir: {}", e))?;
        }
        tokio::fs::write(&path, payload)
            .await
            .map_err(|e| format!("failed to write deployment state file: {}", e))
    }

    pub fn new(store: Arc<PostgresStore>, config: StrategyConfigState) -> Self {
        let (ws_tx, _) = broadcast::channel(1000);
        let deployments_path = Arc::new(Self::deployments_state_path());
        let deployments = Arc::new(RwLock::new(Self::load_deployments(
            deployments_path.as_ref(),
        )));

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
            deployments,
            deployments_path,
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
        let deployments_path = Arc::new(Self::deployments_state_path());
        let deployments = Arc::new(RwLock::new(Self::load_deployments(
            deployments_path.as_ref(),
        )));

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
            deployments,
            deployments_path,
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
