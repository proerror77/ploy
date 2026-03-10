use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// POST /api/sidecar/orders — request body
#[derive(Debug, Deserialize)]
pub struct SidecarOrderRequest {
    pub strategy: String,
    pub account_id: Option<String>,
    pub deployment_id: Option<String>,
    pub domain: Option<String>,
    pub market_slug: String,
    pub token_id: String,
    pub side: Option<String>,
    pub is_buy: Option<bool>,
    pub shares: u64,
    pub price: f64,
    pub idempotency_key: Option<String>,
    pub dry_run: Option<bool>,
    #[serde(alias = "grok_decision_id")]
    pub decision_request_id: Option<String>,
    #[serde(alias = "reasoning")]
    pub decision_reasoning: Option<String>,
    pub edge: Option<f64>,
    pub confidence: Option<f64>,
}

/// POST /api/sidecar/orders — response
#[derive(Debug, Serialize)]
pub struct SidecarOrderResponse {
    pub success: bool,
    pub intent_id: Option<String>,
    pub message: String,
    pub dry_run: bool,
}

/// POST /api/sidecar/intents — request body
#[derive(Debug, Deserialize)]
pub struct SidecarIntentRequest {
    pub intent_id: Option<String>,
    pub account_id: Option<String>,
    pub deployment_id: String,
    pub agent_id: Option<String>,
    pub domain: Option<String>,
    pub market_slug: String,
    pub token_id: String,
    pub side: Option<String>,
    pub order_side: Option<String>,
    pub is_buy: Option<bool>,
    pub size: u64,
    pub price_limit: f64,
    pub idempotency_key: Option<String>,
    pub reason: Option<String>,
    pub confidence: Option<f64>,
    pub edge: Option<f64>,
    pub priority: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    pub dry_run: Option<bool>,
}

/// POST /api/sidecar/intents — response
#[derive(Debug, Serialize)]
pub struct SidecarIntentResponse {
    pub success: bool,
    pub intent_id: String,
    pub message: String,
    pub dry_run: bool,
}

/// GET /api/sidecar/positions — response item
#[derive(Debug, Serialize)]
pub struct SidecarPosition {
    pub id: i64,
    pub market_slug: String,
    pub token_id: String,
    pub side: String,
    pub shares: i64,
    pub avg_price: f64,
    pub current_value: Option<f64>,
    pub pnl: Option<f64>,
    pub status: String,
    pub opened_at: String,
}

/// GET /api/sidecar/risk — response
#[derive(Debug, Serialize)]
pub struct SidecarRiskState {
    pub risk_state: String,
    pub daily_pnl_usd: f64,
    pub daily_loss_limit_usd: f64,
    pub current_drawdown_usd: f64,
    pub max_drawdown_observed_usd: f64,
    pub drawdown_limit_usd: Option<f64>,
    pub queue_depth: usize,
    pub positions: Vec<SidecarRiskPosition>,
    pub circuit_breaker_events: Vec<SidecarCircuitBreakerEvent>,
}

#[derive(Debug, Serialize)]
pub struct SidecarRiskPosition {
    pub market: String,
    pub side: String,
    pub size: f64,
    pub pnl_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct SidecarCircuitBreakerEvent {
    pub timestamp: String,
    pub reason: String,
    pub state: String,
}
