use crate::error::{PloyError, Result};
use crate::strategy::crypto_lob_ml::CryptoLobMlConfig;
#[cfg(feature = "rl")]
use crate::strategy::crypto_rl_policy::CryptoRlPolicyConfig;

use super::{set_bool, set_decimal, set_integer, set_string, set_string_array};

pub(crate) fn build_crypto_lob_ml_runtime_config(cfg: &CryptoLobMlConfig) -> Result<String> {
    if cfg.coins.is_empty() {
        return Err(PloyError::Validation(
            "crypto_lob_ml runtime requires at least one configured coin".to_string(),
        ));
    }

    let mut root = toml::map::Map::new();
    let mut strategy = toml::map::Map::new();
    let mut lob_ml = toml::map::Map::new();

    let mut coins: Vec<String> = cfg
        .coins
        .iter()
        .map(|coin| coin.to_ascii_uppercase())
        .collect();
    coins.sort();
    coins.dedup();

    set_string(&mut strategy, "name", "crypto_lob_ml");
    set_bool(&mut strategy, "enabled", true);

    set_string_array(&mut lob_ml, "coins", &coins);
    set_integer(
        &mut lob_ml,
        "min_time_remaining_secs",
        i64::try_from(cfg.min_time_remaining_secs).unwrap_or(60),
    );
    set_integer(
        &mut lob_ml,
        "max_time_remaining_secs",
        i64::try_from(cfg.max_time_remaining_secs).unwrap_or(900),
    );
    set_integer(
        &mut lob_ml,
        "max_time_remaining_secs_5m",
        i64::try_from(cfg.max_time_remaining_secs_5m).unwrap_or(120),
    );
    set_integer(
        &mut lob_ml,
        "max_time_remaining_secs_15m",
        i64::try_from(cfg.max_time_remaining_secs_15m).unwrap_or(240),
    );
    set_bool(
        &mut lob_ml,
        "require_price_to_beat",
        cfg.require_price_to_beat,
    );
    set_integer(
        &mut lob_ml,
        "max_lob_snapshot_age_secs",
        i64::try_from(cfg.max_lob_snapshot_age_secs).unwrap_or(2),
    );
    set_integer(&mut lob_ml, "tick_interval_ms", 1000);

    root.insert("strategy".to_string(), toml::Value::Table(strategy));
    root.insert("crypto_lob_ml".to_string(), toml::Value::Table(lob_ml));
    toml::to_string_pretty(&toml::Value::Table(root)).map_err(|e| {
        PloyError::Internal(format!(
            "failed to render crypto_lob_ml runtime config: {e}"
        ))
    })
}

#[cfg(feature = "rl")]
pub(crate) fn build_crypto_rl_policy_runtime_config(cfg: &CryptoRlPolicyConfig) -> Result<String> {
    if cfg.coins.is_empty() {
        return Err(PloyError::Validation(
            "crypto_rl_policy runtime requires at least one configured coin".to_string(),
        ));
    }

    let mut root = toml::map::Map::new();
    let mut strategy = toml::map::Map::new();
    let mut rl_policy = toml::map::Map::new();

    let mut coins: Vec<String> = cfg
        .coins
        .iter()
        .map(|coin| coin.to_ascii_uppercase())
        .collect();
    coins.sort();
    coins.dedup();

    set_string(&mut strategy, "name", "crypto_rl_policy");
    set_bool(&mut strategy, "enabled", true);

    set_string_array(&mut rl_policy, "coins", &coins);
    set_integer(
        &mut rl_policy,
        "min_time_remaining_secs",
        i64::try_from(cfg.min_time_remaining_secs).unwrap_or(60),
    );
    set_integer(
        &mut rl_policy,
        "max_time_remaining_secs",
        i64::try_from(cfg.max_time_remaining_secs).unwrap_or(900),
    );
    set_integer(
        &mut rl_policy,
        "default_shares",
        i64::try_from(cfg.default_shares).unwrap_or(50),
    );
    set_decimal(&mut rl_policy, "max_entry_price", cfg.max_entry_price);
    set_integer(
        &mut rl_policy,
        "max_lob_snapshot_age_secs",
        i64::try_from(cfg.max_lob_snapshot_age_secs).unwrap_or(2),
    );
    set_integer(
        &mut rl_policy,
        "tick_interval_ms",
        i64::try_from(cfg.decision_interval_ms).unwrap_or(1000),
    );
    set_integer(
        &mut rl_policy,
        "observation_version",
        i64::from(cfg.observation_version),
    );
    set_string(&mut rl_policy, "policy_output", cfg.policy_output.clone());
    if let Some(path) = cfg.policy_model_path.as_deref() {
        if !path.trim().is_empty() {
            set_string(&mut rl_policy, "policy_model_path", path.to_string());
        }
    }
    if let Some(version) = cfg.policy_model_version.as_deref() {
        if !version.trim().is_empty() {
            set_string(&mut rl_policy, "policy_model_version", version.to_string());
        }
    }

    root.insert("strategy".to_string(), toml::Value::Table(strategy));
    root.insert(
        "crypto_rl_policy".to_string(),
        toml::Value::Table(rl_policy),
    );
    toml::to_string_pretty(&toml::Value::Table(root)).map_err(|e| {
        PloyError::Internal(format!(
            "failed to render crypto_rl_policy runtime config: {e}"
        ))
    })
}
