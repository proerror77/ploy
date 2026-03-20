use ploy_operator_contracts::{
    ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest, DeploymentSummary,
    DesiredState, OperatorEvent, SystemStatus, TradingStateSnapshot,
};
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ControlPlaneClient {
    pub control_plane_addr: String,
    pub runtime_root: PathBuf,
}

impl ControlPlaneClient {
    pub fn from_runtime_root(runtime_root: impl Into<PathBuf>) -> Self {
        Self {
            control_plane_addr: "127.0.0.1:8081".to_string(),
            runtime_root: runtime_root.into(),
        }
    }

    pub fn system_status(&self) -> String {
        match self.read_status_snapshot() {
            Ok(status) => format!(
                "status={} uptime={}s version={}",
                status.status, status.uptime_seconds, status.version
            ),
            Err(err) => format!("status=unavailable error={err}"),
        }
    }

    pub fn list_deployments(&self) -> Vec<DeploymentSummary> {
        self.read_deployment_snapshots().unwrap_or_default()
    }

    pub fn trading_state(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        self.read_trading_state_snapshot()
    }

    pub fn system_snapshot(&self) -> Result<SystemStatus, String> {
        self.read_status_snapshot()
    }

    pub fn deployment_summaries(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.read_deployment_snapshots()
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Result<DeploymentSummary, String> {
        match self.read_deployment_over_http(deployment_id) {
            Ok(deployment) => Ok(deployment),
            Err(err) if should_fallback_to_snapshot(&err) => self
                .deployment_summaries()?
                .into_iter()
                .find(|deployment| deployment.deployment_id == deployment_id)
                .ok_or_else(|| format!("deployment `{deployment_id}` was not found")),
            Err(err) => Err(err),
        }
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<OperatorEvent>, String> {
        let mut stream = TcpStream::connect(&self.control_plane_addr)
            .map_err(|err| format!("connect {}: {err}", self.control_plane_addr))?;
        let request = format!(
            "GET /api/events/stream HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.control_plane_addr
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("write request: {err}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .map_err(|err| format!("set read timeout: {err}"))?;
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|err| format!("read HTTP status: {err}"))?;
        if !status_line.contains("200") {
            return Err(format!("unexpected HTTP status: {}", status_line.trim()));
        }

        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|err| format!("read headers: {err}"))?;
            if bytes == 0 || line == "\r\n" {
                break;
            }
        }

        let mut events = Vec::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if let Some(payload) = trimmed.strip_prefix("data: ") {
                        let event: OperatorEvent = serde_json::from_str(payload)
                            .map_err(|err| format!("parse event payload: {err}"))?;
                        events.push(event);
                        if events.len() >= limit {
                            break;
                        }
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(err) => return Err(format!("read event stream: {err}")),
            }
        }

        Ok(events)
    }

    pub fn inspect_trading_state(
        &self,
        deployment_id: &str,
    ) -> Result<TradingStateSnapshot, String> {
        self.trading_state()?
            .into_iter()
            .find(|state| state.deployment_id == deployment_id)
            .ok_or_else(|| format!("trading state for `{deployment_id}` was not found"))
    }

    pub fn apply_deployment(
        &self,
        request: &DeploymentApplyRequest,
    ) -> Result<DeploymentSummary, String> {
        self.send_json(
            "PUT",
            &format!("/api/deployments/{}", request.deployment_id),
            request,
        )
    }

    pub fn set_desired_state(
        &self,
        deployment_id: &str,
        desired_state: DesiredState,
    ) -> Result<DeploymentSummary, String> {
        self.send_json(
            "POST",
            &format!("/api/deployments/{deployment_id}/control"),
            &DeploymentControlRequest { desired_state },
        )
    }

    fn read_status_snapshot(&self) -> Result<SystemStatus, String> {
        if let Ok(status) = self.read_status_over_http() {
            return Ok(status);
        }
        let body = fs::read_to_string(self.runtime_root.join("system-status.json"))
            .map_err(|err| format!("read status snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse status snapshot: {err}"))
    }

