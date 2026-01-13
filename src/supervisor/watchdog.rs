//! Watchdog Daemon for Component Health Monitoring
//!
//! Monitors component heartbeats and triggers recovery actions when components
//! become unresponsive or fail.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Configuration for watchdog
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Interval between health checks (default: 5s)
    pub check_interval_secs: u64,
    /// Heartbeat timeout before marking component stale (default: 30s)
    pub heartbeat_timeout_secs: u64,
    /// Maximum restart attempts before giving up (default: 3)
    pub max_restart_attempts: u32,
    /// Delay between restart attempts (default: 1s)
    pub restart_delay_secs: u64,
    /// Time window to count restart attempts (default: 300s / 5 min)
    pub restart_window_secs: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 5,
            heartbeat_timeout_secs: 30,
            max_restart_attempts: 3,
            restart_delay_secs: 1,
            restart_window_secs: 300,
        }
    }
}

/// Component health status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Component is healthy
    Healthy,
    /// Component hasn't sent heartbeat recently
    Stale,
    /// Component has failed
    Failed,
    /// Component is being restarted
    Restarting,
    /// Component stopped intentionally
    Stopped,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Stale => write!(f, "stale"),
            HealthStatus::Failed => write!(f, "failed"),
            HealthStatus::Restarting => write!(f, "restarting"),
            HealthStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// Health information for a component
#[derive(Debug, Clone)]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub restart_count: u32,
    pub last_restart: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Event emitted by watchdog
#[derive(Debug, Clone)]
pub enum WatchdogEvent {
    /// Component became stale (no heartbeat)
    ComponentStale {
        component: String,
        last_heartbeat: DateTime<Utc>,
    },
    /// Component failed
    ComponentFailed {
        component: String,
        error: String,
    },
    /// Attempting to restart component
    RestartAttempt {
        component: String,
        attempt: u32,
    },
    /// Restart succeeded
    RestartSucceeded {
        component: String,
    },
    /// Restart failed
    RestartFailed {
        component: String,
        error: String,
    },
    /// Component exhausted restart attempts
    RestartExhausted {
        component: String,
        attempts: u32,
    },
}

/// Tracked component state
#[derive(Debug)]
struct TrackedComponent {
    health: ComponentHealth,
    restart_timestamps: Vec<DateTime<Utc>>,
}

/// Watchdog daemon for monitoring component health
pub struct Watchdog {
    config: WatchdogConfig,
    components: Arc<RwLock<HashMap<String, TrackedComponent>>>,
    event_tx: tokio::sync::broadcast::Sender<WatchdogEvent>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl Watchdog {
    /// Create a new watchdog
    pub fn new(config: WatchdogConfig) -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            config,
            components: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(WatchdogConfig::default())
    }

    /// Subscribe to watchdog events
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WatchdogEvent> {
        self.event_tx.subscribe()
    }

    /// Register a component for monitoring
    pub async fn register(&self, name: &str) {
        let mut components = self.components.write().await;
        components.insert(
            name.to_string(),
            TrackedComponent {
                health: ComponentHealth {
                    name: name.to_string(),
                    status: HealthStatus::Stopped,
                    last_heartbeat: None,
                    restart_count: 0,
                    last_restart: None,
                    last_error: None,
                },
                restart_timestamps: Vec::new(),
            },
        );
        debug!("Registered component for monitoring: {}", name);
    }

