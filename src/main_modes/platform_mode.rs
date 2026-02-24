use ploy::cli::runtime::Cli;
use ploy::config::AppConfig;
use ploy::coordinator::bootstrap::{start_platform, PlatformBootstrapConfig, PlatformStartControl};
use ploy::error::{PloyError, Result};
use tracing::{info, warn};

fn build_platform_config_for_runtime(
    app_config: &AppConfig,
    crypto: bool,
    sports: bool,
    dry_run: bool,
) -> PlatformBootstrapConfig {
    let mut platform_cfg = PlatformBootstrapConfig::from_app_config(app_config);

    if dry_run {
        platform_cfg.dry_run = true;
    }

    // Always reapply deployment matrix after runtime overrides so execution_mode/account scope
    // is evaluated against the effective runtime (e.g., CLI --dry-run).
    platform_cfg.reapply_strategy_deployments_for_runtime(app_config);

    // Explicit domain flags work as a filter, not an override.
    // This prevents bypassing deployment matrix enable/disable controls.
    let explicit_selection = crypto || sports;
    if explicit_selection {
        if !crypto {
            platform_cfg.enable_crypto = false;
            platform_cfg.enable_crypto_momentum = false;
            platform_cfg.enable_crypto_lob_ml = false;
            platform_cfg.enable_crypto_pattern_memory = false;
            platform_cfg.enable_crypto_split_arb = false;
            #[cfg(feature = "rl")]
            {
                platform_cfg.enable_crypto_rl_policy = false;
            }
        }
        if !sports {
            platform_cfg.enable_sports = false;
        }
    }

    // Runtime scope is intentionally limited to crypto + sports agents.
    platform_cfg.enable_politics = false;

    if app_config.openclaw_runtime_lockdown() {
        platform_cfg.enable_crypto = false;
        platform_cfg.enable_crypto_momentum = false;
        platform_cfg.enable_crypto_lob_ml = false;
        platform_cfg.enable_crypto_pattern_memory = false;
        platform_cfg.enable_crypto_split_arb = false;
        #[cfg(feature = "rl")]
        {
            platform_cfg.enable_crypto_rl_policy = false;
        }
        platform_cfg.enable_sports = false;
        platform_cfg.enable_politics = false;
        warn!("platform started in openclaw lockdown mode; built-in agents forced off");
    }

    platform_cfg
}

