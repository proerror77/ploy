use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

// ============================================================================
// Config Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub symbols: Vec<String>,
    pub min_move: f64,
    pub max_entry: f64,
    pub shares: i32,
    pub predictive: bool,
    pub take_profit: Option<f64>,
    pub stop_loss: Option<f64>,
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
