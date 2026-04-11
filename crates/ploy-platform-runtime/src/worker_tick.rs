use chrono::Duration;
use ploy_deployments::WorkerSupervisor;
use ploy_operator_contracts::{DesiredState, ObservedState};
use ploy_platform::ControlPlane;

#[derive(Debug, Clone)]
pub struct WorkerTickConfig {
    pub listen_addr: String,
    pub worker_heartbeat_stale_after_ms: u64,
}

pub fn tick_workers(
    control_plane: &mut ControlPlane,
    supervisor: &mut WorkerSupervisor,
    config: &WorkerTickConfig,
) {
    let records = control_plane.deployments.records();

    for record in records {
        match record.desired_state {
            DesiredState::Running => {
                if supervisor.status(&record.deployment_id).is_none() {
                    supervisor.start(ploy_deployments::WorkerLaunchSpec {
                        deployment_id: record.deployment_id.clone(),
                        bundle_id: record.bundle_id.clone(),
                        runtime_mode: record.runtime_mode.clone(),
                        desired_state: record.desired_state,
                    });
                }
                if let Some(status) = supervisor.heartbeat(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                    control_plane.system.note_source_heartbeat(
                        format!("worker:{}", record.deployment_id),
                        "worker",
                        Duration::milliseconds(config.worker_heartbeat_stale_after_ms as i64),
                    );
                }
            }
            DesiredState::Paused => {
                control_plane
                    .system
                    .clear_source(&format!("worker:{}", record.deployment_id));
                if let Some(status) = supervisor.pause(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                } else {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, ObservedState::Paused);
                }
            }
            DesiredState::Stopped => {
                control_plane
                    .system
                    .clear_source(&format!("worker:{}", record.deployment_id));
                if let Some(status) = supervisor.stop(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                } else {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, ObservedState::Stopped);
                }
            }
        }
    }
}

pub fn refresh_source_health(control_plane: &mut ControlPlane, listen_addr: &str) {
    let stale_sources = control_plane.system.refresh_source_health();
    let records = control_plane.deployments.records();

    for record in records {
        if record.desired_state != DesiredState::Running {
            continue;
        }
        let worker_stale = control_plane
            .system
            .source_is_stale(&format!("worker:{}", record.deployment_id));
        let live_source_stale = record.runtime_mode == "live"
            && (control_plane.system.source_is_stale("live_reconcile")
                || control_plane.system.source_is_stale("venue:polymarket"));

        if worker_stale || live_source_stale {
            control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Degraded);
        }
    }

    if stale_sources > 0 {
        control_plane.system.mark_degraded(listen_addr);
        return;
    }

    if control_plane.system.is_degraded() {
        control_plane.system.mark_recovering(listen_addr);
    }

    for record in control_plane.deployments.records() {
        if record.desired_state == DesiredState::Running
            && record.observed_state == ObservedState::Degraded
        {
            control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Running);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkerTickConfig, refresh_source_health, tick_workers};
    use ploy_deployments::WorkerSupervisor;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::{ControlPlane, DeploymentRecord};
    use rust_decimal_macros::dec;

    fn config() -> WorkerTickConfig {
        WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
        }
    }

    #[test]
    fn tick_boots_running_workers_and_updates_status() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        tick_workers(&mut control_plane, &mut supervisor, &config());
        assert!(supervisor.status("example.paper").is_some());
    }

    #[test]
    fn stale_sources_degrade_and_then_recover() {
        let mut control_plane = ControlPlane::default();
        control_plane.system.mark_running("127.0.0.1:8081");
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "live".to_string(),
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        });
        control_plane.system.note_source_failure(
            "live_reconcile",
            "live_reconcile",
            chrono::Duration::seconds(15),
            "offline".to_string(),
        );

        refresh_source_health(&mut control_plane, "127.0.0.1:8081");
        assert!(control_plane.system.is_degraded());
    }
}
