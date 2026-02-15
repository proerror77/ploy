//! Alert Manager for Feishu Integration
//!
//! Routes alerts based on severity and integrates with Feishu for notifications.
//! Includes rate limiting to prevent alert storms.

use crate::adapters::{FeishuNotifier, TransactionManager};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Alert severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertLevel {
    /// Informational - log only
    Info,
    /// Warning - Feishu notification
    Warning,
    /// Error - Feishu urgent + pause trading
    Error,
    /// Critical - Feishu urgent + emergency shutdown
    Critical,
}

impl AlertLevel {
    /// Get emoji prefix for alert level
    pub fn emoji(&self) -> &'static str {
        match self {
            AlertLevel::Info => "\u{2139}\u{fe0f}",    // info icon
            AlertLevel::Warning => "\u{26a0}\u{fe0f}", // warning icon
            AlertLevel::Error => "\u{274c}",           // red X
            AlertLevel::Critical => "\u{1f6a8}",       // police light
        }
    }

    /// Get severity string
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertLevel::Info => "info",
            AlertLevel::Warning => "warning",
            AlertLevel::Error => "error",
            AlertLevel::Critical => "critical",
        }
    }
}

impl std::fmt::Display for AlertLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Alert payload
#[derive(Debug, Clone)]
pub struct Alert {
    pub level: AlertLevel,
    pub component: String,
    pub title: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

impl Alert {
    /// Create a new alert
    pub fn new(level: AlertLevel, component: &str, title: &str, message: &str) -> Self {
        Self {
            level,
            component: component.to_string(),
            title: title.to_string(),
            message: message.to_string(),
            metadata: None,
            timestamp: Utc::now(),
        }
    }

    /// Add metadata to the alert
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Format for Feishu message
    pub fn format_feishu(&self) -> String {
        format!(
            "{} **{}**\n\n**Component:** {}\n**Time:** {}\n\n{}",
            self.level.emoji(),
            self.title,
            self.component,
            self.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
            self.message
        )
    }
}

/// Configuration for alert manager
#[derive(Debug, Clone)]
pub struct AlertManagerConfig {
    /// Minimum interval between duplicate alerts (default: 60s)
    pub rate_limit_secs: u64,
    /// Whether to send alerts for info level (default: false)
    pub notify_info: bool,
    /// Maximum alerts per minute before throttling (default: 10)
    pub max_alerts_per_minute: u32,
}

impl Default for AlertManagerConfig {
    fn default() -> Self {
        Self {
            rate_limit_secs: 60,
            notify_info: false,
            max_alerts_per_minute: 10,
        }
    }
}

/// Rate limiter state for an alert key
#[derive(Debug)]
struct RateLimitState {
    last_sent: DateTime<Utc>,
    suppressed_count: u32,
}

/// Alert Manager for coordinating notifications
pub struct AlertManager {
    config: AlertManagerConfig,
    feishu: Option<Arc<FeishuNotifier>>,
    transaction_manager: Option<Arc<TransactionManager>>,
    rate_limits: Arc<RwLock<HashMap<String, RateLimitState>>>,
    alerts_this_minute: Arc<RwLock<Vec<DateTime<Utc>>>>,
    event_tx: tokio::sync::broadcast::Sender<Alert>,
}

impl AlertManager {
    /// Create a new alert manager
    pub fn new(config: AlertManagerConfig) -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            config,
            feishu: None,
            transaction_manager: None,
            rate_limits: Arc::new(RwLock::new(HashMap::new())),
            alerts_this_minute: Arc::new(RwLock::new(Vec::new())),
            event_tx,
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(AlertManagerConfig::default())
    }

    /// Set Feishu notifier for alerts
    pub fn with_feishu(mut self, feishu: Arc<FeishuNotifier>) -> Self {
        self.feishu = Some(feishu);
        self
    }

