use crate::agents::crypto::CryptoTradingConfig;
use crate::agents::politics::PoliticsTradingConfig;
use crate::agents::sports::SportsTradingConfig;
use crate::config::{EventEdgeAgentConfig, NbaComebackConfig};
use crate::coordinator::runtime_specs::{
    build_event_edge_runtime_config, build_momentum_runtime_config,
    build_nba_comeback_runtime_config, build_pattern_memory_runtime_config,
    build_split_arb_runtime_config,
};
use crate::error::{PloyError, Result};
use crate::platform::Domain;

use super::{
    ComposableCryptoSpec, PluginDefinition, PluginDeployment, PluginKind, PluginSpec,
    RegisteredStrategySpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedRuntimeSpec {
    pub strategy_label: String,
    pub agent_id: String,
    pub domain: Domain,
    pub strategy_config_toml: String,
}

fn validate_plugin_identity(
    definition: &PluginDefinition,
    deployment: &PluginDeployment,
    expected_plugin_id: &str,
    expected_kind: PluginKind,
    expected_domain: Domain,
) -> Result<()> {
    if definition.plugin_id != expected_plugin_id {
        return Err(PloyError::Validation(format!(
            "plugin projector expected plugin_id {}, got {}",
            expected_plugin_id, definition.plugin_id
        )));
    }
    if deployment.plugin_id != definition.plugin_id {
        return Err(PloyError::Validation(format!(
            "plugin deployment {} targets {}, but definition is {}",
            deployment.deployment_id, deployment.plugin_id, definition.plugin_id
        )));
    }
    if definition.kind != expected_kind {
        return Err(PloyError::Validation(format!(
            "plugin {} has kind {}, expected {}",
            definition.plugin_id, definition.kind, expected_kind
        )));
    }
    if definition.domain != expected_domain {
        return Err(PloyError::Validation(format!(
            "plugin {} has domain {}, expected {}",
            definition.plugin_id, definition.domain, expected_domain
        )));
    }

    Ok(())
}

fn require_composable_crypto_spec<'a>(
    definition: &PluginDefinition,
    spec: &'a PluginSpec,
    expected_signal_block: &str,
) -> Result<&'a ComposableCryptoSpec> {
    match spec {
        PluginSpec::ComposableCrypto(inner) => {
            if !inner
                .signal_blocks
                .iter()
                .any(|signal| signal == expected_signal_block)
            {
                return Err(PloyError::Validation(format!(
                    "plugin {} missing expected signal block {}",
                    definition.plugin_id, expected_signal_block
                )));
            }
            Ok(inner)
        }
        other => Err(PloyError::Validation(format!(
            "plugin {} expected composable crypto spec, got {:?}",
            definition.plugin_id, other
        ))),
    }
}

fn build_composable_crypto_runtime_config(
    definition: &PluginDefinition,
    spec: &ComposableCryptoSpec,
    base_config_toml: String,
) -> Result<String> {
    let mut config: toml::Value = toml::from_str(&base_config_toml).map_err(|err| {
        PloyError::Validation(format!(
            "plugin {} produced invalid composable base config: {err}",
            definition.plugin_id
        ))
    })?;
    let root = config.as_table_mut().ok_or_else(|| {
        PloyError::Validation(format!(
            "plugin {} composable runtime config must be a TOML table",
            definition.plugin_id
        ))
    })?;

    let strategy = root
        .entry("strategy")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| {
            PloyError::Validation(format!(
                "plugin {} runtime [strategy] section must be a TOML table",
                definition.plugin_id
            ))
        })?;
    strategy.insert(
        "name".to_string(),
        toml::Value::String("composable_crypto".to_string()),
    );
    strategy.insert(
        "plugin_id".to_string(),
        toml::Value::String(definition.plugin_id.clone()),
    );

    let mut composable = toml::map::Map::new();
    composable.insert(
        "signal_blocks".to_string(),
        toml::Value::Array(
            spec.signal_blocks
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect(),
        ),
    );
    composable.insert("filters".to_string(), block_array(&["volatility_gate"]));
    composable.insert("entry".to_string(), block_array(&["marketable_limit"]));
    composable.insert("exit".to_string(), block_array(&["trailing_stop"]));
    composable.insert("sizing".to_string(), block_array(&["fixed_shares"]));
    root.insert(
        "composable_crypto".to_string(),
        toml::Value::Table(composable),
    );

    toml::to_string(&config).map_err(|err| {
        PloyError::Validation(format!(
            "failed to serialize composable runtime config for plugin {}: {err}",
            definition.plugin_id
        ))
    })
}

fn block_array(types: &[&str]) -> toml::Value {
    toml::Value::Array(
        types
            .iter()
            .map(|block_type| {
                let mut table = toml::map::Map::new();
                table.insert(
                    "type".to_string(),
                    toml::Value::String((*block_type).to_string()),
                );
                toml::Value::Table(table)
            })
            .collect(),
    )
}

