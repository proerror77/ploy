//! Checkpoint Service
//!
//! Provides periodic state snapshots for crash recovery.
//! Checkpoints are created:
//! - On a regular interval (default: 5 minutes)
//! - On state transitions
//! - Before shutdown

use crate::adapters::TransactionManager;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Configuration for checkpoint service
#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    /// Interval between automatic checkpoints (default: 300s / 5 min)
    pub interval_secs: u64,
    /// Maximum checkpoints to keep per component (default: 10)
    pub max_checkpoints_per_component: u32,
    /// Whether to create checkpoint on state transitions
    pub checkpoint_on_transition: bool,
    /// Whether to create checkpoint before shutdown
    pub checkpoint_on_shutdown: bool,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            interval_secs: 300,
            max_checkpoints_per_component: 10,
            checkpoint_on_transition: true,
            checkpoint_on_shutdown: true,
        }
    }
}

/// Checkpoint data structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub checkpoint_type: String,
    pub component: String,
    pub data: serde_json::Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
}

/// Trait for types that can be checkpointed
pub trait Checkpointable: Send + Sync {
    /// Get checkpoint type identifier
    fn checkpoint_type(&self) -> &str;

    /// Get component name
    fn component_name(&self) -> &str;

    /// Serialize current state to JSON
    fn to_checkpoint(&self) -> serde_json::Value;

    /// Restore state from checkpoint
    fn from_checkpoint(&mut self, data: &serde_json::Value) -> Result<(), String>;

    /// Get current version (for optimistic locking)
    fn version(&self) -> i32;
}

/// Checkpoint service for managing state snapshots
pub struct CheckpointService {
    config: CheckpointConfig,
    transaction_manager: Arc<TransactionManager>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl CheckpointService {
    /// Create a new checkpoint service
    pub fn new(config: CheckpointConfig, transaction_manager: Arc<TransactionManager>) -> Self {
        Self {
            config,
            transaction_manager,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create with default configuration
    pub fn with_defaults(transaction_manager: Arc<TransactionManager>) -> Self {
        Self::new(CheckpointConfig::default(), transaction_manager)
    }

    /// Create a checkpoint for a component
    pub async fn create_checkpoint<T: Checkpointable>(&self, component: &T) -> crate::error::Result<i64> {
        let checkpoint_type = component.checkpoint_type();
        let component_name = component.component_name();
        let data = component.to_checkpoint();
        let version = component.version();

        let id = self
            .transaction_manager
            .save_snapshot(checkpoint_type, component_name, data, version)
            .await?;

        info!(
            "Created checkpoint {} for {}/{} (version {})",
            id, checkpoint_type, component_name, version
        );

        Ok(id)
    }

    /// Restore a component from its latest checkpoint
    pub async fn restore_checkpoint<T: Checkpointable>(
        &self,
        component: &mut T,
    ) -> crate::error::Result<Option<i64>> {
        // Convert to owned strings to avoid borrow issues
        let checkpoint_type = component.checkpoint_type().to_string();
        let component_name = component.component_name().to_string();

        match self
            .transaction_manager
            .get_latest_snapshot(&checkpoint_type, &component_name)
            .await?
        {
            Some((id, data, version)) => {
                match component.from_checkpoint(&data) {
                    Ok(()) => {
                        info!(
                            "Restored checkpoint {} for {}/{} (version {})",
                            id, checkpoint_type, component_name, version
                        );
                        Ok(Some(id))
                    }
                    Err(e) => {
                        error!(
                            "Failed to restore checkpoint {} for {}/{}: {}",
                            id, checkpoint_type, component_name, e
                        );
                        Err(crate::error::PloyError::InvalidState(format!(
                            "Checkpoint restore failed: {}",
                            e
                        )))
                    }
                }
            }
            None => {
                debug!(
                    "No checkpoint found for {}/{}",
                    checkpoint_type, component_name
                );
                Ok(None)
            }
        }
    }

    /// Start the periodic checkpoint task
    pub async fn start<F, Fut>(&self, get_components: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Vec<Box<dyn Checkpointable>>> + Send,
    {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        info!(
            "Checkpoint service started (interval: {}s)",
            self.config.interval_secs
        );

        let interval = Duration::from_secs(self.config.interval_secs);
        let running = self.running.clone();
        let tm = self.transaction_manager.clone();

        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);

            while running.load(std::sync::atomic::Ordering::SeqCst) {
                timer.tick().await;

                let components = get_components().await;
                let mut success_count = 0;
                let mut error_count = 0;

                for component in components.iter() {
                    let checkpoint_type = component.checkpoint_type();
                    let component_name = component.component_name();
                    let data = component.to_checkpoint();
                    let version = component.version();

                    match tm
                        .save_snapshot(checkpoint_type, component_name, data, version)
                        .await
                    {
                        Ok(_) => success_count += 1,
                        Err(e) => {
                            error!(
                                "Failed to checkpoint {}/{}: {}",
                                checkpoint_type, component_name, e
                            );
                            error_count += 1;
                        }
                    }
                }

                if error_count > 0 {
                    warn!(
                        "Checkpoint cycle: {} succeeded, {} failed",
                        success_count, error_count
                    );
                } else {
                    debug!("Checkpoint cycle: {} components saved", success_count);
                }
            }

            info!("Checkpoint service stopped");
        });
    }

    /// Stop the periodic checkpoint task
    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Create checkpoints for all components (on-demand)
    pub async fn checkpoint_all(
        &self,
        components: &[&dyn Checkpointable],
    ) -> crate::error::Result<Vec<i64>> {
        let mut ids = Vec::new();

        for component in components {
            let checkpoint_type = component.checkpoint_type();
            let component_name = component.component_name();
            let data = component.to_checkpoint();
            let version = component.version();

            let id = self
                .transaction_manager
                .save_snapshot(checkpoint_type, component_name, data, version)
                .await?;

            ids.push(id);
        }

        info!("Created {} checkpoints", ids.len());
        Ok(ids)
    }

    /// Check if a checkpoint exists for a component
    pub async fn has_checkpoint(
        &self,
        checkpoint_type: &str,
        component: &str,
    ) -> crate::error::Result<bool> {
        let snapshot = self
            .transaction_manager
            .get_latest_snapshot(checkpoint_type, component)
            .await?;

        Ok(snapshot.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockComponent {
        name: String,
        state: i32,
        version: i32,
    }

    impl Checkpointable for MockComponent {
        fn checkpoint_type(&self) -> &str {
            "mock"
        }

        fn component_name(&self) -> &str {
            &self.name
        }

        fn to_checkpoint(&self) -> serde_json::Value {
            serde_json::json!({
                "state": self.state,
                "name": self.name
            })
        }

        fn from_checkpoint(&mut self, data: &serde_json::Value) -> Result<(), String> {
            self.state = data["state"]
                .as_i64()
                .ok_or("missing state")?
                as i32;
            Ok(())
        }

        fn version(&self) -> i32 {
            self.version
        }
    }

    #[test]
    fn test_mock_checkpoint() {
        let component = MockComponent {
            name: "test".to_string(),
            state: 42,
            version: 1,
        };

        let data = component.to_checkpoint();
        assert_eq!(data["state"], 42);
        assert_eq!(data["name"], "test");
    }
}