    /// Set transaction manager for persistence
    pub fn with_transaction_manager(mut self, tm: Arc<TransactionManager>) -> Self {
        self.transaction_manager = Some(tm);
        self
    }

    /// Subscribe to alerts
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Alert> {
        self.event_tx.subscribe()
    }

    /// Generate rate limit key for an alert
    fn rate_limit_key(alert: &Alert) -> String {
        format!("{}:{}:{}", alert.component, alert.level, alert.title)
    }

    /// Check if alert should be rate limited
    async fn should_rate_limit(&self, alert: &Alert) -> bool {
        let key = Self::rate_limit_key(alert);
        let now = Utc::now();

        let mut limits = self.rate_limits.write().await;

        if let Some(state) = limits.get_mut(&key) {
            let elapsed = now.signed_duration_since(state.last_sent).num_seconds() as u64;
            if elapsed < self.config.rate_limit_secs {
                state.suppressed_count += 1;
                debug!(
                    "Rate limiting alert '{}' ({} suppressed)",
                    alert.title, state.suppressed_count
                );
                return true;
            }

            // Reset state
            state.last_sent = now;
            state.suppressed_count = 0;
        } else {
            limits.insert(
                key,
                RateLimitState {
                    last_sent: now,
                    suppressed_count: 0,
                },
            );
        }

        false
    }

    /// Check global rate limit (alerts per minute)
    async fn is_throttled(&self) -> bool {
        let now = Utc::now();
        let minute_ago = now - chrono::Duration::minutes(1);

        let mut alerts = self.alerts_this_minute.write().await;

        // Clean old entries
        alerts.retain(|t| *t > minute_ago);

        if alerts.len() >= self.config.max_alerts_per_minute as usize {
            warn!(
                "Alert throttling: {} alerts in last minute (max: {})",
                alerts.len(),
                self.config.max_alerts_per_minute
            );
            return true;
        }

        alerts.push(now);
        false
    }

    /// Send an alert
    pub async fn alert(&self, alert: Alert) {
        // Always broadcast locally
        let _ = self.event_tx.send(alert.clone());

        // Log based on level
        match alert.level {
            AlertLevel::Info => info!("[{}] {}: {}", alert.component, alert.title, alert.message),
            AlertLevel::Warning => {
                warn!("[{}] {}: {}", alert.component, alert.title, alert.message)
            }
            AlertLevel::Error => {
                error!("[{}] {}: {}", alert.component, alert.title, alert.message)
            }
            AlertLevel::Critical => {
                error!(
                    "CRITICAL [{}] {}: {}",
                    alert.component, alert.title, alert.message
                )
            }
        }

        // Persist to database
        if let Some(ref tm) = self.transaction_manager {
            if let Err(e) = tm
                .record_event(
                    "alert",
                    &alert.component,
                    alert.level.as_str(),
                    &format!("{}: {}", alert.title, alert.message),
                    alert.metadata.clone(),
                )
                .await
            {
                error!("Failed to persist alert: {}", e);
            }
        }

        // Check if we should notify via Feishu
        let should_notify = match alert.level {
            AlertLevel::Info => self.config.notify_info,
            AlertLevel::Warning | AlertLevel::Error | AlertLevel::Critical => true,
        };

        if !should_notify {
            return;
        }

        // Check rate limiting
        if self.should_rate_limit(&alert).await {
            return;
        }

        // Check global throttling
        if self.is_throttled().await {
            return;
        }

        // Send to Feishu
        if let Some(ref feishu) = self.feishu {
            let message = alert.format_feishu();

            // Send to Feishu (all alerts go through same channel)
            if let Err(e) = feishu.send_message(&message).await {
                error!("Failed to send Feishu alert: {}", e);
            }
        }
    }

    /// Send an info alert
    pub async fn info(&self, component: &str, title: &str, message: &str) {
        self.alert(Alert::new(AlertLevel::Info, component, title, message))
            .await;
    }

