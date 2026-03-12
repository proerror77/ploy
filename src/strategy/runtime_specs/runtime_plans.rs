use std::collections::HashSet;

use tracing::{info, warn};

use crate::config::AppConfig;
use crate::strategy::CryptoTradingConfig;
use crate::{AgentRiskParams, Domain};

use super::deployment_matrix::{
    coin_symbol_for, crypto_series_id_for, symbol_for_crypto_series_id,
    RuntimeCryptoStrategyTargets,
};
#[cfg(feature = "rl")]
use super::runtime_configs::build_crypto_rl_policy_runtime_config;
use super::runtime_configs::{
    build_crypto_lob_ml_runtime_config, build_event_edge_runtime_config,
    build_momentum_runtime_config, build_nba_comeback_runtime_config,
    build_pattern_memory_runtime_config, build_pm_5m_directional_runtime_config,
    build_split_arb_runtime_config,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedRuntimeDataPlaneKind {
    ManagedCrypto,
    SharedCrypto,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedRuntimeBootstrapStep {
    None,
    EnsurePatternMemoryTable,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedRuntimeSpec {
    pub(crate) strategy_label: &'static str,
    pub(crate) agent_id: String,
    pub(crate) domain: Domain,
    pub(crate) risk_params: AgentRiskParams,
    pub(crate) strategy_config_toml: String,
    pub(crate) data_plane: ManagedRuntimeDataPlaneKind,
    pub(crate) bootstrap_step: ManagedRuntimeBootstrapStep,
}

fn pattern_memory_runtime_coins(
    crypto_cfg: &CryptoTradingConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
) -> Vec<String> {
    let mut coins: Vec<String> = if runtime_crypto_targets.pattern_memory_coins.is_empty() {
        crypto_cfg.coins.clone()
    } else {
        runtime_crypto_targets
            .pattern_memory_coins
            .iter()
            .cloned()
            .collect()
    };
    coins.sort();
    coins.dedup();
    coins
}

fn pm_5m_directional_runtime_coins(
    crypto_cfg: &CryptoTradingConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
) -> Vec<String> {
    let mut coins: Vec<String> = if runtime_crypto_targets.pm_5m_directional_coins.is_empty() {
        crypto_cfg.coins.clone()
    } else {
        runtime_crypto_targets
            .pm_5m_directional_coins
            .iter()
            .cloned()
            .collect()
    };
    coins.sort();
    coins.dedup();
    coins
}

fn split_arb_runtime_symbols_and_series(
    crypto_cfg: &CryptoTradingConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
) -> Option<(Vec<String>, Vec<String>)> {
    let mut coins: Vec<String> = if runtime_crypto_targets.split_arb_coins.is_empty() {
        crypto_cfg.coins.clone()
    } else {
        runtime_crypto_targets
            .split_arb_coins
            .iter()
            .cloned()
            .collect()
    };
    coins.sort();
    coins.dedup();

    let mut horizons: Vec<String> = if runtime_crypto_targets.split_arb_horizons.is_empty() {
        vec!["5m".to_string(), "15m".to_string()]
    } else {
        runtime_crypto_targets
            .split_arb_horizons
            .iter()
            .cloned()
            .collect()
    };
    horizons.sort();
    horizons.dedup();

    let mut series_set: HashSet<String> = HashSet::new();
    for coin in &coins {
        let normalized = coin.trim_end_matches("USDT");
        for horizon in &horizons {
            if let Some(series_id) = crypto_series_id_for(normalized, horizon) {
                series_set.insert(series_id.to_string());
            }
        }
    }
    let mut series_ids: Vec<String> = series_set.into_iter().collect();
    series_ids.sort();
    if series_ids.is_empty() {
        return None;
    }

    let mut symbols: Vec<String> = coins
        .iter()
        .filter_map(|coin| {
            let normalized = coin.trim_end_matches("USDT");
            coin_symbol_for(normalized)
        })
        .collect();
    symbols.sort();
    symbols.dedup();
    if symbols.is_empty() {
        symbols = series_ids
            .iter()
            .filter_map(|series_id| symbol_for_crypto_series_id(series_id).map(str::to_string))
            .collect();
        symbols.sort();
        symbols.dedup();
    }

    Some((symbols, series_ids))
}

pub(crate) fn collect_managed_runtime_specs(
    config: &crate::coordinator::bootstrap::PlatformBootstrapConfig,
    app_config: &AppConfig,
    runtime_crypto_targets: &RuntimeCryptoStrategyTargets,
) -> Vec<ManagedRuntimeSpec> {
    let mut specs = Vec::new();

    if config.enable_crypto {
        let crypto_cfg = config.crypto.clone();
        if config.enable_crypto_momentum {
            match build_momentum_runtime_config(&crypto_cfg) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "momentum",
                    agent_id: crypto_cfg.agent_id.clone(),
                    domain: Domain::Crypto,
                    risk_params: crypto_cfg.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::ManagedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                }),
                Err(e) => warn!(
                    agent = crypto_cfg.agent_id,
                    error = %e,
                    entry_mode = ?crypto_cfg.entry_mode,
                    "momentum runtime config unavailable; skipping managed momentum startup"
                ),
            }
        } else {
            info!(
                agent = crypto_cfg.agent_id,
                "crypto momentum agent disabled"
            );
        }

        if config.enable_crypto_pm_5m_directional {
            let coins = pm_5m_directional_runtime_coins(&crypto_cfg, runtime_crypto_targets);
            match build_pm_5m_directional_runtime_config(&coins) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "pm_5m_directional",
                    agent_id: "pm_5m_directional".to_string(),
                    domain: Domain::Crypto,
                    risk_params: crypto_cfg.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::ManagedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                }),
                Err(e) => warn!(
                    agent = "pm_5m_directional",
                    error = %e,
                    "pm_5m_directional enabled but no valid runtime config could be built"
                ),
            }
        }

        if config.enable_crypto_pattern_memory {
            let coins = pattern_memory_runtime_coins(&crypto_cfg, runtime_crypto_targets);
            match build_pattern_memory_runtime_config(&coins) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "pattern_memory",
                    agent_id: "pattern_memory".to_string(),
                    domain: Domain::Crypto,
                    risk_params: crypto_cfg.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::ManagedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::EnsurePatternMemoryTable,
                }),
                Err(e) => warn!(
                    agent = "pattern_memory",
                    error = %e,
                    "pattern_memory enabled but no valid runtime config could be built"
                ),
            }
        }

        if config.enable_crypto_split_arb {
            if let Some((symbols, series_ids)) =
                split_arb_runtime_symbols_and_series(&crypto_cfg, runtime_crypto_targets)
            {
                specs.push(ManagedRuntimeSpec {
                    strategy_label: "staggered_arb",
                    agent_id: "staggered_arb".to_string(),
                    domain: Domain::Crypto,
                    risk_params: crypto_cfg.risk_params.clone(),
                    strategy_config_toml: build_split_arb_runtime_config(&symbols, &series_ids),
                    data_plane: ManagedRuntimeDataPlaneKind::SharedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                });
            } else {
                warn!(
                    agent = "staggered_arb",
                    "staggered_arb enabled but no recognized coin/horizon series ids were resolved"
                );
            }
        }

        if config.managed_crypto.enable_lob_ml {
            let lob_cfg = config.managed_crypto.lob_ml.clone();
            match build_crypto_lob_ml_runtime_config(&lob_cfg) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "crypto_lob_ml",
                    agent_id: format!("{}_strategy", lob_cfg.agent_id),
                    domain: Domain::Crypto,
                    risk_params: lob_cfg.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::ManagedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                }),
                Err(e) => warn!(
                    agent = lob_cfg.agent_id,
                    error = %e,
                    "crypto_lob_ml canonical runtime config unavailable; skipping managed wrapper startup"
                ),
            }
        }

        #[cfg(feature = "rl")]
        if config.managed_crypto.enable_rl_policy {
            let rl_cfg = config.managed_crypto.rl_policy.clone();
            match build_crypto_rl_policy_runtime_config(&rl_cfg) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "crypto_rl_policy",
                    agent_id: format!("{}_strategy", rl_cfg.agent_id),
                    domain: Domain::Crypto,
                    risk_params: rl_cfg.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::ManagedCrypto,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                }),
                Err(e) => warn!(
                    agent = rl_cfg.agent_id,
                    error = %e,
                    "crypto_rl_policy canonical runtime config unavailable; skipping managed wrapper startup"
                ),
            }
        }
    }

    if config.enable_sports {
        if let Some(nba_cfg) = app_config.nba_comeback.as_ref() {
            specs.push(ManagedRuntimeSpec {
                strategy_label: "nba_comeback",
                agent_id: config.sports.agent_id.clone(),
                domain: Domain::Sports,
                risk_params: config.sports.risk_params.clone(),
                strategy_config_toml: build_nba_comeback_runtime_config(
                    nba_cfg,
                    &app_config.database.url,
                ),
                data_plane: ManagedRuntimeDataPlaneKind::None,
                bootstrap_step: ManagedRuntimeBootstrapStep::None,
            });
        } else {
            warn!(
                agent = config.sports.agent_id,
                "sports runtime enabled but nba_comeback config missing; skipping canonical managed runtime"
            );
        }
    }

    if config.enable_politics {
        if let Some(event_edge_cfg) = app_config.event_edge_agent.as_ref() {
            match build_event_edge_runtime_config(event_edge_cfg) {
                Ok(strategy_config_toml) => specs.push(ManagedRuntimeSpec {
                    strategy_label: "event_edge",
                    agent_id: config.politics.agent_id.clone(),
                    domain: Domain::Politics,
                    risk_params: config.politics.risk_params.clone(),
                    strategy_config_toml,
                    data_plane: ManagedRuntimeDataPlaneKind::None,
                    bootstrap_step: ManagedRuntimeBootstrapStep::None,
                }),
                Err(e) => warn!(
                    agent = config.politics.agent_id,
                    error = %e,
                    "event_edge canonical runtime config unavailable; skipping managed wrapper startup"
                ),
            }
        } else {
            warn!(
                agent = config.politics.agent_id,
                "politics runtime enabled but event_edge config missing; skipping canonical managed runtime"
            );
        }
    }

    specs
}
