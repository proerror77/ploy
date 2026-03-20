use ploy_operator_contracts::{DeploymentSummary, DesiredState, ObservedState};

#[derive(Debug, Default)]
pub struct ControlPlaneClient;

impl ControlPlaneClient {
    pub fn system_status(&self) -> String {
        "control-plane client ready".to_string()
    }

    pub fn list_deployments(&self) -> Vec<DeploymentSummary> {
        vec![DeploymentSummary {
            deployment_id: "example.paper".to_string(),
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        }]
    }
}
