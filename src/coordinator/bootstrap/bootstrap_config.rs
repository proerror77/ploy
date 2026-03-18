use tracing::info;

use crate::config::AppConfig;
use crate::coordinator::CoordinatorConfig;
use crate::strategy::CryptoTradingConfig;

mod coordinator_env;
mod crypto_env;

use super::managed_crypto::{apply_managed_crypto_runtime_env, ManagedCryptoRuntimeConfig};
use super::runtime_config::{PoliticsRuntimeConfig, SportsRuntimeConfig};
use super::strategy_deployments::apply_strategy_deployments;
use super::support::load_strategy_deployments;
use super::OpenClawConfig;
use coordinator_env::apply_coordinator_runtime_env;
use crypto_env::apply_crypto_runtime_env;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlatformBootstrapConfig {
    pub coordinator: CoordinatorConfig,
    pub enable_crypto: bool,
    #[serde(default)]
    pub enable_crypto_momentum: bool,
    #[serde(default)]
    pub enable_crypto_pattern_memory: bool,
    #[serde(default)]
    pub enable_crypto_split_arb: bool,
    #[serde(default)]
    pub enable_crypto_pm_5m_directional: bool,
    #[serde(default)]
    pub enable_crypto_lob_ml: bool,
    #[serde(default)]
    pub enable_crypto_rl_policy: bool,
    pub enable_sports: bool,
    pub enable_politics: bool,
    #[serde(default)]
    pub enable_economics: bool,
    /// Enable OpenClaw meta-agent (Layer 3 orchestrator)
    #[serde(default)]
    pub enable_openclaw: bool,
    pub dry_run: bool,
    pub crypto: CryptoTradingConfig,
    #[serde(default, alias = "legacy_crypto")]
    pub managed_crypto: ManagedCryptoRuntimeConfig,
    pub sports: SportsRuntimeConfig,
    pub politics: PoliticsRuntimeConfig,
    /// OpenClaw meta-agent configuration
    #[serde(default)]
    pub openclaw: OpenClawConfig,
}

impl Default for PlatformBootstrapConfig {
    fn default() -> Self {
        Self {
            coordinator: CoordinatorConfig::default(),
            enable_crypto: true,
            enable_crypto_momentum: true,
            enable_crypto_pattern_memory: false,
            enable_crypto_split_arb: false,
            enable_crypto_pm_5m_directional: false,
            enable_crypto_lob_ml: false,
            enable_crypto_rl_policy: false,
            enable_sports: false,
            enable_politics: false,
            enable_economics: false,
            enable_openclaw: false,
            dry_run: true,
            crypto: CryptoTradingConfig::default(),
            managed_crypto: ManagedCryptoRuntimeConfig::default(),
            sports: SportsRuntimeConfig::default(),
            politics: PoliticsRuntimeConfig::default(),
            openclaw: OpenClawConfig::default(),
        }
    }
}

impl PlatformBootstrapConfig {
    /// Re-evaluate deployment matrix against the current runtime account + dry-run mode.
    pub fn reapply_strategy_deployments_for_runtime(&mut self, app: &AppConfig) {
        let strategy_deployments = load_strategy_deployments();
        if strategy_deployments.is_empty() {
            return;
        }

        let runtime_account_id = if app.account.id.trim().is_empty() {
            "default".to_string()
        } else {
            app.account.id.clone()
        };
        apply_strategy_deployments(
            self,
            &strategy_deployments,
            &runtime_account_id,
            self.dry_run,
        );
    }

    /// Build from AppConfig, enabling agents based on their config sections
    pub fn from_app_config(app: &AppConfig) -> Self {
        let mut cfg = Self::default();
        cfg.dry_run = app.dry_run.enabled;
        cfg.sports.account_id = app.account.id.clone();
        apply_coordinator_runtime_env(&mut cfg, app);
        apply_crypto_runtime_env(&mut cfg, app);

        apply_managed_crypto_runtime_env(&cfg.crypto, &mut cfg.managed_crypto);

        if let Some(ref nba) = app.nba_comeback {
            if nba.enabled {
                cfg.enable_sports = true;
                cfg.sports.poll_interval_secs = nba.espn_poll_interval_secs;
            }
        }

        if let Some(ref ee) = app.event_edge_agent {
            if ee.enabled {
                cfg.enable_politics = true;
            }
        }

        cfg.reapply_strategy_deployments_for_runtime(app);

        if app.openclaw_runtime_lockdown() {
            cfg.enable_crypto = false;
            cfg.enable_crypto_momentum = false;
            cfg.enable_crypto_pattern_memory = false;
            cfg.enable_crypto_split_arb = false;
            cfg.enable_crypto_pm_5m_directional = false;
            cfg.enable_crypto_lob_ml = false;
            cfg.enable_crypto_rl_policy = false;
            cfg.managed_crypto.enable_lob_ml = false;
            #[cfg(feature = "rl")]
            {
                cfg.managed_crypto.enable_rl_policy = false;
            }
            cfg.enable_sports = false;
            cfg.enable_politics = false;
            cfg.enable_economics = false;
            info!(
                "agent framework lockdown active (mode=openclaw): built-in managed/legacy runtime loops are disabled"
            );
        }

        cfg
    }
}
