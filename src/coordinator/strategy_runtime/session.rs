use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::{sync::Mutex, task::JoinHandle};
use tracing::warn;

use crate::agent_runtime::AgentStatus;
use crate::strategy::StrategyManager;

pub(super) struct ManagedRuntimeSession {
    pub(super) strategy_id: String,
    pub(super) started_at: DateTime<Utc>,
    pub(super) manager: Arc<StrategyManager>,
    pub(super) paused: Arc<AtomicBool>,
    pub(super) runtime_alive: Arc<AtomicBool>,
    pub(super) orders_submitted: Arc<AtomicU64>,
    pub(super) orders_filled: Arc<AtomicU64>,
    pub(super) status: AgentStatus,
    pub(super) split_arb_poll_registry: Arc<Mutex<HashSet<String>>>,
    pub(super) subscribed_token_count: usize,
    pub(super) action_task: JoinHandle<()>,
}

impl ManagedRuntimeSession {
    pub(super) async fn shutdown(self, strategy_label: &str, agent_id: &str) {
        self.runtime_alive.store(false, Ordering::Release);
        self.split_arb_poll_registry.lock().await.clear();
        if let Err(error) = self.manager.stop_all(true).await {
            warn!(
                strategy = strategy_label,
                agent_id = agent_id,
                strategy_id = %self.strategy_id,
                error = %error,
                "managed strategy runtime stop_all failed"
            );
        }
        self.action_task.abort();
    }
}
