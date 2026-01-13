//! Recovery Playbook
//!
//! Defines recovery actions and sequences for handling various failure scenarios.

use tracing::{debug, info, warn};

/// Recovery actions that can be taken
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// No action needed
    None,
    /// Log the issue only
    LogOnly,
    /// Send alert notification
    Alert { level: AlertSeverity, message: String },
    /// Restart a specific component
    RestartComponent { name: String },
    /// Trip the circuit breaker
    TripCircuitBreaker { reason: String },
    /// Close the circuit breaker (resume trading)
    CloseCircuitBreaker,
    /// Pause trading operations
    PauseTrading { duration_secs: u64 },
    /// Resume trading operations
    ResumeTrading,
    /// Reconnect WebSocket
    ReconnectWebSocket { name: String },
    /// Refresh quotes
    RefreshQuotes,
    /// Create checkpoint
    CreateCheckpoint,
    /// Initiate graceful shutdown
    GracefulShutdown { reason: String },
    /// Emergency shutdown (immediate)
    EmergencyShutdown { reason: String },
    /// Escalate to human operator
    EscalateToOperator { reason: String },
}

/// Alert severity for recovery actions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Failure scenario classification
#[derive(Debug, Clone)]
pub enum FailureScenario {
    /// Component stopped sending heartbeats
    ComponentStale { component: String, duration_secs: u64 },
    /// Component crashed or failed
    ComponentCrash { component: String, error: String },
    /// WebSocket disconnected
    WebSocketDisconnect { name: String, duration_secs: u64 },
    /// Quotes are stale
    StaleQuotes { age_secs: u64 },
    /// Too many consecutive trade failures
    ConsecutiveTradeFailures { count: u32 },
    /// Daily loss limit approached or exceeded
    DailyLossLimit { current_loss: String, limit: String },
    /// Circuit breaker tripped
    CircuitBreakerTripped { reason: String },
    /// Database connection lost
    DatabaseDisconnect { duration_secs: u64 },
    /// External API unreachable
    ExternalApiFailure { service: String, error: String },
    /// Memory usage high
    HighMemoryUsage { percent: f64 },
    /// Disk space low
    LowDiskSpace { percent: f64 },
}

/// Recovery playbook with predefined responses
pub struct RecoveryPlaybook {
    /// Maximum restart attempts per component
    pub max_restart_attempts: u32,
    /// Timeout before escalating stale component (seconds)
    pub stale_escalation_secs: u64,
    /// WebSocket reconnect timeout before alerting (seconds)
    pub ws_reconnect_timeout_secs: u64,
}

impl Default for RecoveryPlaybook {
    fn default() -> Self {
        Self {
            max_restart_attempts: 3,
            stale_escalation_secs: 120,
            ws_reconnect_timeout_secs: 60,
        }
    }
}

impl RecoveryPlaybook {
    /// Create a new playbook with default settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Determine recovery actions for a failure scenario
    pub fn get_actions(&self, scenario: &FailureScenario) -> Vec<RecoveryAction> {
        match scenario {
            FailureScenario::ComponentStale {
                component,
                duration_secs,
            } => self.handle_component_stale(component, *duration_secs),

            FailureScenario::ComponentCrash { component, error } => {
                self.handle_component_crash(component, error)
            }

            FailureScenario::WebSocketDisconnect { name, duration_secs } => {
                self.handle_websocket_disconnect(name, *duration_secs)
            }

            FailureScenario::StaleQuotes { age_secs } => self.handle_stale_quotes(*age_secs),

            FailureScenario::ConsecutiveTradeFailures { count } => {
                self.handle_trade_failures(*count)
            }

            FailureScenario::DailyLossLimit { current_loss, limit } => {
                self.handle_daily_loss_limit(current_loss, limit)
            }

            FailureScenario::CircuitBreakerTripped { reason } => {
                self.handle_circuit_breaker_tripped(reason)
            }

            FailureScenario::DatabaseDisconnect { duration_secs } => {
                self.handle_database_disconnect(*duration_secs)
            }

            FailureScenario::ExternalApiFailure { service, error } => {
                self.handle_external_api_failure(service, error)
            }

            FailureScenario::HighMemoryUsage { percent } => self.handle_high_memory(*percent),

            FailureScenario::LowDiskSpace { percent } => self.handle_low_disk_space(*percent),
        }
    }

