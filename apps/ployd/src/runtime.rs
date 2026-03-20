use crate::config::PlatformConfig;
use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::{DeploymentApplyRequest, DesiredState, ObservedState};
use ploy_platform::{ControlPlane, DeploymentRecord};
use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub struct PloyDaemon {
    pub config: PlatformConfig,
    pub control_plane: ControlPlane,
    pub supervisor: WorkerSupervisor,
}

impl PloyDaemon {
    pub fn boot(config: &PlatformConfig) -> io::Result<Self> {
        let mut control_plane = ControlPlane::default();
        control_plane
            .system
            .set_status(format!("running@{}", config.listen_addr));

        let mut daemon = Self {
            config: config.clone(),
            control_plane,
            supervisor: WorkerSupervisor::default(),
        };
        daemon.load_registry()?;
        daemon.tick();

        Ok(daemon)
    }

    pub fn write_runtime_snapshots(&mut self) -> io::Result<()> {
        self.load_registry()?;
        self.tick();
        self.persist_registry()?;
        fs::create_dir_all(&self.config.runtime_root)?;
        write_json(
            &self.config.status_file,
            &self.control_plane.system.status(),
        )?;
        write_json(
            &self.config.deployment_status_file,
            &self.control_plane.deployments.summaries(),
        )?;
        Ok(())
    }

    pub fn run_forever(&mut self) -> io::Result<()> {
        loop {
            self.write_runtime_snapshots()?;
            thread::sleep(Duration::from_millis(self.config.tick_interval_ms));
        }
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Option<DeploymentRecord> {
        self.control_plane.deployments.get(deployment_id).cloned()
    }

    pub fn apply_deployment(
        &mut self,
        request: DeploymentApplyRequest,
    ) -> io::Result<DeploymentRecord> {
        let record = DeploymentRecord {
            deployment_id: request.deployment_id,
            bundle_id: request.bundle_id,
            runtime_mode: request.runtime_mode,
            desired_state: request.desired_state,
            observed_state: observed_state_for_desired(request.desired_state),
        };
        self.control_plane.deployments.upsert(record.clone());
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(self
            .control_plane
            .deployments
            .get(&record.deployment_id)
            .cloned()
            .expect("deployment persisted"))
    }

    pub fn set_desired_state(
        &mut self,
        deployment_id: &str,
        desired_state: DesiredState,
    ) -> io::Result<Option<DeploymentRecord>> {
        let Some(record) = self.control_plane.deployments.get(deployment_id).cloned() else {
            return Ok(None);
        };

        self.control_plane
            .deployments
            .set_desired_state(deployment_id, desired_state);
        self.control_plane
            .deployments
            .set_observed_state(deployment_id, observed_state_for_desired(desired_state));
        self.persist_registry()?;
        self.write_runtime_snapshots()?;
        Ok(self
            .control_plane
            .deployments
            .get(&record.deployment_id)
            .cloned())
    }

    fn load_registry(&mut self) -> io::Result<()> {
        if !self.config.registry_file.exists() {
            return Ok(());
        }

        let raw = fs::read_to_string(&self.config.registry_file)?;
        if raw.trim().is_empty() {
            return Ok(());
        }

        let records: Vec<DeploymentRecord> = serde_json::from_str(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        for record in records {
            let deployment_id = record.deployment_id.clone();
            let desired_state = record.desired_state;
            let bundle_id = record.bundle_id.clone();
            let runtime_mode = record.runtime_mode.clone();
            self.control_plane.deployments.upsert(record);

            if desired_state == DesiredState::Running {
                self.supervisor.start(WorkerLaunchSpec {
                    deployment_id: deployment_id.clone(),
                    bundle_id,
                    runtime_mode,
                    desired_state,
                });
                if let Some(status) = self.supervisor.heartbeat(&deployment_id) {
                    self.control_plane
                        .deployments
                        .set_observed_state(&deployment_id, status.observed_state);
                }
            }
        }

        Ok(())
    }

    fn tick(&mut self) {
        let records = self.control_plane.deployments.records();

        for record in records {
            match record.desired_state {
                DesiredState::Running => {
                    if self.supervisor.status(&record.deployment_id).is_none() {
                        self.supervisor.start(WorkerLaunchSpec {
                            deployment_id: record.deployment_id.clone(),
                            bundle_id: record.bundle_id.clone(),
                            runtime_mode: record.runtime_mode.clone(),
                            desired_state: record.desired_state,
                        });
                    }
                    if let Some(status) = self.supervisor.heartbeat(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    }
                }
                DesiredState::Paused => {
                    if let Some(status) = self.supervisor.pause(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    } else {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, ObservedState::Paused);
                    }
                }
                DesiredState::Stopped => {
                    if let Some(status) = self.supervisor.stop(&record.deployment_id) {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, status.observed_state);
                    } else {
                        self.control_plane
                            .deployments
                            .set_observed_state(&record.deployment_id, ObservedState::Stopped);
                    }
                }
            }
        }
    }

    fn persist_registry(&self) -> io::Result<()> {
        write_json(
            &self.config.registry_file,
            &self.control_plane.deployments.records(),
        )
    }
}

fn observed_state_for_desired(desired_state: DesiredState) -> ObservedState {
    match desired_state {
        DesiredState::Running => ObservedState::Starting,
        DesiredState::Paused => ObservedState::Paused,
        DesiredState::Stopped => ObservedState::Stopped,
    }
}

fn write_json<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::PloyDaemon;
    use crate::config::PlatformConfig;
    use ploy_operator_contracts::{DesiredState, ObservedState};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployd-{label}-{unique}"))
    }

    #[test]
    fn daemon_loads_platform_config() {
        let config = PlatformConfig {
            listen_addr: "127.0.0.1:9090".to_string(),
            ..PlatformConfig::default()
        };

        let daemon = PloyDaemon::boot(&config).expect("boot");
        let status = daemon.control_plane.system.status();
        assert!(status.status.contains("127.0.0.1:9090"));
    }

    #[test]
    fn daemon_writes_runtime_snapshots_for_operator_clients() {
        let root = temp_dir("snapshots");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "starting"
                }
            ])
            .to_string(),
        )
        .expect("write registry");

        let config = PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            tick_interval_ms: 5,
            ..PlatformConfig::default()
        };

        let mut daemon = PloyDaemon::boot(&config).expect("boot");
        daemon.write_runtime_snapshots().expect("write snapshots");

        let status: ploy_operator_contracts::SystemStatus =
            serde_json::from_str(&fs::read_to_string(&config.status_file).expect("status file"))
                .expect("status json");
        assert!(status.status.contains("127.0.0.1:8081"));

        let deployments: Vec<ploy_operator_contracts::DeploymentSummary> = serde_json::from_str(
            &fs::read_to_string(&config.deployment_status_file).expect("deployment file"),
        )
        .expect("deployment json");
        assert_eq!(deployments.len(), 1);
        assert_eq!(deployments[0].deployment_id, "example.paper");
        assert_eq!(deployments[0].desired_state, DesiredState::Running);
        assert_eq!(deployments[0].observed_state, ObservedState::Running);
    }
}
