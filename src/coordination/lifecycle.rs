//! Component Lifecycle Management
//!
//! Manages ordered startup and shutdown of system components with state tracking.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// Component lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    /// Component is stopped
    Stopped,
    /// Component is starting up
    Starting,
    /// Component is running normally
    Running,
    /// Component is running but degraded
    Degraded,
    /// Component is shutting down
    Stopping,
    /// Component has failed
    Failed,
}

impl ComponentState {
    /// Check if component is in a healthy state
    pub fn is_healthy(&self) -> bool {
        matches!(self, ComponentState::Running)
    }

    /// Check if component can accept work
    pub fn can_work(&self) -> bool {
        matches!(self, ComponentState::Running | ComponentState::Degraded)
    }

    /// Check if component is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(self, ComponentState::Stopped | ComponentState::Failed)
    }
}

impl std::fmt::Display for ComponentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComponentState::Stopped => write!(f, "stopped"),
            ComponentState::Starting => write!(f, "starting"),
            ComponentState::Running => write!(f, "running"),
            ComponentState::Degraded => write!(f, "degraded"),
            ComponentState::Stopping => write!(f, "stopping"),
            ComponentState::Failed => write!(f, "failed"),
        }
    }
}

/// Lifecycle events broadcast to listeners
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Component state changed
    StateChanged {
        component: String,
        from: ComponentState,
        to: ComponentState,
        reason: Option<String>,
    },
    /// System startup initiated
    StartupInitiated,
    /// System startup completed
    StartupCompleted { duration_ms: u64 },
    /// System shutdown initiated
    ShutdownInitiated { reason: String },
    /// System shutdown completed
    ShutdownCompleted { duration_ms: u64 },
    /// Component health check failed
    HealthCheckFailed { component: String, error: String },
}

/// Component registration info
#[derive(Debug, Clone)]
struct ComponentInfo {
    name: String,
    state: ComponentState,
    priority: u8, // Lower = starts first, stops last
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    restart_count: u32,
}

/// Configuration for lifecycle manager
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// Timeout for component startup (ms)
    pub startup_timeout_ms: u64,
    /// Timeout for component shutdown (ms)
    pub shutdown_timeout_ms: u64,
    /// Maximum restart attempts before marking as failed
    pub max_restart_attempts: u32,
    /// Delay between restart attempts (ms)
    pub restart_delay_ms: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            startup_timeout_ms: 30_000,   // 30 seconds
            shutdown_timeout_ms: 120_000, // 2 minutes
            max_restart_attempts: 3,
            restart_delay_ms: 1_000, // 1 second
        }
    }
}

/// Manages component lifecycle for ordered startup/shutdown
pub struct LifecycleManager {
    components: Arc<RwLock<HashMap<String, ComponentInfo>>>,
    config: LifecycleConfig,
    event_tx: broadcast::Sender<LifecycleEvent>,
    system_state: Arc<RwLock<ComponentState>>,
}

