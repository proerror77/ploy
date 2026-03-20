use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::{DesiredState, ObservedState};
use ploy_platform::{ControlPlane, DeploymentRecord};

#[test]
fn platform_smoke_registers_and_starts_one_deployment() {
    let mut control_plane = ControlPlane::default();
    control_plane.deployments.upsert(DeploymentRecord {
        deployment_id: "example.paper".to_string(),
        bundle_id: "openclaw".to_string(),
        runtime_mode: "paper".to_string(),
        desired_state: DesiredState::Running,
        observed_state: ObservedState::Starting,
    });

    let mut supervisor = WorkerSupervisor::default();
    supervisor.start(WorkerLaunchSpec {
        deployment_id: "example.paper".to_string(),
        bundle_id: "openclaw".to_string(),
        runtime_mode: "paper".to_string(),
        desired_state: DesiredState::Running,
    });
    supervisor.heartbeat("example.paper");

    let summary = control_plane
        .deployments
        .summaries()
        .into_iter()
        .next()
        .expect("deployment summary");
    assert_eq!(summary.deployment_id, "example.paper");
    assert_eq!(summary.desired_state, DesiredState::Running);
    assert_eq!(supervisor.workers().count(), 1);
    assert_eq!(
        supervisor
            .workers()
            .next()
            .expect("worker")
            .observed_state,
        ObservedState::Running
    );
}