    fn handle_component_stale(&self, component: &str, duration_secs: u64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if duration_secs > self.stale_escalation_secs {
            // Component has been stale too long - escalate
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Error,
                message: format!(
                    "Component {} unresponsive for {}s",
                    component, duration_secs
                ),
            });
            actions.push(RecoveryAction::RestartComponent {
                name: component.to_string(),
            });
        } else if duration_secs > 30 {
            // Initial stale detection
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("Component {} may be stale ({}s)", component, duration_secs),
            });
        }

        actions
    }

    fn handle_component_crash(&self, component: &str, error: &str) -> Vec<RecoveryAction> {
        vec![
            RecoveryAction::Alert {
                level: AlertSeverity::Error,
                message: format!("Component {} crashed: {}", component, error),
            },
            RecoveryAction::CreateCheckpoint,
            RecoveryAction::RestartComponent {
                name: component.to_string(),
            },
        ]
    }

    fn handle_websocket_disconnect(&self, name: &str, duration_secs: u64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if duration_secs > self.ws_reconnect_timeout_secs {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("WebSocket {} disconnected for {}s", name, duration_secs),
            });
            actions.push(RecoveryAction::ReconnectWebSocket {
                name: name.to_string(),
            });

            // If WebSocket is critical for quotes, consider tripping circuit
            if duration_secs > 120 {
                actions.push(RecoveryAction::TripCircuitBreaker {
                    reason: format!("WebSocket {} disconnected {}s", name, duration_secs),
                });
            }
        } else {
            actions.push(RecoveryAction::ReconnectWebSocket {
                name: name.to_string(),
            });
        }

        actions
    }

    fn handle_stale_quotes(&self, age_secs: u64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if age_secs > 30 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("Quotes are stale ({}s old)", age_secs),
            });
            actions.push(RecoveryAction::RefreshQuotes);
        }

        if age_secs > 60 {
            actions.push(RecoveryAction::TripCircuitBreaker {
                reason: format!("Quotes stale for {}s", age_secs),
            });
        }

        actions
    }

    fn handle_trade_failures(&self, count: u32) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if count >= 5 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Error,
                message: format!("{} consecutive trade failures", count),
            });
            actions.push(RecoveryAction::TripCircuitBreaker {
                reason: format!("{} consecutive failures", count),
            });
        } else if count >= 3 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("{} consecutive trade failures", count),
            });
        }

        actions
    }

    fn handle_daily_loss_limit(&self, current_loss: &str, limit: &str) -> Vec<RecoveryAction> {
        vec![
            RecoveryAction::Alert {
                level: AlertSeverity::Critical,
                message: format!("Daily loss {} exceeds limit {}", current_loss, limit),
            },
            RecoveryAction::TripCircuitBreaker {
                reason: format!("Daily loss limit: {}", current_loss),
            },
            RecoveryAction::CreateCheckpoint,
            RecoveryAction::EscalateToOperator {
                reason: format!("Daily loss limit hit: {}", current_loss),
            },
        ]
    }

    fn handle_circuit_breaker_tripped(&self, reason: &str) -> Vec<RecoveryAction> {
        vec![
            RecoveryAction::Alert {
                level: AlertSeverity::Error,
                message: format!("Circuit breaker tripped: {}", reason),
            },
            RecoveryAction::PauseTrading { duration_secs: 300 },
            RecoveryAction::CreateCheckpoint,
        ]
    }

    fn handle_database_disconnect(&self, duration_secs: u64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if duration_secs > 30 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Critical,
                message: format!("Database disconnected for {}s", duration_secs),
            });
            actions.push(RecoveryAction::TripCircuitBreaker {
                reason: "Database unavailable".to_string(),
            });
        }

        if duration_secs > 120 {
            actions.push(RecoveryAction::EscalateToOperator {
                reason: "Prolonged database disconnection".to_string(),
            });
        }

        actions
    }

    fn handle_external_api_failure(&self, service: &str, error: &str) -> Vec<RecoveryAction> {
        vec![
            RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("External API {} failed: {}", service, error),
            },
            RecoveryAction::LogOnly,
        ]
    }

    fn handle_high_memory(&self, percent: f64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if percent > 90.0 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Critical,
                message: format!("Memory usage critical: {:.1}%", percent),
            });
            actions.push(RecoveryAction::GracefulShutdown {
                reason: "High memory usage".to_string(),
            });
        } else if percent > 80.0 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("Memory usage high: {:.1}%", percent),
            });
        }

        actions
    }

    fn handle_low_disk_space(&self, percent: f64) -> Vec<RecoveryAction> {
        let mut actions = Vec::new();

        if percent < 5.0 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Critical,
                message: format!("Disk space critical: {:.1}% free", percent),
            });
            actions.push(RecoveryAction::EscalateToOperator {
                reason: "Critical disk space".to_string(),
            });
        } else if percent < 10.0 {
            actions.push(RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                message: format!("Disk space low: {:.1}% free", percent),
            });
        }

        actions
    }
}

