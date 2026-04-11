use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::DesiredState;
use ploy_platform::{DeploymentRecord, DeploymentRegistry};
use ploy_trading::TradingRuntime;
use std::collections::BTreeMap;

pub fn apply_loaded_registry_state(
    records: Vec<DeploymentRecord>,
    registry: &mut DeploymentRegistry,
    supervisor: &mut WorkerSupervisor,
    trading: &mut BTreeMap<String, TradingRuntime>,
) {
    for record in records {
        let deployment_id = record.deployment_id.clone();
        let desired_state = record.desired_state;
        let bundle_id = record.bundle_id.clone();
        let runtime_mode = record.runtime_mode.clone();

        registry.upsert(record);
        trading.entry(deployment_id.clone()).or_default();

        if desired_state == DesiredState::Running {
            supervisor.start(WorkerLaunchSpec {
                deployment_id: deployment_id.clone(),
                bundle_id,
                runtime_mode,
                desired_state,
            });
            if let Some(status) = supervisor.heartbeat(&deployment_id) {
                registry.set_observed_state(&deployment_id, status.observed_state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_loaded_registry_state;
    use ploy_deployments::WorkerSupervisor;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::{DeploymentRecord, DeploymentRegistry};
    use ploy_trading::TradingRuntime;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

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
        );

        assert!(registry.get("example.paper").is_some());
        assert!(trading.contains_key("example.paper"));
        assert!(supervisor.status("example.paper").is_some());
    }
}
