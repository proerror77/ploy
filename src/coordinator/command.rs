//! Coordinator Commands — control messages between coordinator and agents

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::state::{AgentSnapshot, QueueStatsSnapshot};
use crate::platform::PlatformRiskState;

/// Commands sent from the coordinator to individual agents
#[derive(Debug)]
pub enum CoordinatorCommand {
    /// Pause trading — agent should stop submitting orders but keep data feeds alive
    Pause,
    /// Resume trading after a pause
    Resume,
    /// Force-close all positions and stop
    ForceClose,
    /// Graceful shutdown — finish pending work, then stop
    Shutdown,
    /// Health check request — agent should respond with current state
    HealthCheck(oneshot::Sender<AgentHealthResponse>),
}

/// Control commands sent to the coordinator (broadcast to agents)
#[derive(Debug, Clone)]
pub enum CoordinatorControlCommand {
    /// Pause all agents (stop submitting orders, keep feeds alive)
    PauseAll,
    /// Resume all agents after pause
    ResumeAll,
    /// Force-close all positions and stop agents
    ForceCloseAll,
    /// Graceful shutdown for all agents
    ShutdownAll,
}

/// Response to a HealthCheck command
#[derive(Debug, Clone)]
pub struct AgentHealthResponse {
    pub snapshot: AgentSnapshot,
    pub is_healthy: bool,
    pub uptime_secs: u64,
    pub orders_submitted: u64,
    pub orders_filled: u64,
}

/// Runtime governance policy exposed to control-plane APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernancePolicySnapshot {
    pub block_new_intents: bool,
    pub blocked_domains: Vec<String>,
    pub max_intent_notional_usd: Option<Decimal>,
    pub max_total_notional_usd: Option<Decimal>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
    pub reason: Option<String>,
}

/// Full replacement payload for runtime governance policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernancePolicyUpdate {
    pub block_new_intents: bool,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    pub max_intent_notional_usd: Option<Decimal>,
    pub max_total_notional_usd: Option<Decimal>,
    pub updated_by: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Per-domain allocator ledger snapshot (account-level capital tracking).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocatorLedgerSnapshot {
    pub domain: String,
    pub enabled: bool,
    pub cap_notional_usd: Decimal,
    pub open_notional_usd: Decimal,
    pub pending_notional_usd: Decimal,
    pub available_notional_usd: Decimal,
}

/// Runtime governance + risk + capital view for OpenClaw control-plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceStatusSnapshot {
    pub account_id: String,
    pub ingress_mode: String,
    pub policy: GovernancePolicySnapshot,
    pub account_notional_usd: Decimal,
    pub platform_exposure_usd: Decimal,
    pub risk_state: PlatformRiskState,
    pub daily_pnl_usd: Decimal,
    pub daily_loss_limit_usd: Decimal,
    pub queue: QueueStatsSnapshot,
    pub allocators: Vec<AllocatorLedgerSnapshot>,
    pub updated_at: DateTime<Utc>,
}
