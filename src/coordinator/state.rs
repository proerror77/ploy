//! Coordinator State — shared global state across all agents

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::platform::{
    AgentStatus, AggregatedPosition, CircuitBreakerEvent, Domain, PlatformRiskState, Position,
    QueueStats,
};

/// Per-agent snapshot visible to the coordinator and TUI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub agent_id: String,
    pub name: String,
    pub domain: Domain,
    pub status: AgentStatus,
    pub position_count: usize,
    pub exposure: Decimal,
    pub daily_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    /// Strategy-specific metrics (string map for extensibility across agents).
    pub metrics: HashMap<String, String>,
    pub last_heartbeat: DateTime<Utc>,
    pub error_message: Option<String>,
}

/// Serializable queue stats snapshot (mirrors QueueStats from platform)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueStatsSnapshot {
    pub current_size: usize,
    pub max_size: usize,
    pub enqueued_total: u64,
    pub dequeued_total: u64,
    pub expired_total: u64,
}

impl From<QueueStats> for QueueStatsSnapshot {
    fn from(qs: QueueStats) -> Self {
        Self {
            current_size: qs.current_size,
            max_size: qs.max_size,
            enqueued_total: qs.enqueued_total,
            dequeued_total: qs.dequeued_total,
            expired_total: qs.expired_total,
        }
    }
}

/// Platform-wide state aggregated from all agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalState {
    /// Per-agent snapshots keyed by agent_id
    pub agents: HashMap<String, AgentSnapshot>,
    /// Aggregated portfolio across all agents
    pub portfolio: AggregatedPosition,
    /// Open positions across all agents (best-effort)
    pub positions: Vec<Position>,
    /// Current risk state (Normal / Elevated / Halted)
    pub risk_state: PlatformRiskState,
    /// Daily PnL (risk-gate tracked)
    pub daily_pnl: Decimal,
    /// Daily loss limit (risk-gate configured)
    pub daily_loss_limit: Decimal,
    /// Circuit breaker event history
    pub circuit_breaker_events: Vec<CircuitBreakerEvent>,
    /// Order queue statistics
    pub queue_stats: QueueStatsSnapshot,
    /// Total realized PnL across all agents
    pub total_realized_pnl: Decimal,
    /// Coordinator start time
    pub started_at: DateTime<Utc>,
    /// Last time state was refreshed
    pub last_refresh: DateTime<Utc>,
}

impl GlobalState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            agents: HashMap::new(),
            portfolio: AggregatedPosition::default(),
            positions: Vec::new(),
            risk_state: PlatformRiskState::Normal,
            daily_pnl: Decimal::ZERO,
            daily_loss_limit: Decimal::ZERO,
            circuit_breaker_events: Vec::new(),
            queue_stats: QueueStatsSnapshot::default(),
            total_realized_pnl: Decimal::ZERO,
            started_at: now,
            last_refresh: now,
        }
    }

    /// Number of agents currently in Running status
    pub fn active_agent_count(&self) -> usize {
        self.agents
            .values()
            .filter(|a| matches!(a.status, AgentStatus::Running))
            .count()
    }

    /// Total exposure across all agents
    pub fn total_exposure(&self) -> Decimal {
        self.portfolio.total_exposure
    }

    /// Total unrealized PnL
    pub fn total_unrealized_pnl(&self) -> Decimal {
        self.portfolio.unrealized_pnl
    }
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}