impl LifecycleManager {
    /// Create a new lifecycle manager
    pub fn new(config: LifecycleConfig) -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            components: Arc::new(RwLock::new(HashMap::new())),
            config,
            event_tx,
            system_state: Arc::new(RwLock::new(ComponentState::Stopped)),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(LifecycleConfig::default())
    }

    /// Subscribe to lifecycle events
    pub fn subscribe(&self) -> broadcast::Receiver<LifecycleEvent> {
        self.event_tx.subscribe()
    }

    /// Register a component with priority (lower = starts first)
    pub async fn register(&self, name: &str, priority: u8) {
        let mut components = self.components.write().await;
        components.insert(
            name.to_string(),
            ComponentInfo {
                name: name.to_string(),
                state: ComponentState::Stopped,
                priority,
                started_at: None,
                stopped_at: None,
                last_error: None,
                restart_count: 0,
            },
        );
        debug!("Registered component: {} with priority {}", name, priority);
    }

    /// Get component state
    pub async fn get_state(&self, name: &str) -> Option<ComponentState> {
        let components = self.components.read().await;
        components.get(name).map(|c| c.state)
    }

    /// Get all component states
    pub async fn get_all_states(&self) -> HashMap<String, ComponentState> {
        let components = self.components.read().await;
        components
            .iter()
            .map(|(k, v)| (k.clone(), v.state))
            .collect()
    }

    /// Get system state
    pub async fn system_state(&self) -> ComponentState {
        *self.system_state.read().await
    }

    /// Update component state
    pub async fn set_state(&self, name: &str, state: ComponentState, reason: Option<&str>) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            let from = component.state;
            component.state = state;

            if state == ComponentState::Running {
                component.started_at = Some(Utc::now());
            } else if state.is_terminal() {
                component.stopped_at = Some(Utc::now());
            }

            if let Some(r) = reason {
                if state == ComponentState::Failed {
                    component.last_error = Some(r.to_string());
                }
            }

            // Broadcast state change
            let _ = self.event_tx.send(LifecycleEvent::StateChanged {
                component: name.to_string(),
                from,
                to: state,
                reason: reason.map(String::from),
            });

            info!(
                "Component {} state: {} -> {}{}",
                name,
                from,
                state,
                reason.map(|r| format!(" ({})", r)).unwrap_or_default()
            );
        }
    }

    /// Mark component as failed
    pub async fn mark_failed(&self, name: &str, error: &str) {
        self.set_state(name, ComponentState::Failed, Some(error))
            .await;
    }

    /// Mark component as degraded
    pub async fn mark_degraded(&self, name: &str, reason: &str) {
        self.set_state(name, ComponentState::Degraded, Some(reason))
            .await;
    }

    /// Record component restart
    pub async fn record_restart(&self, name: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            component.restart_count += 1;
            component.state = ComponentState::Starting;
            info!("Component {} restart #{}", name, component.restart_count);
        }
    }

    /// Check if component can be restarted
    pub async fn can_restart(&self, name: &str) -> bool {
        let components = self.components.read().await;
        if let Some(component) = components.get(name) {
            component.restart_count < self.config.max_restart_attempts
        } else {
            false
        }
    }

    /// Get ordered startup list (sorted by priority, lowest first)
    pub async fn get_startup_order(&self) -> Vec<String> {
        let components = self.components.read().await;
        let mut ordered: Vec<_> = components.values().collect();
        ordered.sort_by_key(|c| c.priority);
        ordered.iter().map(|c| c.name.clone()).collect()
    }

    /// Get ordered shutdown list (reverse of startup)
    pub async fn get_shutdown_order(&self) -> Vec<String> {
        let mut order = self.get_startup_order().await;
        order.reverse();
        order
    }

    /// Start all components in order
    pub async fn start_all<F, Fut>(&self, start_fn: F) -> crate::error::Result<()>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = crate::error::Result<()>>,
    {
        let start_time = std::time::Instant::now();
        let _ = self.event_tx.send(LifecycleEvent::StartupInitiated);

        *self.system_state.write().await = ComponentState::Starting;

        let order = self.get_startup_order().await;
        info!("Starting {} components in order: {:?}", order.len(), order);

        for name in order {
            self.set_state(&name, ComponentState::Starting, None).await;

            match tokio::time::timeout(
                std::time::Duration::from_millis(self.config.startup_timeout_ms),
                start_fn(name.clone()),
            )
            .await
            {
                Ok(Ok(())) => {
                    self.set_state(&name, ComponentState::Running, None).await;
                }
                Ok(Err(e)) => {
                    let error = format!("Startup failed: {}", e);
                    self.mark_failed(&name, &error).await;
                    error!("Component {} failed to start: {}", name, e);
                    *self.system_state.write().await = ComponentState::Failed;
                    return Err(e);
                }
                Err(_) => {
                    let error =
                        format!("Startup timeout after {}ms", self.config.startup_timeout_ms);
                    self.mark_failed(&name, &error).await;
                    error!("Component {} startup timeout", name);
                    *self.system_state.write().await = ComponentState::Failed;
                    return Err(crate::error::PloyError::ComponentFailure {
                        component: name,
                        reason: error,
                    });
                }
            }
        }

        *self.system_state.write().await = ComponentState::Running;

        let duration = start_time.elapsed().as_millis() as u64;
        let _ = self.event_tx.send(LifecycleEvent::StartupCompleted {
            duration_ms: duration,
        });
        info!("All components started successfully in {}ms", duration);

        Ok(())
    }

    /// Stop all components in reverse order
    pub async fn stop_all<F, Fut>(&self, stop_fn: F, reason: &str)
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let start_time = std::time::Instant::now();
        let _ = self.event_tx.send(LifecycleEvent::ShutdownInitiated {
            reason: reason.to_string(),
        });

        *self.system_state.write().await = ComponentState::Stopping;

        let order = self.get_shutdown_order().await;
        info!(
            "Stopping {} components in order: {:?} (reason: {})",
            order.len(),
            order,
            reason
        );

        for name in order {
            let current_state = self
                .get_state(&name)
                .await
                .unwrap_or(ComponentState::Stopped);

            if current_state.is_terminal() {
                continue;
            }

            self.set_state(&name, ComponentState::Stopping, Some(reason))
                .await;

            match tokio::time::timeout(
                std::time::Duration::from_millis(self.config.shutdown_timeout_ms),
                stop_fn(name.clone()),
            )
            .await
            {
                Ok(()) => {
                    self.set_state(&name, ComponentState::Stopped, None).await;
                }
                Err(_) => {
                    warn!(
                        "Component {} shutdown timeout after {}ms",
                        name, self.config.shutdown_timeout_ms
                    );
                    self.set_state(&name, ComponentState::Stopped, Some("timeout"))
                        .await;
                }
            }
        }

        *self.system_state.write().await = ComponentState::Stopped;

        let duration = start_time.elapsed().as_millis() as u64;
        let _ = self.event_tx.send(LifecycleEvent::ShutdownCompleted {
            duration_ms: duration,
        });
        info!("All components stopped in {}ms", duration);
    }

    /// Check system health
    pub async fn is_healthy(&self) -> bool {
        let components = self.components.read().await;
        components.values().all(|c| c.state.is_healthy())
    }

    /// Check if any component is degraded
    pub async fn is_degraded(&self) -> bool {
        let components = self.components.read().await;
        components
            .values()
            .any(|c| c.state == ComponentState::Degraded)
    }

    /// Get unhealthy components
    pub async fn get_unhealthy(&self) -> Vec<(String, ComponentState)> {
        let components = self.components.read().await;
        components
            .iter()
            .filter(|(_, c)| !c.state.is_healthy())
            .map(|(k, c)| (k.clone(), c.state))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_state_display() {
        assert_eq!(ComponentState::Running.to_string(), "running");
        assert_eq!(ComponentState::Failed.to_string(), "failed");
    }

    #[test]
    fn test_component_state_checks() {
        assert!(ComponentState::Running.is_healthy());
        assert!(!ComponentState::Degraded.is_healthy());
        assert!(ComponentState::Degraded.can_work());
        assert!(!ComponentState::Stopped.can_work());
        assert!(ComponentState::Failed.is_terminal());
    }

    #[tokio::test]
    async fn test_lifecycle_manager_register() {
        let manager = LifecycleManager::with_defaults();
        manager.register("component1", 1).await;
        manager.register("component2", 2).await;

        let states = manager.get_all_states().await;
        assert_eq!(states.len(), 2);
        assert_eq!(states.get("component1"), Some(&ComponentState::Stopped));
    }

    #[tokio::test]
    async fn test_startup_order() {
        let manager = LifecycleManager::with_defaults();
        manager.register("high_priority", 1).await;
        manager.register("low_priority", 3).await;
        manager.register("medium_priority", 2).await;

        let order = manager.get_startup_order().await;
        assert_eq!(
            order,
            vec!["high_priority", "medium_priority", "low_priority"]
        );
    }
}
