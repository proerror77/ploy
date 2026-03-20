use crate::protocol::{WorkerLaunchSpec, WorkerStatus};
use chrono::Utc;
use ploy_operator_contracts::ObservedState;

#[derive(Debug, Clone)]
pub struct DeploymentRuntime {
    spec: WorkerLaunchSpec,
}

impl DeploymentRuntime {
    pub fn new(spec: WorkerLaunchSpec) -> Self {
        Self { spec }
    }

    pub fn boot_status(&self) -> WorkerStatus {
        WorkerStatus {
            deployment_id: self.spec.deployment_id.clone(),
            observed_state: ObservedState::Starting,
            last_heartbeat: Utc::now(),
        }
    }
}