    fn read_deployment_snapshots(&self) -> Result<Vec<DeploymentSummary>, String> {
        if let Ok(deployments) = self.read_deployments_over_http() {
            return Ok(deployments);
        }
        let body = fs::read_to_string(self.runtime_root.join("deployments.json"))
            .map_err(|err| format!("read deployment snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse deployment snapshot: {err}"))
    }

    fn read_trading_state_snapshot(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        if let Ok(snapshot) = self.read_trading_state_over_http() {
            return Ok(snapshot);
        }
        let body = fs::read_to_string(self.runtime_root.join("trading-state.json"))
            .map_err(|err| format!("read trading state snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse trading state snapshot: {err}"))
    }

    fn read_status_over_http(&self) -> Result<SystemStatus, String> {
        self.get_json("/api/system/status")
    }

    fn read_deployments_over_http(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.get_json("/api/deployments")
    }

    fn read_deployment_over_http(&self, deployment_id: &str) -> Result<DeploymentSummary, String> {
        self.get_json(&format!("/api/deployments/{deployment_id}"))
    }

    fn read_trading_state_over_http(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        self.get_json("/api/trading/state")
    }

    fn get_json<T>(&self, path: &str) -> Result<T, String>
    where
        T: DeserializeOwned,
    {
        let mut stream = TcpStream::connect(&self.control_plane_addr)
            .map_err(|err| format!("connect {}: {err}", self.control_plane_addr))?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.control_plane_addr
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("write request: {err}"))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|err| format!("read response: {err}"))?;

        let (status_line, body) = split_http_response(&response)?;
        if !status_line.contains("200") {
            return Err(decode_http_error(status_line, body));
        }
        serde_json::from_str(body).map_err(|err| format!("parse HTTP body: {err}"))
    }

    fn send_json<B, T>(&self, method: &str, path: &str, body: &B) -> Result<T, String>
    where
        B: serde::Serialize,
        T: DeserializeOwned,
    {
        let mut stream = TcpStream::connect(&self.control_plane_addr)
            .map_err(|err| format!("connect {}: {err}", self.control_plane_addr))?;
        let body = serde_json::to_string(body).map_err(|err| format!("serialize body: {err}"))?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.control_plane_addr,
            body.len(),
            body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("write request: {err}"))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|err| format!("read response: {err}"))?;

        let (status_line, body) = split_http_response(&response)?;
        if !status_line.contains("200") {
            return Err(decode_http_error(status_line, body));
        }
        serde_json::from_str(body).map_err(|err| format!("parse HTTP body: {err}"))
    }
}

fn split_http_response(response: &str) -> Result<(&str, &str), String> {
    let mut parts = response.splitn(2, "\r\n\r\n");
    let status_line = parts
        .next()
        .and_then(|headers| headers.lines().next())
        .ok_or_else(|| "missing HTTP status line".to_string())?;
    let body = parts
        .next()
        .ok_or_else(|| "missing HTTP body".to_string())?;
    Ok((status_line, body))
}

fn decode_http_error(status_line: &str, body: &str) -> String {
    if let Ok(error) = serde_json::from_str::<ControlPlaneErrorResponse>(body) {
        match error.message {
            Some(message) => format!("HTTP {} {}: {}", status_line.trim(), error.error, message),
            None => format!("HTTP {} {}", status_line.trim(), error.error),
        }
    } else {
        format!("unexpected HTTP status: {}", status_line.trim())
    }
}

fn should_fallback_to_snapshot(error: &str) -> bool {
    error.starts_with("connect ")
        || error.starts_with("write request:")
        || error.starts_with("read response:")
        || error.starts_with("missing HTTP ")
}

