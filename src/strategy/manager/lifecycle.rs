use anyhow::anyhow;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use super::{Result, StrategyManager};
use crate::strategy::traits::{PositionInfo, Strategy, StrategyStateInfo};

/// A running strategy instance with its task handle
pub(super) struct RunningStrategy {
    /// The strategy instance
    pub(super) strategy: Arc<RwLock<Box<dyn Strategy>>>,
    /// Background task handle
    pub(super) task_handle: Option<JoinHandle<()>>,
    /// When the strategy was started
    pub(super) started_at: DateTime<Utc>,
    /// Configuration used to start the strategy
    pub(super) config_path: Option<String>,
}

impl StrategyManager {
    /// Start a strategy
    pub async fn start_strategy(
        &self,
        strategy: Box<dyn Strategy>,
        config_path: Option<String>,
    ) -> Result<()> {
        let strategy_id = strategy.id().to_string();
        let strategy_name = strategy.name().to_string();

        {
            let strategies = self.strategies.read().await;
            if strategies.contains_key(&strategy_id) {
                return Err(anyhow!("Strategy {} is already running", strategy_id).into());
            }
        }

        info!("Starting strategy: {} ({})", strategy_name, strategy_id);

        let strategy = Arc::new(RwLock::new(strategy));
        let required_feeds = {
            let s = strategy.read().await;
            s.required_feeds()
        };

        for feed in &required_feeds {
            debug!("Strategy {} subscribed to feed: {:?}", strategy_id, feed);
        }

        let task_handle = self
            .spawn_strategy_task(strategy_id.clone(), strategy.clone())
            .await;

        {
            let mut strategies = self.strategies.write().await;
            strategies.insert(
                strategy_id.clone(),
                RunningStrategy {
                    strategy,
                    task_handle: Some(task_handle),
                    started_at: Utc::now(),
                    config_path,
                },
            );
        }

        info!("Strategy {} started successfully", strategy_id);
        Ok(())
    }

    /// Stop a strategy
    pub async fn stop_strategy(&self, strategy_id: &str, graceful: bool) -> Result<()> {
        let mut strategies = self.strategies.write().await;

        let running = strategies
            .remove(strategy_id)
            .ok_or_else(|| anyhow!("Strategy {} is not running", strategy_id))?;

        info!("Stopping strategy: {}", strategy_id);

        if graceful {
            let actions = {
                let mut strategy = running.strategy.write().await;
                strategy.shutdown().await?
            };

            for action in actions {
                if let Err(err) = self.action_tx.send((strategy_id.to_string(), action)).await {
                    return Err(anyhow!(
                        "Failed to send shutdown action for strategy {}: {}",
                        strategy_id,
                        err
                    )
                    .into());
                }
            }
        }

        if let Some(handle) = running.task_handle {
            handle.abort();
        }

        info!("Strategy {} stopped", strategy_id);
        Ok(())
    }

    /// Stop all strategies
    pub async fn stop_all(&self, graceful: bool) -> Result<()> {
        let strategy_ids: Vec<String> = {
            let strategies = self.strategies.read().await;
            strategies.keys().cloned().collect()
        };

        for id in strategy_ids {
            if let Err(err) = self.stop_strategy(&id, graceful).await {
                error!("Error stopping strategy {}: {}", id, err);
            }
        }

        let _ = self.shutdown_tx.send(());
        Ok(())
    }

    /// Get status of all running strategies
    pub async fn get_status(&self) -> Vec<StrategyStatus> {
        let strategies = self.strategies.read().await;
        let mut statuses = Vec::with_capacity(strategies.len());

        for (id, running) in strategies.iter() {
            statuses.push(strategy_status(id, running).await);
        }

        statuses
    }

    /// Get status of a specific strategy
    pub async fn get_strategy_status(&self, strategy_id: &str) -> Option<StrategyStatus> {
        let strategies = self.strategies.read().await;
        let running = strategies.get(strategy_id)?;
        Some(strategy_status(strategy_id, running).await)
    }

    /// Get positions for a specific strategy
    pub async fn get_positions(&self, strategy_id: &str) -> Option<Vec<PositionInfo>> {
        let strategies = self.strategies.read().await;
        let running = strategies.get(strategy_id)?;
        let strategy = running.strategy.read().await;
        Some(strategy.positions())
    }

    /// List running strategy IDs
    pub async fn list_running(&self) -> Vec<String> {
        let strategies = self.strategies.read().await;
        strategies.keys().cloned().collect()
    }

    /// Check if a strategy is running
    pub async fn is_running(&self, strategy_id: &str) -> bool {
        let strategies = self.strategies.read().await;
        strategies.contains_key(strategy_id)
    }
}

async fn strategy_status(strategy_id: &str, running: &RunningStrategy) -> StrategyStatus {
    let strategy = running.strategy.read().await;
    let state = strategy.state();
    let positions = strategy.positions();

    StrategyStatus {
        id: strategy_id.to_string(),
        name: strategy.name().to_string(),
        state,
        position_count: positions.len(),
        started_at: running.started_at,
        config_path: running.config_path.clone(),
    }
}

/// Status information for a running strategy
#[derive(Debug, Clone)]
pub struct StrategyStatus {
    /// Strategy ID
    pub id: String,
    /// Strategy name
    pub name: String,
    /// Current state info
    pub state: StrategyStateInfo,
    /// Number of open positions
    pub position_count: usize,
    /// When the strategy was started
    pub started_at: DateTime<Utc>,
    /// Config file path (if started from config)
    pub config_path: Option<String>,
}

impl StrategyStatus {
    /// Get uptime as a human-readable string
    pub fn uptime(&self) -> String {
        let duration = Utc::now() - self.started_at;
        let hours = duration.num_hours();
        let minutes = duration.num_minutes() % 60;
        let seconds = duration.num_seconds() % 60;

        if hours > 0 {
            format!("{}h {}m {}s", hours, minutes, seconds)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
    }
}