pub async fn run_platform_mode(
    action: &str,
    crypto: bool,
    sports: bool,
    dry_run: bool,
    pause: Option<String>,
    resume: Option<String>,
    cli: &Cli,
) -> Result<()> {
    let app_config = AppConfig::load_from(&cli.config).unwrap_or_else(|e| {
        warn!("Failed to load config: {}, using defaults", e);
        AppConfig::default_config(true, "btc-price-series-15m")
    });

    if action != "start" {
        return Err(PloyError::Validation(format!(
            "unsupported platform action '{}'; only 'start' is supported",
            action
        )));
    }

    let platform_cfg = build_platform_config_for_runtime(&app_config, crypto, sports, dry_run);

    info!(
        "Platform mode: crypto={} sports={} dry_run={}",
        platform_cfg.enable_crypto, platform_cfg.enable_sports, platform_cfg.dry_run,
    );

    let control = PlatformStartControl { pause, resume };
    start_platform(platform_cfg, &app_config, control).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default)]
    struct EnvOverride {
        previous: Vec<(String, Option<String>)>,
    }

    impl EnvOverride {
        fn set(&mut self, key: &str, value: &str) {
            if !self.previous.iter().any(|(existing, _)| existing == key) {
                self.previous.push((key.to_string(), env::var(key).ok()));
            }
            unsafe {
                env::set_var(key, value);
            }
        }

        fn remove(&mut self, key: &str) {
            if !self.previous.iter().any(|(existing, _)| existing == key) {
                self.previous.push((key.to_string(), env::var(key).ok()));
            }
            unsafe {
                env::remove_var(key);
            }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            for (key, value) in self.previous.iter().rev() {
                if let Some(value) = value {
                    unsafe {
                        env::set_var(key, value);
                    }
                } else {
                    unsafe {
                        env::remove_var(key);
                    }
                }
            }
        }
    }

    #[test]
    fn runtime_dry_run_override_reapplies_deployment_execution_mode() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-crypto-dryrun-only",
                "strategy":"momentum",
                "domain":"Crypto",
                "market_selector":{"mode":"dynamic","domain":"Crypto","query":"BTC"},
                "timeframe":"5m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":90,
                "cooldown_secs":60,
                "execution_mode":"dry_run_only"
              }
            ]"#,
        );

        let app = AppConfig::default_config(false, "btc-price-series-15m");

        let live_cfg = build_platform_config_for_runtime(&app, false, false, false);
        assert!(
            !live_cfg.enable_crypto,
            "dry_run_only deployment should not enable crypto in live runtime"
        );

        let dry_cfg = build_platform_config_for_runtime(&app, false, false, true);
        assert!(
            dry_cfg.dry_run,
            "runtime dry-run override should be applied"
        );
        assert!(
            dry_cfg.enable_crypto,
            "dry_run_only deployment should enable crypto in dry-run runtime"
        );
    }

    #[test]
    fn explicit_domain_flags_filter_deployments_instead_of_overriding() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-sports-only",
                "strategy":"sports",
                "domain":"Sports",
                "market_selector":{"mode":"dynamic","domain":"Sports","query":"NBA"},
                "timeframe":"15m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":80,
                "cooldown_secs":120
              }
            ]"#,
        );

        let app = AppConfig::default_config(false, "btc-price-series-15m");
        let cfg = build_platform_config_for_runtime(&app, true, false, false);

        assert!(
            !cfg.enable_crypto,
            "explicit --crypto must not bypass deployment matrix if crypto has no enabled deployment"
        );
        assert!(
            !cfg.enable_sports,
            "explicit selection should filter out unselected domains"
        );
    }

    #[test]
    fn pattern_memory_deployment_does_not_enable_lob_ml() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-pattern-memory-5m",
                "strategy":"pattern_memory",
                "domain":"Crypto",
                "market_selector":{"mode":"dynamic","domain":"Crypto","query":"BTC 5m"},
                "timeframe":"5m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":90,
                "cooldown_secs":60
              }
            ]"#,
        );

        let app = AppConfig::default_config(true, "btc-price-series-15m");
        let cfg = build_platform_config_for_runtime(&app, true, false, true);

        assert!(cfg.enable_crypto);
        assert!(cfg.enable_crypto_pattern_memory);
        assert!(
            !cfg.enable_crypto_lob_ml,
            "pattern_memory deployment should not auto-route to crypto_lob_ml"
        );
    }

    #[test]
    fn split_arb_deployment_does_not_enable_momentum() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-split-arb-15m",
                "strategy":"split_arb",
                "domain":"Crypto",
                "market_selector":{"mode":"dynamic","domain":"Crypto","query":"ETH 15m"},
                "timeframe":"15m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":80,
                "cooldown_secs":120
              }
            ]"#,
        );

        let app = AppConfig::default_config(true, "btc-price-series-15m");
        let cfg = build_platform_config_for_runtime(&app, true, false, true);

        assert!(cfg.enable_crypto);
        assert!(cfg.enable_crypto_split_arb);
        assert!(
            !cfg.enable_crypto_momentum,
            "split_arb deployment should not auto-route to momentum"
        );
    }

    #[test]
    fn runtime_scope_disables_politics_even_if_deployment_enables_it() {
        let _guard = env_lock().lock().expect("failed to lock env");
        let mut env_override = EnvOverride::default();
        env_override.remove("PLOY_DEPLOYMENTS_JSON");
        env_override.set(
            "PLOY_STRATEGY_DEPLOYMENTS_JSON",
            r#"[
              {
                "id":"dep-politics",
                "strategy":"event_edge",
                "domain":"Politics",
                "market_selector":{"mode":"dynamic","domain":"Politics","query":"Election"},
                "timeframe":"15m",
                "enabled":true,
                "allocator_profile":"default",
                "risk_profile":"default",
                "priority":70,
                "cooldown_secs":120
              }
            ]"#,
        );

        let app = AppConfig::default_config(true, "btc-price-series-15m");
        let cfg = build_platform_config_for_runtime(&app, false, false, true);

        assert!(
            !cfg.enable_politics,
            "platform runtime should keep politics disabled in crypto+sports-only scope"
        );
    }
}
