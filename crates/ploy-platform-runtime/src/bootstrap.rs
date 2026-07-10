use crate::{WorkerTickConfig, build_worker_launch_spec};
use ploy_deployments::WorkerSupervisor;
use ploy_operator_contracts::{DeploymentState, DesiredState};
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

        if desired_state == DesiredState::Running
            && registry
                .get(&deployment_id)
                .is_some_and(|record| record.deployment_state != DeploymentState::Archived)
        {
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
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Duration as StdDuration;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_id() -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let sequence = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("{}-{unique}-{sequence}", std::process::id())
    }

    fn test_working_directory() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ploy-platform-bootstrap-workdir-{}", test_id()));
        fs::create_dir_all(&path).expect("create test working directory");
        path
    }

    fn test_runner_binary() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ploy-platform-bootstrap-test-runner-{}.sh",
            test_id()
        ));
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, "#!/bin/sh\nsleep 30\n").expect("write test runner");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))
                .expect("chmod test runner");
        }
        fs::rename(&tmp_path, &path).expect("publish test runner");
        path
    }

    fn config() -> WorkerTickConfig {
        WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
            runner_binary: test_runner_binary(),
            strategy_config_root: PathBuf::from("config/strategies"),
            working_directory: test_working_directory(),
        }
    }

    #[test]
    fn apply_loaded_state_bootstraps_running_workers() {
        let mut registry = DeploymentRegistry::default();
        let mut supervisor = WorkerSupervisor::default();
        let mut trading = BTreeMap::<String, TradingRuntime>::new();
        let config = config();

        apply_loaded_registry_state(
            vec![DeploymentRecord {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Starting,
            }],
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config,
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
        let config = config();

        let records = vec![DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        }];

        let first_pid = wait_for_worker_pid(
            records.clone(),
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config,
        );

        let second_pid = wait_for_worker_pid(
            records,
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config,
        );

        assert_eq!(first_pid, second_pid);
    }

    #[test]
    fn archived_running_record_is_not_bootstrapped() {
        let mut registry = DeploymentRegistry::default();
        let mut supervisor = WorkerSupervisor::default();
        let mut trading = BTreeMap::<String, TradingRuntime>::new();
        let config = config();

        apply_loaded_registry_state(
            vec![DeploymentRecord {
                deployment_id: "example.archived".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Archived,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Stopped,
            }],
            &mut registry,
            &mut supervisor,
            &mut trading,
            &config,
        );

        assert!(registry.get("example.archived").is_some());
        assert!(trading.contains_key("example.archived"));
        assert!(supervisor.status("example.archived").is_none());
    }

    fn wait_for_worker_pid(
        records: Vec<DeploymentRecord>,
        registry: &mut DeploymentRegistry,
        supervisor: &mut WorkerSupervisor,
        trading: &mut BTreeMap<String, TradingRuntime>,
        config: &WorkerTickConfig,
    ) -> u32 {
        for _ in 0..20 {
            apply_loaded_registry_state(records.clone(), registry, supervisor, trading, config);
            if let Some(pid) = supervisor
                .status("example.paper")
                .and_then(|status| status.pid)
            {
                return pid;
            }
            thread::sleep(StdDuration::from_millis(10));
        }
        let status = supervisor.status("example.paper");
        panic!(
            "worker did not report a pid after bounded bootstrap ticks; final status={status:?}"
        );
    }
}