    /// Record heartbeat from a component
    pub async fn heartbeat(&self, name: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            component.health.last_heartbeat = Some(Utc::now());
            if component.health.status == HealthStatus::Stale {
                component.health.status = HealthStatus::Healthy;
                info!("Component {} recovered from stale state", name);
            } else if component.health.status == HealthStatus::Stopped
                || component.health.status == HealthStatus::Restarting
            {
                component.health.status = HealthStatus::Healthy;
            }
        }
    }

    /// Mark component as started
    pub async fn mark_started(&self, name: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            component.health.status = HealthStatus::Healthy;
            component.health.last_heartbeat = Some(Utc::now());
        }
    }

    /// Mark component as stopped
    pub async fn mark_stopped(&self, name: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            component.health.status = HealthStatus::Stopped;
        }
    }

    /// Mark component as failed with error
    pub async fn mark_failed(&self, name: &str, error: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            component.health.status = HealthStatus::Failed;
            component.health.last_error = Some(error.to_string());

            let _ = self.event_tx.send(WatchdogEvent::ComponentFailed {
                component: name.to_string(),
                error: error.to_string(),
            });

            error!("Component {} marked as failed: {}", name, error);
        }
    }

    /// Get health of a specific component
    pub async fn get_health(&self, name: &str) -> Option<ComponentHealth> {
        let components = self.components.read().await;
        components.get(name).map(|c| c.health.clone())
    }

    /// Get health of all components
    pub async fn get_all_health(&self) -> Vec<ComponentHealth> {
        let components = self.components.read().await;
        components.values().map(|c| c.health.clone()).collect()
    }

    /// Check if all components are healthy
    pub async fn all_healthy(&self) -> bool {
        let components = self.components.read().await;
        components
            .values()
            .all(|c| matches!(c.health.status, HealthStatus::Healthy | HealthStatus::Stopped))
    }

    /// Get list of unhealthy components
    pub async fn get_unhealthy(&self) -> Vec<ComponentHealth> {
        let components = self.components.read().await;
        components
            .values()
            .filter(|c| {
                matches!(
                    c.health.status,
                    HealthStatus::Stale | HealthStatus::Failed | HealthStatus::Restarting
                )
            })
            .map(|c| c.health.clone())
            .collect()
    }

    /// Check if component can be restarted (within limit)
    pub async fn can_restart(&self, name: &str) -> bool {
        let components = self.components.read().await;
        if let Some(component) = components.get(name) {
            let window_start =
                Utc::now() - chrono::Duration::seconds(self.config.restart_window_secs as i64);
            let recent_restarts = component
                .restart_timestamps
                .iter()
                .filter(|t| **t > window_start)
                .count();
            recent_restarts < self.config.max_restart_attempts as usize
        } else {
            false
        }
    }

    /// Record a restart attempt
    pub async fn record_restart(&self, name: &str) {
        let mut components = self.components.write().await;
        if let Some(component) = components.get_mut(name) {
            let now = Utc::now();
            component.health.status = HealthStatus::Restarting;
            component.health.restart_count += 1;
            component.health.last_restart = Some(now);
            component.restart_timestamps.push(now);

            // Clean old timestamps
            let window_start =
                now - chrono::Duration::seconds(self.config.restart_window_secs as i64);
            component
                .restart_timestamps
                .retain(|t| *t > window_start);

            let attempt = component.restart_timestamps.len() as u32;
            let _ = self.event_tx.send(WatchdogEvent::RestartAttempt {
                component: name.to_string(),
                attempt,
            });

            info!("Restart attempt #{} for component {}", attempt, name);
        }
    }

    /// Run health check cycle
    async fn check_health(&self) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(self.config.heartbeat_timeout_secs as i64);

        let mut components = self.components.write().await;

        for (name, component) in components.iter_mut() {
            // Skip stopped components
            if component.health.status == HealthStatus::Stopped {
                continue;
            }

            // Check heartbeat timeout
            if let Some(last_hb) = component.health.last_heartbeat {
                if now.signed_duration_since(last_hb) > timeout {
                    if component.health.status != HealthStatus::Stale {
                        component.health.status = HealthStatus::Stale;

                        let _ = self.event_tx.send(WatchdogEvent::ComponentStale {
                            component: name.clone(),
                            last_heartbeat: last_hb,
                        });

                        warn!(
                            "Component {} is stale (last heartbeat: {:?})",
                            name, last_hb
                        );
                    }
                }
            }
        }
    }

    /// Start the watchdog daemon
    pub async fn start<F, Fut>(&self, restart_fn: F)
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        info!("Watchdog daemon started");

        let check_interval = Duration::from_secs(self.config.check_interval_secs);
        let restart_delay = Duration::from_secs(self.config.restart_delay_secs);

        let components = self.components.clone();
        let event_tx = self.event_tx.clone();
        let running = self.running.clone();
        let max_attempts = self.config.max_restart_attempts;
        let window_secs = self.config.restart_window_secs;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(check_interval);

            while running.load(std::sync::atomic::Ordering::SeqCst) {
                interval.tick().await;

                let now = Utc::now();
                let _timeout = chrono::Duration::seconds(30); // Use default timeout
                let window_start = now - chrono::Duration::seconds(window_secs as i64);

                // Collect components needing restart
                let to_restart: Vec<String> = {
                    let components = components.read().await;
                    components
                        .iter()
                        .filter(|(_, c)| {
                            matches!(c.health.status, HealthStatus::Stale | HealthStatus::Failed)
                        })
                        .filter(|(_, c)| {
                            c.restart_timestamps.iter().filter(|t| **t > window_start).count()
                                < max_attempts as usize
                        })
                        .map(|(name, _)| name.clone())
                        .collect()
                };

                // Attempt restarts
                for name in to_restart {
                    {
                        let mut components = components.write().await;
                        if let Some(component) = components.get_mut(&name) {
                            component.health.status = HealthStatus::Restarting;
                            component.restart_timestamps.push(now);

                            let attempt = component
                                .restart_timestamps
                                .iter()
                                .filter(|t| **t > window_start)
                                .count() as u32;

                            let _ = event_tx.send(WatchdogEvent::RestartAttempt {
                                component: name.clone(),
                                attempt,
                            });
                        }
                    }

                    tokio::time::sleep(restart_delay).await;

                    match restart_fn(name.clone()).await {
                        Ok(()) => {
                            let mut components = components.write().await;
                            if let Some(component) = components.get_mut(&name) {
                                component.health.status = HealthStatus::Healthy;
                                component.health.last_heartbeat = Some(Utc::now());
                            }

                            let _ = event_tx.send(WatchdogEvent::RestartSucceeded {
                                component: name.clone(),
                            });

                            info!("Component {} restarted successfully", name);
                        }
                        Err(e) => {
                            let mut components = components.write().await;
                            if let Some(component) = components.get_mut(&name) {
                                component.health.status = HealthStatus::Failed;
                                component.health.last_error = Some(e.clone());

                                let recent_restarts = component
                                    .restart_timestamps
                                    .iter()
                                    .filter(|t| **t > window_start)
                                    .count();

                                if recent_restarts >= max_attempts as usize {
                                    let _ = event_tx.send(WatchdogEvent::RestartExhausted {
                                        component: name.clone(),
                                        attempts: max_attempts,
                                    });

                                    error!(
                                        "Component {} exhausted restart attempts ({})",
                                        name, max_attempts
                                    );
                                } else {
                                    let _ = event_tx.send(WatchdogEvent::RestartFailed {
                                        component: name.clone(),
                                        error: e.clone(),
                                    });

                                    warn!("Component {} restart failed: {}", name, e);
                                }
                            }
                        }
                    }
                }
            }

            info!("Watchdog daemon stopped");
        });
    }

    /// Stop the watchdog daemon
    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Trait for components that can be monitored
pub trait Monitored {
    /// Get component name
    fn component_name(&self) -> &str;

    /// Check if component is healthy
    fn is_healthy(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_watchdog_register() {
        let watchdog = Watchdog::with_defaults();
        watchdog.register("test_component").await;

        let health = watchdog.get_health("test_component").await;
        assert!(health.is_some());
        assert_eq!(health.unwrap().status, HealthStatus::Stopped);
    }

    #[tokio::test]
    async fn test_watchdog_heartbeat() {
        let watchdog = Watchdog::with_defaults();
        watchdog.register("test_component").await;
        watchdog.mark_started("test_component").await;
        watchdog.heartbeat("test_component").await;

        let health = watchdog.get_health("test_component").await.unwrap();
        assert_eq!(health.status, HealthStatus::Healthy);
        assert!(health.last_heartbeat.is_some());
    }

    #[tokio::test]
    async fn test_watchdog_can_restart() {
        let watchdog = Watchdog::with_defaults();
        watchdog.register("test_component").await;

        assert!(watchdog.can_restart("test_component").await);
    }
}
