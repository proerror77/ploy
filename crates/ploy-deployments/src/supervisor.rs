use crate::health::heartbeat;
use crate::protocol::{WorkerLaunchSpec, WorkerStatus};
use crate::runtime::DeploymentRuntime;
use ploy_operator_contracts::ObservedState;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct WorkerSupervisor {
    workers: BTreeMap<String, WorkerStatus>,
}

impl WorkerSupervisor {
    pub fn start(&mut self, spec: WorkerLaunchSpec) -> &WorkerStatus {
        let deployment_id = spec.deployment_id.clone();
        let runtime = DeploymentRuntime::new(spec);
        self.workers.insert(deployment_id.clone(), runtime.boot_status());
        self.workers.get(&deployment_id).expect("worker status")
    }

    pub fn heartbeat(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let status = self.workers.get_mut(deployment_id)?;
        heartbeat(status);
        Some(status)
    }

    pub fn fail(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let status = self.workers.get_mut(deployment_id)?;
        status.observed_state = ObservedState::Failed;
        Some(status)
    }

    pub fn restart(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let status = self.workers.get_mut(deployment_id)?;
        status.observed_state = ObservedState::Starting;
        heartbeat(status);
        Some(status)
    }

    pub fn workers(&self) -> impl Iterator<Item = &WorkerStatus> {
        self.workers.values()
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerSupervisor;
    use crate::protocol::WorkerLaunchSpec;
    use ploy_operator_contracts::{DesiredState, ObservedState};

    #[test]
    fn start_one_worker() {
        let mut supervisor = WorkerSupervisor::default();
        let status = supervisor.start(WorkerLaunchSpec {
            deployment_id: "openclaw.default".to_string(),
            bundle_id: "openclaw".to_string(),
            runtime_mode: "paper".to_string(),
            desired_state: DesiredState::Running,
        });

        assert_eq!(status.observed_state, ObservedState::Starting);
    }

    #[test]
    fn restart_failed_worker() {
        let mut supervisor = WorkerSupervisor::default();
        supervisor.start(WorkerLaunchSpec {
            deployment_id: "openclaw.default".to_string(),
            bundle_id: "openclaw".to_string(),
            runtime_mode: "paper".to_string(),
            desired_state: DesiredState::Running,
        });

        supervisor.fail("openclaw.default");
        let restarted = supervisor.restart("openclaw.default").expect("restarted");
        assert_eq!(restarted.observed_state, ObservedState::Running);
    }
}
