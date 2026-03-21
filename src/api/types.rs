use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use crate::plugins::DeploymentState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorAction {
    Pause,
    Resume,
    ForceClose,
    ClaimCheck,
    ClaimRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorScope {
    Global,
    Domain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorActionRequest {
    pub action: OperatorAction,
    pub scope: OperatorScope,
    #[serde(default)]
    pub domain: Option<String>,
    pub requested_by: String,
    #[serde(default)]
    pub reason: Option<String>,
}

impl OperatorActionRequest {
    pub fn validate(&self) -> Option<String> {
        match self.scope {
            OperatorScope::Global => None,
            OperatorScope::Domain => self
                .domain
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|_| ())
                .map_or_else(
                    || Some("domain scope requires a domain".to_string()),
                    |_| None,
                ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorActionResponse {
    pub accepted: bool,
    pub action_id: String,
    pub action: OperatorAction,
    pub scope: OperatorScope,
    pub effective_targets: Vec<String>,
    pub message: String,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorDomainStatus {
    pub domain: String,
    pub ingress_mode: String,
    pub paused: bool,
    pub exposure_usd: f64,
    pub daily_pnl_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorClaimerStatus {
    pub enabled: bool,
    pub pending_redeemable_count: u64,
    pub pending_redeemable_notional_usd: f64,
    #[serde(default)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorRecentAction {
    pub action_id: String,
    pub action: OperatorAction,
    pub scope: OperatorScope,
    #[serde(default)]
    pub domain: Option<String>,
    pub accepted: bool,
    pub message: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorStatusResponse {
    pub runtime_mode: String,
    pub account_id: String,
    pub dry_run: bool,
    pub system_status: String,
    pub risk_state: String,
    pub queue_depth: u64,
    pub domains: Vec<OperatorDomainStatus>,
    pub claimer: OperatorClaimerStatus,
    pub recent_actions: Vec<OperatorRecentAction>,
}

// ============================================================================
// Stats Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodayStats {
    pub total_trades: i64,
    pub successful_trades: i64,
    pub failed_trades: i64,
    pub total_volume: f64,
    pub pnl: f64,
    pub win_rate: f64,
    pub avg_trade_time_ms: i64,
    pub active_positions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnLDataPoint {
    pub timestamp: DateTime<Utc>,
    pub cumulative_pnl: f64,
    pub trade_count: i64,
}

// ============================================================================
// Trade Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResponse {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradesListResponse {
    pub trades: Vec<TradeResponse>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct TradeQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}

// ============================================================================
// Position Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionResponse {
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub entry_time: DateTime<Utc>,
    pub duration_seconds: i64,
}

// ============================================================================
// Health Check Types
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub db: String,
    pub uptime_secs: i64,
}

// ============================================================================
// System Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub status: String,
    pub uptime_seconds: i64,
    pub version: String,
    pub strategy: String,
    pub last_trade_time: Option<DateTime<Utc>>,
    pub websocket_connected: bool,
    pub database_connected: bool,
    pub error_count_1h: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemControlResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentStateSummary {
    pub enabled: usize,
    pub draining: usize,
    pub disabled: usize,
    pub archived: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCapabilitySummary {
    pub plugin_id: String,
    pub kind: String,
    pub version: String,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBudgetSummary {
    pub available_notional_usd: String,
    pub reserved_notional_usd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapabilities {
    pub account_id: String,
    pub runtime_mode: String,
    pub execution_plane: String,
    pub dry_run: bool,
    pub coordinator_running: bool,
    pub supported_domains: Vec<String>,
    pub active_domains: Vec<String>,
    pub total_deployments: usize,
    pub enabled_deployments: usize,
    pub scoped_total_deployments: usize,
    pub scoped_enabled_deployments: usize,
    pub deployment_states: DeploymentStateSummary,
    pub scoped_deployment_states: DeploymentStateSummary,
    pub deployments_by_domain: HashMap<String, usize>,
    pub available_plugins: Vec<PluginCapabilitySummary>,
    pub system_controls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRuntimeSummary {
    pub account_id: String,
    pub wallet_address: Option<String>,
    pub label: Option<String>,
    pub runtime_active: bool,
    pub deployment_total: usize,
    pub deployment_enabled: usize,
    pub deployment_states: DeploymentStateSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsOverview {
    pub runtime_account_id: String,
    pub dry_run: bool,
    pub runtime_budget: AccountBudgetSummary,
    pub accounts: Vec<AccountRuntimeSummary>,
}

// ============================================================================
// Config Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyConfig {
    pub symbols: Vec<String>,
    pub min_move: f64,
    pub max_entry: f64,
    pub shares: i32,
    pub predictive: bool,
    #[serde(default)]
    pub exit_edge_floor: Option<f64>,
    #[serde(default)]
    pub exit_price_band: Option<f64>,
    #[serde(default)]
    pub time_decay_exit_secs: Option<u64>,
    #[serde(default)]
    pub liquidity_exit_spread_bps: Option<u32>,
}

// ============================================================================
// Strategy Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningStrategy {
    pub name: String,
    pub status: String,
    pub pnl_usd: f64,
    pub order_count: u64,
    pub domain: String,
    pub win_rate: Option<f64>,
    pub loss_streak: Option<u32>,
    pub size_multiplier: Option<f64>,
    pub settled_trades: Option<u64>,
    pub daily_realized_pnl_usd: Option<f64>,
}

// ============================================================================
// Security Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub severity: String,
    pub details: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct SecurityEventQuery {
    pub limit: Option<i64>,
    pub severity: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
}

// ============================================================================
// WebSocket Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsMessage {
    #[serde(rename = "log")]
    Log(LogEntry),
    #[serde(rename = "trade")]
    Trade(TradeResponse),
    #[serde(rename = "position")]
    Position(PositionResponse),
    #[serde(rename = "market")]
    Market(MarketData),
    #[serde(rename = "status")]
    Status(StatusUpdate),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub component: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketData {
    pub token_id: String,
    pub token_name: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub last_price: f64,
    pub volume_24h: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::{
        OperatorAction, OperatorActionRequest, OperatorActionResponse, OperatorClaimerStatus,
        OperatorDomainStatus, OperatorRecentAction, OperatorScope, OperatorStatusResponse,
        StrategyConfig,
    };
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn strategy_config_rejects_deprecated_take_profit_stop_loss_fields() {
        let payload = json!({
            "symbols": ["BTCUSDT"],
            "min_move": 0.1,
            "max_entry": 1.0,
            "shares": 10,
            "predictive": false,
            "take_profit": 0.02,
            "stop_loss": 0.05
        });

        let parsed = serde_json::from_value::<StrategyConfig>(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn operator_action_request_serializes_pause_domain() {
        let req = OperatorActionRequest {
            action: OperatorAction::Pause,
            scope: OperatorScope::Domain,
            domain: Some("crypto".to_string()),
            requested_by: "test".to_string(),
            reason: Some("ops".to_string()),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"action\":\"pause\""));
        assert!(json.contains("\"scope\":\"domain\""));
        assert!(json.contains("\"domain\":\"crypto\""));
    }

    #[test]
    fn operator_action_response_serializes_claim_run_receipt() {
        let resp = OperatorActionResponse {
            accepted: true,
            action_id: "act-123".to_string(),
            action: OperatorAction::ClaimRun,
            scope: OperatorScope::Global,
            effective_targets: vec!["global".to_string()],
            message: "claim started".to_string(),
            requested_at: Utc::now(),
        };

        let json = serde_json::to_value(resp).unwrap();
        assert_eq!(json["accepted"], true);
        assert_eq!(json["action"], "claim_run");
        assert_eq!(json["scope"], "global");
    }

    #[test]
    fn operator_status_response_serializes_operator_snapshot() {
        let status = OperatorStatusResponse {
            runtime_mode: "platform".to_string(),
            account_id: "default".to_string(),
            dry_run: true,
            system_status: "running".to_string(),
            risk_state: "normal".to_string(),
            queue_depth: 2,
            domains: vec![OperatorDomainStatus {
                domain: "crypto".to_string(),
                ingress_mode: "enabled".to_string(),
                paused: false,
                exposure_usd: 12.5,
                daily_pnl_usd: 1.25,
            }],
            claimer: OperatorClaimerStatus {
                enabled: false,
                pending_redeemable_count: 0,
                pending_redeemable_notional_usd: 0.0,
                last_checked_at: None,
                last_run_at: None,
                last_error: None,
            },
            recent_actions: vec![OperatorRecentAction {
                action_id: "act-1".to_string(),
                action: OperatorAction::Pause,
                scope: OperatorScope::Global,
                domain: None,
                accepted: true,
                message: "paused".to_string(),
                requested_by: "test".to_string(),
                requested_at: Utc::now(),
            }],
        };

        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["runtime_mode"], "platform");
        assert_eq!(json["domains"][0]["domain"], "crypto");
        assert_eq!(json["claimer"]["enabled"], false);
        assert_eq!(json["recent_actions"][0]["action"], "pause");
    }

    #[test]
    fn operator_domain_scope_requires_domain() {
        let req = OperatorActionRequest {
            action: OperatorAction::Pause,
            scope: OperatorScope::Domain,
            domain: None,
            requested_by: "test".to_string(),
            reason: None,
        };

        assert_eq!(
            req.validate().as_deref(),
            Some("domain scope requires a domain")
        );
    }
}
