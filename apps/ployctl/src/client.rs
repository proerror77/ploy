use ploy_operator_contracts::{DeploymentSummary, SystemStatus};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ControlPlaneClient {
    pub runtime_root: PathBuf,
}

impl ControlPlaneClient {
    pub fn from_runtime_root(runtime_root: impl Into<PathBuf>) -> Self {
        Self {
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

    fn read_status_snapshot(&self) -> Result<SystemStatus, String> {
        let body = fs::read_to_string(self.runtime_root.join("system-status.json"))
            .map_err(|err| format!("read status snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse status snapshot: {err}"))
    }

    fn read_deployment_snapshots(&self) -> Result<Vec<DeploymentSummary>, String> {
        let body = fs::read_to_string(self.runtime_root.join("deployments.json"))
            .map_err(|err| format!("read deployment snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse deployment snapshot: {err}"))
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
    use ploy_operator_contracts::{DeploymentSummary, DesiredState, ObservedState, SystemStatus};
    use std::fs;
    use std::path::PathBuf;
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
            client.inspect_deployment("example.paper").expect("deployment"),
            deployments[0]
        );
    }
}
