pub mod risk_view;

pub use risk_view::RiskView;

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

/// Minimal health state for WebSocket liveness tracking.
pub struct HealthState {
    ws_connected: AtomicBool,
    message_count: Mutex<u64>,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            ws_connected: AtomicBool::new(false),
            message_count: Mutex::new(0),
        }
    }

    pub fn set_ws_connected(&self, connected: bool) {
        self.ws_connected.store(connected, Ordering::Relaxed);
    }

    pub fn is_ws_connected(&self) -> bool {
        self.ws_connected.load(Ordering::Relaxed)
    }

    pub async fn record_ws_message(&self) {
        let mut count = self.message_count.lock().await;
        *count += 1;
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}
