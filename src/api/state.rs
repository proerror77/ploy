use crate::adapters::PostgresStore;
use crate::agent::grok::GrokClient;
use crate::api::types::{MarketData, PositionResponse, TradeResponse, WsMessage};
use crate::coordinator::CoordinatorHandle;
use crate::platform::{Domain, StrategyDeployment};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::time::Duration;

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

    /// Runtime mode (`standalone` or `platform`).
    pub runtime_mode: String,

    /// Account scope for this process/runtime.
    pub account_id: String,

    /// Whether execution is dry-run for this runtime.
    pub dry_run: bool,

    /// Coordinator handle for sidecar order submission (optional — only set when platform is running)
    pub coordinator: Option<CoordinatorHandle>,

    /// Grok client for sidecar unified decisions (optional — only set when GROK_API_KEY is present)
    pub grok_client: Option<Arc<GrokClient>>,

    /// Strategy deployment matrix (control-plane first-class resource).
    pub deployments: Arc<RwLock<HashMap<String, StrategyDeployment>>>,
    /// Persistence path for deployment matrix state.
    pub deployments_path: Arc<PathBuf>,
    /// Runtime-allowed domains for this process.
    pub allowed_domains: Arc<std::collections::HashSet<Domain>>,
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
    fn default_allowed_domains() -> std::collections::HashSet<Domain> {
        let mut domains = std::collections::HashSet::new();
        domains.insert(Domain::Crypto);
        domains.insert(Domain::Sports);
        domains.insert(Domain::Politics);
        domains.insert(Domain::Economics);
        domains
    }

    fn normalize_account_id(raw: Option<&str>) -> String {
        raw.map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| "default".to_string())
    }

    fn parse_boolish(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }

    fn default_account_id_from_env() -> String {
        Self::normalize_account_id(std::env::var("PLOY_ACCOUNT_ID").ok().as_deref())
    }

    fn default_dry_run_from_env() -> bool {
        std::env::var("PLOY_DRY_RUN__ENABLED")
            .or_else(|_| std::env::var("PLOY_DRY_RUN"))
            .ok()
            .map(|v| Self::parse_boolish(&v))
            .unwrap_or(true)
    }

    fn parse_deployments(raw: &str) -> HashMap<String, StrategyDeployment> {
        let mut out = HashMap::new();
        if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
            for mut deployment in items {
                let id = deployment.id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                deployment.id = id.clone();
                deployment.normalize_account_ids_in_place();
                out.insert(id, deployment);
            }
        }
        out
    }

    fn deployments_state_path() -> PathBuf {
        if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
            return PathBuf::from(path);
        }
        let container_data_root = Path::new("/opt/ploy/data");
        if container_data_root.exists() {
            return container_data_root.join("state/deployments.json");
        }
        let repo_state_deployment = Path::new("data/state/deployments.json");
        if repo_state_deployment.exists() {
            return repo_state_deployment.to_path_buf();
        }
        let repo_root_deployment = Path::new("deployment/deployments.json");
        if repo_root_deployment.exists() {
            return repo_root_deployment.to_path_buf();
        }
        let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
        if container_deployment.exists() {
            return container_deployment.to_path_buf();
        }
        PathBuf::from("data/state/deployments.json")
    }

    fn load_deployments(path: &Path) -> HashMap<String, StrategyDeployment> {
        let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
            .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
            .unwrap_or_default();
        if !raw.trim().is_empty() {
            return Self::parse_deployments(&raw);
        }

        let repo_state_path = Path::new("data/state/deployments.json");
        let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
        let deployment_file_candidates = [
            path.to_path_buf(),
            repo_state_path.to_path_buf(),
            container_data_path.to_path_buf(),
            Path::new("deployment/deployments.json").to_path_buf(),
            Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
        ];

        for candidate in deployment_file_candidates {
            if let Ok(contents) = std::fs::read_to_string(&candidate) {
                let items = Self::parse_deployments(&contents);
                if !items.is_empty() {
                    return items;
                }
            }
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
        let allowed_domains = Arc::new(Self::default_allowed_domains());
        let account_id = Self::default_account_id_from_env();

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
            runtime_mode: "standalone".to_string(),
            account_id,
            dry_run: Self::default_dry_run_from_env(),
            coordinator: None,
            grok_client: None,
            deployments,
            deployments_path,
            allowed_domains,
        }
    }

    /// Create AppState with coordinator and Grok client (for platform mode)
    pub fn with_platform_services(
        store: Arc<PostgresStore>,
        config: StrategyConfigState,
        coordinator: Option<CoordinatorHandle>,
        grok_client: Option<Arc<GrokClient>>,
        account_id: String,
        dry_run: bool,
    ) -> Self {
        let (ws_tx, _) = broadcast::channel(1000);
        let deployments_path = Arc::new(Self::deployments_state_path());
        let (deployments, allowed_domains) = if let Some(coordinator_ref) = coordinator.as_ref() {
            (
                coordinator_ref.shared_deployments(),
                coordinator_ref.allowed_domains(),
            )
        } else {
            (
                Arc::new(RwLock::new(Self::load_deployments(
                    deployments_path.as_ref(),
                ))),
                Arc::new(Self::default_allowed_domains()),
            )
        };
        let account_id = Self::normalize_account_id(Some(account_id.as_str()));

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
            runtime_mode: "platform".to_string(),
            account_id,
            dry_run,
            coordinator,
            grok_client,
            deployments,
            deployments_path,
            allowed_domains,
        }
    }

    pub fn is_domain_allowed(&self, domain: Domain) -> bool {
        self.allowed_domains.contains(&domain)
    }

    pub fn allowed_domains_labels(&self) -> Vec<String> {
        let mut labels = self
            .allowed_domains
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        labels.sort();
        labels
    }

    /// Broadcast a WebSocket message to all connected clients
    pub fn broadcast(&self, msg: WsMessage) {
        let _ = self.ws_tx.send(msg);
    }

    /// Get system uptime in seconds
    pub fn uptime_seconds(&self) -> i64 {
        (Utc::now() - self.start_time).num_seconds()
    }

    /// Broadcast DB-backed realtime updates so UI receives trade/position/market
    /// events even when orders are submitted by internal runtime agents.
    pub fn spawn_realtime_broadcast_loop(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            let mut last_trade_key: Option<String> = None;
            let mut last_positions_sig = String::new();
            let mut last_market_key: Option<String> = None;

            loop {
                tick.tick().await;

                // Latest trade from cycles.
                if let Ok(Some(row)) = sqlx::query(
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
                    ORDER BY created_at DESC
                    LIMIT 1
                    "#,
                )
                .fetch_optional(state.store.pool())
                .await
                {
                    let id = if let Ok(v) = row.try_get::<uuid::Uuid, _>("id") {
                        v.to_string()
                    } else if let Ok(v) = row.try_get::<String, _>("id") {
                        v
                    } else {
                        String::new()
                    };
                    if !id.is_empty() && last_trade_key.as_deref() != Some(id.as_str()) {
                        let timestamp: DateTime<Utc> =
                            row.try_get("created_at").unwrap_or_else(|_| Utc::now());
                        let token_id: String = row.try_get("leg1_token_id").unwrap_or_default();
                        let side: String = row.try_get("leg1_side").unwrap_or_default();
                        let shares: i32 = row.try_get("leg1_shares").unwrap_or(0);
                        let entry_price: Decimal =
                            row.try_get("leg1_price").unwrap_or(Decimal::ZERO);
                        let exit_price = row
                            .try_get::<Decimal, _>("leg2_price")
                            .ok()
                            .and_then(|d| d.to_f64());
                        let pnl = row
                            .try_get::<Decimal, _>("pnl")
                            .ok()
                            .and_then(|d| d.to_f64());
                        let status: String = row.try_get("state").unwrap_or_default();
                        let error_message: Option<String> = row.try_get("error_message").ok();

                        state.broadcast(WsMessage::Trade(TradeResponse {
                            id: id.clone(),
                            timestamp,
                            token_id: token_id.clone(),
                            token_name: token_id.split('-').next().unwrap_or("Unknown").to_string(),
                            side,
                            shares,
                            entry_price: entry_price.to_f64().unwrap_or(0.0),
                            exit_price,
                            pnl,
                            status,
                            error_message,
                        }));
                        last_trade_key = Some(id);
                    }
                }

                // Active positions from cycles.
                if let Ok(rows) = sqlx::query(
                    r#"
                    SELECT
                        id,
                        created_at,
                        leg1_token_id,
                        leg1_side,
                        leg1_shares,
                        leg1_price
                    FROM cycles
                    WHERE state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
                    ORDER BY created_at DESC
                    LIMIT 50
                    "#,
                )
                .fetch_all(state.store.pool())
                .await
                {
                    let mut sig_parts: Vec<String> = Vec::with_capacity(rows.len());
                    for row in &rows {
                        let id = if let Ok(v) = row.try_get::<uuid::Uuid, _>("id") {
                            v.to_string()
                        } else if let Ok(v) = row.try_get::<String, _>("id") {
                            v
                        } else {
                            String::new()
                        };
                        if !id.is_empty() {
                            sig_parts.push(id);
                        }
                    }
                    let sig = sig_parts.join(",");
                    if sig != last_positions_sig {
                        for row in rows {
                            let entry_time: DateTime<Utc> =
                                row.try_get("created_at").unwrap_or_else(|_| Utc::now());
                            let token_id: String = row.try_get("leg1_token_id").unwrap_or_default();
                            let side: String = row.try_get("leg1_side").unwrap_or_default();
                            let shares: i32 = row.try_get("leg1_shares").unwrap_or(0);
                            let entry_price: Decimal =
                                row.try_get("leg1_price").unwrap_or(Decimal::ZERO);
                            let entry_price_f64 = entry_price.to_f64().unwrap_or(0.0);
                            let duration_seconds = (Utc::now() - entry_time).num_seconds();

                            state.broadcast(WsMessage::Position(PositionResponse {
                                token_id: token_id.clone(),
                                token_name: token_id
                                    .split('-')
                                    .next()
                                    .unwrap_or("Unknown")
                                    .to_string(),
                                side,
                                shares,
                                entry_price: entry_price_f64,
                                current_price: entry_price_f64,
                                unrealized_pnl: 0.0,
                                entry_time,
                                duration_seconds,
                            }));
                        }
                        last_positions_sig = sig;
                    }
                }

                // Latest market quote.
                if let Ok(Some(row)) = sqlx::query(
                    r#"
                    SELECT token_id, best_bid, best_ask, received_at
                    FROM clob_quote_ticks
                    ORDER BY received_at DESC
                    LIMIT 1
                    "#,
                )
                .fetch_optional(state.store.pool())
                .await
                {
                    let token_id: String = row.try_get("token_id").unwrap_or_default();
                    let received_at: DateTime<Utc> =
                        row.try_get("received_at").unwrap_or_else(|_| Utc::now());
                    let market_key = format!("{}:{}", token_id, received_at.timestamp_micros());
                    if !token_id.is_empty()
                        && last_market_key.as_deref() != Some(market_key.as_str())
                    {
                        let best_bid = row
                            .try_get::<Decimal, _>("best_bid")
                            .ok()
                            .and_then(|d| d.to_f64())
                            .unwrap_or(0.0);
                        let best_ask = row
                            .try_get::<Decimal, _>("best_ask")
                            .ok()
                            .and_then(|d| d.to_f64())
                            .unwrap_or(0.0);
                        state.broadcast(WsMessage::Market(MarketData {
                            token_id: token_id.clone(),
                            token_name: token_id.clone(),
                            best_bid,
                            best_ask,
                            spread: (best_ask - best_bid).max(0.0),
                            last_price: if best_bid > 0.0 && best_ask > 0.0 {
                                (best_bid + best_ask) / 2.0
                            } else {
                                best_ask.max(best_bid)
                            },
                            volume_24h: 0.0,
                            timestamp: received_at,
                        }));
                        last_market_key = Some(market_key);
                    }
                }
            }
        });
    }
}
