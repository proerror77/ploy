use super::runtime_spawns::ManagedStrategyRuntimeSpawn;
use super::support::load_strategy_deployments;
use super::*;
use crate::strategy::runtime_specs::deployment_matrix as deployment_matrix_specs;
use crate::strategy::runtime_specs::runtime_plans::{
    collect_managed_runtime_specs, ManagedRuntimeSpec,
};

pub(super) use crate::strategy::runtime_specs::deployment_matrix::RuntimeCryptoStrategyTargets;
#[cfg(all(test, feature = "rl"))]
pub(super) use crate::strategy::runtime_specs::runtime_configs::build_crypto_rl_policy_runtime_config;
#[cfg(test)]
pub(super) use crate::strategy::runtime_specs::runtime_configs::{
    build_crypto_lob_ml_runtime_config, build_event_edge_runtime_config,
    build_momentum_runtime_config, build_nba_comeback_runtime_config,
    build_split_arb_runtime_config,
};
pub(super) use crate::strategy::runtime_specs::runtime_plans::{
    ManagedRuntimeBootstrapStep, ManagedRuntimeDataPlaneKind,
};

#[derive(Debug, Clone)]
pub(super) struct ManagedStrategyRuntimePlan {
    pub(super) spawn: ManagedStrategyRuntimeSpawn,
    pub(super) data_plane: ManagedRuntimeDataPlaneKind,
    pub(super) bootstrap_step: ManagedRuntimeBootstrapStep,
}

pub(super) fn collect_runtime_crypto_strategy_targets(
    runtime_account_id: &str,
    runtime_dry_run: bool,
) -> RuntimeCryptoStrategyTargets {
    let deployments = load_strategy_deployments();
    deployment_matrix_specs::collect_runtime_crypto_strategy_targets(
        &deployments,
        runtime_account_id,
        runtime_dry_run,
    )
}

pub(super) fn apply_strategy_deployments(
    cfg: &mut PlatformBootstrapConfig,
    deployments: &[StrategyDeployment],
    runtime_account_id: &str,
    runtime_dry_run: bool,
) {
    deployment_matrix_specs::apply_strategy_deployments(
        cfg,
        deployments,
        runtime_account_id,
        runtime_dry_run,
    );
}

fn managed_runtime_plan_from_spec(spec: ManagedRuntimeSpec) -> ManagedStrategyRuntimePlan {
    ManagedStrategyRuntimePlan {
        spawn: ManagedStrategyRuntimeSpawn {
            strategy_label: spec.strategy_label,
            agent_id: spec.agent_id,
            domain: spec.domain,
            risk_params: spec.risk_params,
            strategy_config_toml: spec.strategy_config_toml,
        },
        data_plane: spec.data_plane,
        bootstrap_step: spec.bootstrap_step,
    }
}

pub(super) fn collect_managed_strategy_runtime_plans(
    config: &PlatformBootstrapConfig,
    app_config: &AppConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
) -> Vec<ManagedStrategyRuntimePlan> {
    collect_managed_runtime_specs(config, app_config, runtime_crypto_targets)
        .into_iter()
        .map(managed_runtime_plan_from_spec)
        .collect()
}
