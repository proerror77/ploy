use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStatus {
    pub status: String,
    pub uptime_seconds: i64,
    pub version: String,
    pub strategy: String,
    pub last_trade_time: Option<DateTime<Utc>>,
    pub websocket_connected: bool,
    pub database_connected: bool,
    pub error_count_1h: i64,
    #[serde(default)]
    pub last_claim_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub degraded_claim_accounts: usize,
    #[serde(default)]
    pub pending_redeemable_count: usize,
    #[serde(default)]
    pub pending_redeemable_notional: Decimal,
    #[serde(default)]
    pub live_reconcile_failures: u32,
    #[serde(default)]
    pub next_live_reconcile_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_live_reconcile_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemControlResponse {
    pub success: bool,
    pub message: String,
}