    /// Send a warning alert
    pub async fn warning(&self, component: &str, title: &str, message: &str) {
        self.alert(Alert::new(AlertLevel::Warning, component, title, message))
            .await;
    }

    /// Send an error alert
    pub async fn error(&self, component: &str, title: &str, message: &str) {
        self.alert(Alert::new(AlertLevel::Error, component, title, message))
            .await;
    }

    /// Send a critical alert
    pub async fn critical(&self, component: &str, title: &str, message: &str) {
        self.alert(Alert::new(AlertLevel::Critical, component, title, message))
            .await;
    }

    /// Send alert about circuit breaker trip
    pub async fn circuit_breaker_tripped(&self, reason: &str) {
        self.alert(
            Alert::new(
                AlertLevel::Error,
                "circuit_breaker",
                "Circuit Breaker Tripped",
                reason,
            )
            .with_metadata(serde_json::json!({
                "action": "trading_paused",
                "reason": reason
            })),
        )
        .await;
    }

    /// Send alert about component failure
    pub async fn component_failed(&self, component: &str, error: &str) {
        self.alert(
            Alert::new(AlertLevel::Error, component, "Component Failed", error).with_metadata(
                serde_json::json!({
                    "action": "restart_attempted",
                    "error": error
                }),
            ),
        )
        .await;
    }

    /// Send alert about component restart exhausted
    pub async fn restart_exhausted(&self, component: &str, attempts: u32) {
        self.alert(
            Alert::new(
                AlertLevel::Critical,
                component,
                "Restart Attempts Exhausted",
                &format!(
                    "Component {} failed to restart after {} attempts",
                    component, attempts
                ),
            )
            .with_metadata(serde_json::json!({
                "action": "manual_intervention_required",
                "attempts": attempts
            })),
        )
        .await;
    }

    /// Send alert about daily loss limit
    pub async fn daily_loss_limit_hit(&self, loss: &str) {
        self.alert(
            Alert::new(
                AlertLevel::Critical,
                "risk_manager",
                "Daily Loss Limit Hit",
                &format!("Trading halted due to daily loss of {}", loss),
            )
            .with_metadata(serde_json::json!({
                "action": "trading_halted",
                "loss": loss
            })),
        )
        .await;
    }

    /// Get suppressed alert counts
    pub async fn get_suppressed_counts(&self) -> HashMap<String, u32> {
        let limits = self.rate_limits.read().await;
        limits
            .iter()
            .filter(|(_, state)| state.suppressed_count > 0)
            .map(|(key, state)| (key.clone(), state.suppressed_count))
            .collect()
    }

    /// Reset rate limits (call daily)
    pub async fn reset_rate_limits(&self) {
        let mut limits = self.rate_limits.write().await;
        limits.clear();
        debug!("Alert rate limits reset");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_level_ordering() {
        assert!(AlertLevel::Info < AlertLevel::Warning);
        assert!(AlertLevel::Warning < AlertLevel::Error);
        assert!(AlertLevel::Error < AlertLevel::Critical);
    }

    #[test]
    fn test_alert_format_feishu() {
        let alert = Alert::new(
            AlertLevel::Warning,
            "test_component",
            "Test Alert",
            "This is a test message",
        );

        let formatted = alert.format_feishu();
        assert!(formatted.contains("Test Alert"));
        assert!(formatted.contains("test_component"));
        assert!(formatted.contains("This is a test message"));
    }

    #[tokio::test]
    async fn test_rate_limiting() {
        let config = AlertManagerConfig {
            rate_limit_secs: 60,
            ..Default::default()
        };
        let manager = AlertManager::new(config);

        let alert = Alert::new(AlertLevel::Warning, "test", "Test", "Message");

        // First alert should not be rate limited
        assert!(!manager.should_rate_limit(&alert).await);

        // Second identical alert should be rate limited
        assert!(manager.should_rate_limit(&alert).await);
    }
}
