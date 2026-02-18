//! Coordinator Commands — control messages between coordinator and agents

use tokio::sync::oneshot;

use super::state::AgentSnapshot;

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
