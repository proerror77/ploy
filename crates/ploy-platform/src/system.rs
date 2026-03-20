use chrono::{DateTime, Utc};
use ploy_operator_contracts::{SystemControlResponse, SystemStatus};

#[derive(Debug, Clone)]
pub struct SystemService {
    booted_at: DateTime<Utc>,
    status: String,
}

impl Default for SystemService {
    fn default() -> Self {
        Self {
            booted_at: Utc::now(),
            status: "starting".to_string(),
        }
    }
}

impl SystemService {
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn status(&self) -> SystemStatus {
        SystemStatus {
            status: self.status.clone(),
            uptime_seconds: (Utc::now() - self.booted_at).num_seconds(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            strategy: "platform".to_string(),
            last_trade_time: None,
            websocket_connected: false,
            database_connected: false,
            error_count_1h: 0,
        }
    }

    pub fn control_response(&self, action: &str) -> SystemControlResponse {
        SystemControlResponse {
            success: true,
            message: format!("system {action} accepted"),
        }
    }
}
