use ploy_operator_contracts::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentSummary, DesiredState, SystemStatus,
};
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{Read, Write};
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

    pub fn inspect_deployment(&self, deployment_id: &str) -> Option<DeploymentSummary> {
        self.list_deployments()
            .into_iter()
            .find(|deployment| deployment.deployment_id == deployment_id)
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

    fn read_status_over_http(&self) -> Result<SystemStatus, String> {
        self.get_json("/api/system/status")
    }

    fn read_deployments_over_http(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.get_json("/api/deployments")
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

        let mut parts = response.splitn(2, "\r\n\r\n");
        let status_line = parts
            .next()
            .and_then(|headers| headers.lines().next())
            .ok_or_else(|| "missing HTTP status line".to_string())?;
        if !status_line.contains("200") {
            return Err(format!("unexpected HTTP status: {status_line}"));
        }

        let body = parts
            .next()
            .ok_or_else(|| "missing HTTP body".to_string())?;
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

        let mut parts = response.splitn(2, "\r\n\r\n");
        let status_line = parts
            .next()
            .and_then(|headers| headers.lines().next())
            .ok_or_else(|| "missing HTTP status line".to_string())?;
        if !status_line.contains("200") {
            return Err(format!("unexpected HTTP status: {status_line}"));
        }
        let body = parts
            .next()
            .ok_or_else(|| "missing HTTP body".to_string())?;
        serde_json::from_str(body).map_err(|err| format!("parse HTTP body: {err}"))
    }
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
        DeploymentApplyRequest, DeploymentSummary, DesiredState, ObservedState, SystemStatus,
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
}
