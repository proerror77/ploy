use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::{SystemControlResponse, SystemStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePhase {
    Starting,
    Running,
    Degraded,
    Recovering,
}

impl RuntimePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Degraded => "degraded",
            Self::Recovering => "recovering",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemService {
    booted_at: DateTime<Utc>,
    phase: RuntimePhase,
    listen_addr: Option<String>,
    last_trade_time: Option<DateTime<Utc>>,
    websocket_connected: bool,
    database_connected: bool,
    error_timestamps: Vec<DateTime<Utc>>,
    live_reconcile_failures: u32,
    next_live_reconcile_at: Option<DateTime<Utc>>,
    last_live_reconcile_error: Option<String>,
}

impl Default for SystemService {
    fn default() -> Self {
        Self {
            booted_at: Utc::now(),
            phase: RuntimePhase::Starting,
            listen_addr: None,
            last_trade_time: None,
            websocket_connected: false,
            database_connected: false,
            error_timestamps: Vec::new(),
            live_reconcile_failures: 0,
            next_live_reconcile_at: None,
            last_live_reconcile_error: None,
        }
    }
}

impl SystemService {
    pub fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        let (phase, listen_addr) = status
            .split_once('@')
            .map_or((status.as_str(), None), |(phase, listen_addr)| {
                (phase, Some(listen_addr.to_string()))
            });
        self.phase = match phase {
            "running" => RuntimePhase::Running,
            "degraded" => RuntimePhase::Degraded,
            "recovering" => RuntimePhase::Recovering,
            _ => RuntimePhase::Starting,
        };
        self.listen_addr = listen_addr;
    }

    pub fn mark_running(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        self.phase = RuntimePhase::Running;
    }

    pub fn mark_recovering(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        self.phase = RuntimePhase::Recovering;
    }

    pub fn mark_degraded(&mut self, listen_addr: &str) {
        self.listen_addr = Some(listen_addr.to_string());
        self.error_timestamps.push(Utc::now());
        self.phase = RuntimePhase::Degraded;
    }

    pub fn is_degraded(&self) -> bool {
        self.phase == RuntimePhase::Degraded
    }

    pub fn set_websocket_connected(&mut self, connected: bool) {
        self.websocket_connected = connected;
    }

    pub fn set_database_connected(&mut self, connected: bool) {
        self.database_connected = connected;
    }

    pub fn note_trade(&mut self, last_trade_time: Option<DateTime<Utc>>) {
        if let Some(last_trade_time) = last_trade_time {
            self.last_trade_time = Some(
                self.last_trade_time
                    .map(|current| current.max(last_trade_time))
                    .unwrap_or(last_trade_time),
            );
        }
    }

    pub fn note_live_reconcile_failure(
        &mut self,
        failures: u32,
        next_live_reconcile_at: DateTime<Utc>,
        error: String,
    ) {
        self.live_reconcile_failures = failures;
        self.next_live_reconcile_at = Some(next_live_reconcile_at);
        self.last_live_reconcile_error = Some(error);
    }

    pub fn note_live_reconcile_healthy(&mut self) {
        self.live_reconcile_failures = 0;
        self.next_live_reconcile_at = None;
        self.last_live_reconcile_error = None;
    }

    pub fn status(&self) -> SystemStatus {
        let cutoff = Utc::now() - Duration::hours(1);
        SystemStatus {
            status: match &self.listen_addr {
                Some(listen_addr) => format!("{}@{listen_addr}", self.phase.as_str()),
                None => self.phase.as_str().to_string(),
            },
            uptime_seconds: (Utc::now() - self.booted_at).num_seconds(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            strategy: "platform".to_string(),
            last_trade_time: self.last_trade_time,
            websocket_connected: self.websocket_connected,
            database_connected: self.database_connected,
            error_count_1h: self
                .error_timestamps
                .iter()
                .filter(|timestamp| **timestamp >= cutoff)
                .count() as i64,
            live_reconcile_failures: self.live_reconcile_failures,
            next_live_reconcile_at: self.next_live_reconcile_at,
            last_live_reconcile_error: self.last_live_reconcile_error.clone(),
        }
    }

    pub fn control_response(&self, action: &str) -> SystemControlResponse {
        SystemControlResponse {
            success: true,
            message: format!("system {action} accepted"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SystemService;
    use chrono::{Duration, Utc};

    #[test]
    fn system_service_transitions_through_degraded_and_recovering() {
        let mut service = SystemService::default();
        assert_eq!(service.status().status, "starting");

        service.mark_running("127.0.0.1:8081");
        assert_eq!(service.status().status, "running@127.0.0.1:8081");

        service.mark_degraded("127.0.0.1:8081");
        service.note_live_reconcile_failure(
            2,
            Utc::now() + Duration::seconds(5),
            "gateway offline".to_string(),
        );
        let degraded = service.status();
        assert_eq!(degraded.status, "degraded@127.0.0.1:8081");
        assert_eq!(degraded.error_count_1h, 1);
        assert_eq!(degraded.live_reconcile_failures, 2);
        assert!(degraded.next_live_reconcile_at.is_some());
        assert_eq!(
            degraded.last_live_reconcile_error.as_deref(),
            Some("gateway offline")
        );
        assert!(service.is_degraded());

        service.mark_recovering("127.0.0.1:8081");
        assert_eq!(service.status().status, "recovering@127.0.0.1:8081");

        service.note_live_reconcile_healthy();
        service.mark_running("127.0.0.1:8081");
        assert_eq!(service.status().status, "running@127.0.0.1:8081");
        assert_eq!(service.status().error_count_1h, 1);
        assert_eq!(service.status().live_reconcile_failures, 0);
    }

    #[test]
    fn system_service_tracks_trade_time_and_connectivity() {
        let mut service = SystemService::default();
        let earlier = Utc::now() - Duration::minutes(5);
        let later = Utc::now();

        service.note_trade(Some(earlier));
        service.note_trade(Some(later));
        service.set_database_connected(true);
        service.set_websocket_connected(true);

        let status = service.status();
        assert_eq!(status.last_trade_time, Some(later));
        assert!(status.database_connected);
        assert!(status.websocket_connected);
        assert_eq!(status.live_reconcile_failures, 0);
    }

    #[test]
    fn set_status_restores_phase() {
        let mut service = SystemService::default();
        service.set_status("degraded@127.0.0.1:8081");
        assert_eq!(service.status().status, "degraded@127.0.0.1:8081");

        service.set_status("running");
        assert_eq!(service.status().status, "running");
    }
}