impl Default for ControlPlaneClient {
    fn default() -> Self {
        Self::from_runtime_root(Path::new("run/platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneClient;
    use ploy_operator_contracts::{
        DeploymentApplyRequest, DeploymentSnapshotEvent, DeploymentSummary, DesiredState,
        ObservedState, OperatorEvent, SystemSnapshotEvent, SystemStatus, TradingStateSnapshot,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-{label}-{unique}"))
    }

    #[test]
    fn client_reads_system_and_deployment_snapshots() {
        let runtime_root = temp_dir("runtime");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::to_string(&SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 42,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
            })
            .expect("status json"),
        )
        .expect("write status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::to_string(&vec![DeploymentSummary {
                deployment_id: "example.paper".to_string(),
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Running,
            }])
            .expect("deployments json"),
        )
        .expect("write deployments");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        assert!(client.system_status().contains("running"));

        let deployments = client.list_deployments();
        assert_eq!(deployments.len(), 1);
        assert_eq!(
            client
                .inspect_deployment("example.paper")
                .expect("deployment"),
            deployments[0]
        );
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                ..TradingStateSnapshot::default()
            }])
            .expect("trading json"),
        )
        .expect("write trading state");
        assert_eq!(
            client
                .inspect_trading_state("example.paper")
                .expect("trading state")
                .risk
                .open_positions,
            0,
        );
    }

    #[test]
    fn client_prefers_http_control_plane_when_available() {
        let runtime_root = temp_dir("http-runtime");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 1024];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("GET /api/system/status") {
                    serde_json::json!({
                        "status": "running-via-http",
                        "uptime_seconds": 99,
                        "version": "0.1.0",
                        "strategy": "platform",
                        "last_trade_time": null,
                        "websocket_connected": false,
                        "database_connected": false,
                        "error_count_1h": 0
                    })
                    .to_string()
                } else if request.starts_with("GET /api/deployments/http.paper") {
                    serde_json::json!({
                        "deployment_id": "http.paper",
                        "desired_state": "running",
                        "observed_state": "running"
                    })
                    .to_string()
                } else {
                    serde_json::json!([
                        {
                            "deployment_id": "http.paper",
                            "desired_state": "running",
                            "observed_state": "running"
                        }
                    ])
                    .to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        assert!(client.system_status().contains("running-via-http"));
        assert_eq!(
            client
                .inspect_deployment("http.paper")
                .expect("deployment over http")
                .deployment_id,
            "http.paper"
        );
    }

    #[test]
    fn client_applies_and_controls_deployment_over_http() {
        let runtime_root = temp_dir("http-mutate");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 2048];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("PUT /api/deployments/example.paper") {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "desired_state": "running",
                        "observed_state": "starting"
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "desired_state": "paused",
                        "observed_state": "paused"
                    })
                    .to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let applied = client
            .apply_deployment(&DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                desired_state: DesiredState::Running,
            })
            .expect("apply");
        assert_eq!(applied.deployment_id, "example.paper");

        let paused = client
            .set_desired_state("example.paper", DesiredState::Paused)
            .expect("pause");
        assert_eq!(paused.desired_state, DesiredState::Paused);
    }

    #[test]
    fn client_reads_recent_events_from_sse() {
        let runtime_root = temp_dir("http-events");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let event1 =
                serde_json::to_string(&OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
                    system: SystemStatus {
                        status: "running".to_string(),
                        uptime_seconds: 7,
                        version: "0.1.0".to_string(),
                        strategy: "platform".to_string(),
                        last_trade_time: None,
                        websocket_connected: false,
                        database_connected: false,
                        error_count_1h: 0,
                    },
                }))
                .expect("event1");
            let event2 = serde_json::to_string(&OperatorEvent::DeploymentSnapshot(
                DeploymentSnapshotEvent {
                    deployments: vec![DeploymentSummary {
                        deployment_id: "example.paper".to_string(),
                        desired_state: DesiredState::Running,
                        observed_state: ObservedState::Running,
                    }],
                },
            ))
            .expect("event2");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {event1}\n\ndata: {event2}\n\n"
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let events = client.recent_events(2).expect("recent events");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], OperatorEvent::SystemSnapshot(_)));
        assert!(matches!(events[1], OperatorEvent::DeploymentSnapshot(_)));
    }

    #[test]
    fn client_preserves_http_error_body_details() {
        let runtime_root = temp_dir("http-error");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "error": "deployment_not_found",
                "message": "deployment `missing.paper` was not found",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let error = client
            .inspect_deployment("missing.paper")
            .expect_err("http error");
        assert!(error.contains("404"));
        assert!(error.contains("deployment_not_found"));
        assert!(error.contains("missing.paper"));
    }
}