/// Execute a recovery action
pub async fn execute_action(action: &RecoveryAction) {
    match action {
        RecoveryAction::None => {}
        RecoveryAction::LogOnly => debug!("Recovery action: log only"),
        RecoveryAction::Alert { level, message } => {
            info!("Recovery alert [{:?}]: {}", level, message);
            // Actual alert sending is handled by AlertManager
        }
        RecoveryAction::RestartComponent { name } => {
            info!("Recovery action: restart component {}", name);
            // Actual restart is handled by Watchdog
        }
        RecoveryAction::TripCircuitBreaker { reason } => {
            warn!("Recovery action: trip circuit breaker - {}", reason);
            // Actual trip is handled by CircuitBreaker
        }
        RecoveryAction::CloseCircuitBreaker => {
            info!("Recovery action: close circuit breaker");
        }
        RecoveryAction::PauseTrading { duration_secs } => {
            warn!("Recovery action: pause trading for {}s", duration_secs);
        }
        RecoveryAction::ResumeTrading => {
            info!("Recovery action: resume trading");
        }
        RecoveryAction::ReconnectWebSocket { name } => {
            info!("Recovery action: reconnect WebSocket {}", name);
        }
        RecoveryAction::RefreshQuotes => {
            info!("Recovery action: refresh quotes");
        }
        RecoveryAction::CreateCheckpoint => {
            info!("Recovery action: create checkpoint");
        }
        RecoveryAction::GracefulShutdown { reason } => {
            warn!("Recovery action: graceful shutdown - {}", reason);
        }
        RecoveryAction::EmergencyShutdown { reason } => {
            warn!("Recovery action: EMERGENCY shutdown - {}", reason);
        }
        RecoveryAction::EscalateToOperator { reason } => {
            warn!("Recovery action: escalate to operator - {}", reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_stale_actions() {
        let playbook = RecoveryPlaybook::default();

        // Short stale - no action
        let actions = playbook.get_actions(&FailureScenario::ComponentStale {
            component: "test".to_string(),
            duration_secs: 10,
        });
        assert!(actions.is_empty());

        // Medium stale - warning
        let actions = playbook.get_actions(&FailureScenario::ComponentStale {
            component: "test".to_string(),
            duration_secs: 60,
        });
        assert!(!actions.is_empty());
        assert!(matches!(
            actions[0],
            RecoveryAction::Alert {
                level: AlertSeverity::Warning,
                ..
            }
        ));

        // Long stale - error + restart
        let actions = playbook.get_actions(&FailureScenario::ComponentStale {
            component: "test".to_string(),
            duration_secs: 150,
        });
        assert!(actions.len() >= 2);
    }

    #[test]
    fn test_trade_failures_actions() {
        let playbook = RecoveryPlaybook::default();

        // 5 failures - trip circuit
        let actions = playbook.get_actions(&FailureScenario::ConsecutiveTradeFailures { count: 5 });
        assert!(actions
            .iter()
            .any(|a| matches!(a, RecoveryAction::TripCircuitBreaker { .. })));
    }

    #[test]
    fn test_daily_loss_limit_actions() {
        let playbook = RecoveryPlaybook::default();

        let actions = playbook.get_actions(&FailureScenario::DailyLossLimit {
            current_loss: "$150".to_string(),
            limit: "$100".to_string(),
        });

        // Should include critical alert, circuit breaker trip, checkpoint, and escalation
        assert!(actions.len() >= 4);
        assert!(actions
            .iter()
            .any(|a| matches!(a, RecoveryAction::EscalateToOperator { .. })));
    }
}
