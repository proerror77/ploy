use crate::{build_worker_launch_spec, WorkerTickConfig};
use ploy_deployments::WorkerSupervisor;
use ploy_operator_contracts::DesiredState;
use ploy_platform::{DeploymentRecord, DeploymentRegistry};
use ploy_trading::TradingRuntime;
use std::collections::BTreeMap;

pub fn apply_loaded_registry_state(
    records: Vec<DeploymentRecord>,
    registry: &mut DeploymentRegistry,
    supervisor: &mut WorkerSupervisor,
    trading: &mut BTreeMap<String, TradingRuntime>,
    config: &WorkerTickConfig,
) {
    for record in records {
        let deployment_id = record.deployment_id.clone();
        let desired_state = record.desired_state;

        registry.upsert(record);
        trading.entry(deployment_id.clone()).or_default();

        if desired_state == DesiredState::Running {
            if supervisor.status(&deployment_id).is_none() {
                supervisor.start(build_worker_launch_spec(
                    registry.get(&deployment_id).expect("record inserted"),
                    config,
                ));
            }
            if let Some(status) = supervisor.heartbeat(&deployment_id) {
                registry.set_observed_state(&deployment_id, status.observed_state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_loaded_registry_state;
    use crate::WorkerTickConfig;
    use ploy_deployments::WorkerSupervisor;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::{DeploymentRecord, DeploymentRegistry};
    use ploy_trading::TradingRuntime;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn config() -> WorkerTickConfig {
        WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
            runner_binary: PathBuf::from("/bin/sh"),
            strategy_config_root: PathBuf::from("config/strategies"),
            working_directory: std::env::current_dir().expect("cwd"),
        }
    }

    #[test]
    fn apply_loaded_state_bootstraps_running_workers() {
        let mut registry = DeploymentRegistry::default();
        let mut supervisor = WorkerSupervisor::default();
        let mut trading = BTreeMap::<String, TradingRuntime>::new();

        apply_loaded_registry_state(
            vec![DeploymentRecord {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Starting,
            }],
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config(),
        );

        assert!(registry.get("example.paper").is_some());
        assert!(trading.contains_key("example.paper"));
        assert!(supervisor.status("example.paper").is_some());
    }

    #[test]
    fn apply_loaded_state_does_not_respawn_existing_worker() {
        let mut registry = DeploymentRegistry::default();
        let mut supervisor = WorkerSupervisor::default();
        let mut trading = BTreeMap::<String, TradingRuntime>::new();

        let records = vec![DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        }];

        apply_loaded_registry_state(
            records.clone(),
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config(),
        );
        let first_pid = supervisor
            .status("example.paper")
            .and_then(|status| status.pid)
            .expect("first pid");

        apply_loaded_registry_state(
            records,
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config(),
        );
        let second_pid = supervisor
            .status("example.paper")
            .and_then(|status| status.pid)
            .expect("second pid");

        assert_eq!(first_pid, second_pid);
    }
}