fn require_registered_strategy_spec<'a>(
    definition: &PluginDefinition,
    spec: &'a PluginSpec,
    expected_strategy_name: &str,
) -> Result<&'a RegisteredStrategySpec> {
    match spec {
        PluginSpec::RegisteredStrategy(inner) => {
            if inner.strategy_name != expected_strategy_name {
                return Err(PloyError::Validation(format!(
                    "plugin {} expected registered strategy {}, got {}",
                    definition.plugin_id, expected_strategy_name, inner.strategy_name
                )));
            }
            Ok(inner)
        }
        other => Err(PloyError::Validation(format!(
            "plugin {} expected registered strategy spec, got {:?}",
            definition.plugin_id, other
        ))),
    }
}

pub(crate) fn project_momentum_runtime_spec(
    definition: &PluginDefinition,
    spec: &PluginSpec,
    deployment: &PluginDeployment,
    symbols: &[String],
    crypto_cfg: &CryptoTradingConfig,
) -> Result<ProjectedRuntimeSpec> {
    validate_plugin_identity(
        definition,
        deployment,
        "crypto.momentum.v1",
        PluginKind::ComposableCrypto,
        Domain::Crypto,
    )?;
    let inner = require_composable_crypto_spec(definition, spec, "momentum")?;
    let strategy_config_toml = build_composable_crypto_runtime_config(
        definition,
        inner,
        build_momentum_runtime_config(symbols, crypto_cfg),
    )?;

    Ok(ProjectedRuntimeSpec {
        strategy_label: "composable_crypto".to_string(),
        agent_id: crypto_cfg.agent_id.clone(),
        domain: Domain::Crypto,
        strategy_config_toml,
    })
}

pub(crate) fn project_pattern_memory_runtime_spec(
    definition: &PluginDefinition,
    spec: &PluginSpec,
    deployment: &PluginDeployment,
    coins: &[String],
) -> Result<ProjectedRuntimeSpec> {
    validate_plugin_identity(
        definition,
        deployment,
        "crypto.pattern_memory.v1",
        PluginKind::ComposableCrypto,
        Domain::Crypto,
    )?;
    let _ = require_composable_crypto_spec(definition, spec, "pattern_memory")?;

    Ok(ProjectedRuntimeSpec {
        strategy_label: "pattern_memory".to_string(),
        agent_id: "pattern_memory".to_string(),
        domain: Domain::Crypto,
        strategy_config_toml: build_pattern_memory_runtime_config(coins)?,
    })
}

pub(crate) fn project_event_edge_runtime_spec(
    definition: &PluginDefinition,
    spec: &PluginSpec,
    deployment: &PluginDeployment,
    rest_url: &str,
    politics_cfg: &PoliticsTradingConfig,
    ee_cfg: &EventEdgeAgentConfig,
) -> Result<ProjectedRuntimeSpec> {
    validate_plugin_identity(
        definition,
        deployment,
        "politics.event_edge.v1",
        PluginKind::RegisteredStrategy,
        Domain::Politics,
    )?;
    let _ = require_registered_strategy_spec(definition, spec, "event_edge")?;

    Ok(ProjectedRuntimeSpec {
        strategy_label: "event_edge".to_string(),
        agent_id: politics_cfg.agent_id.clone(),
        domain: Domain::Politics,
        strategy_config_toml: build_event_edge_runtime_config(rest_url, ee_cfg),
    })
}

pub(crate) fn project_nba_comeback_runtime_spec(
    definition: &PluginDefinition,
    spec: &PluginSpec,
    deployment: &PluginDeployment,
    database_url: &str,
    sports_cfg: &SportsTradingConfig,
    nba_cfg: &NbaComebackConfig,
) -> Result<Option<ProjectedRuntimeSpec>> {
    validate_plugin_identity(
        definition,
        deployment,
        "sports.nba_comeback.v1",
        PluginKind::RegisteredStrategy,
        Domain::Sports,
    )?;
    let _ = require_registered_strategy_spec(definition, spec, "nba_comeback")?;

    if nba_cfg.grok_enabled {
        return Ok(None);
    }

    Ok(Some(ProjectedRuntimeSpec {
        strategy_label: "nba_comeback".to_string(),
        agent_id: sports_cfg.agent_id.clone(),
        domain: Domain::Sports,
        strategy_config_toml: build_nba_comeback_runtime_config(database_url, nba_cfg),
    }))
}

pub(crate) fn project_split_arb_runtime_spec(
    definition: &PluginDefinition,
    spec: &PluginSpec,
    deployment: &PluginDeployment,
    symbols: &[String],
    series_ids: &[String],
) -> Result<ProjectedRuntimeSpec> {
    validate_plugin_identity(
        definition,
        deployment,
        "crypto.split_arb.v1",
        PluginKind::ComposableCrypto,
        Domain::Crypto,
    )?;
    let _ = require_composable_crypto_spec(definition, spec, "split_arb")?;

    Ok(ProjectedRuntimeSpec {
        strategy_label: "split_arb".to_string(),
        agent_id: "split_arb".to_string(),
        domain: Domain::Crypto,
        strategy_config_toml: build_split_arb_runtime_config(symbols, series_ids),
    })
}
