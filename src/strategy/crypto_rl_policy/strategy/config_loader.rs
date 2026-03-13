use super::{
    CryptoRlPolicyStrategy, PloyError, Result, STRATEGY_NAME, default_max_entry_price,
    default_observation_version, default_policy_output,
};
#[cfg(feature = "onnx")]
use super::load_policy_model;
use crate::adapters::SpotPrice;
use crate::collector::LobSnapshot;
use crate::domain::Quote;
use crate::strategy::crypto::{known_binance_symbols, series_ids_for_symbol};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(super) struct CryptoRlPolicyStrategyConfig {
    pub(super) coins: Vec<String>,
    pub(super) min_time_remaining_secs: u64,
    pub(super) max_time_remaining_secs: u64,
    pub(super) default_shares: u64,
    pub(super) max_entry_price: Decimal,
    pub(super) max_lob_snapshot_age_secs: u64,
    pub(super) tick_interval_ms: u64,
    #[serde(default = "default_observation_version")]
    pub(super) observation_version: u32,
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    #[serde(default)]
    pub(super) policy_model_path: Option<String>,
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    #[serde(default = "default_policy_output")]
    pub(super) policy_output: String,
    #[serde(default)]
    pub(super) policy_model_version: Option<String>,
}

impl Default for CryptoRlPolicyStrategyConfig {
    fn default() -> Self {
        Self {
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            default_shares: 50,
            max_entry_price: default_max_entry_price(),
            max_lob_snapshot_age_secs: 2,
            tick_interval_ms: 1000,
            observation_version: default_observation_version(),
            policy_model_path: None,
            policy_output: default_policy_output(),
            policy_model_version: None,
        }
    }
}

impl CryptoRlPolicyStrategyConfig {
    fn normalize(&mut self) {
        if self.coins.is_empty() {
            self.coins = vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()];
        }
        if self.min_time_remaining_secs == 0 {
            self.min_time_remaining_secs = 60;
        }
        self.max_time_remaining_secs = self
            .max_time_remaining_secs
            .max(self.min_time_remaining_secs);
        if self.default_shares == 0 {
            self.default_shares = 50;
        }
        if self.tick_interval_ms == 0 {
            self.tick_interval_ms = 1000;
        }
        self.observation_version = match self.observation_version {
            1 => 1,
            _ => 2,
        };
    }

    fn configured_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self
            .coins
            .iter()
            .filter_map(|coin| normalize_symbol(coin))
            .collect();
        if symbols.is_empty() {
            symbols = known_binance_symbols()
                .iter()
                .map(|symbol| (*symbol).to_string())
                .collect();
        }
        symbols.sort();
        symbols.dedup();
        symbols
    }

    fn configured_series_ids(&self) -> Vec<String> {
        let mut series_ids = Vec::new();
        for symbol in self.configured_symbols() {
            series_ids.extend(series_ids_for_symbol(&symbol));
        }
        series_ids.sort();
        series_ids.dedup();
        series_ids
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CryptoRlPolicyStrategyToml {
    strategy: StrategySection,
    #[serde(default)]
    crypto_rl_policy: CryptoRlPolicyStrategyConfig,
}

pub(super) fn build_strategy_from_toml(id: String, config_str: &str) -> Result<CryptoRlPolicyStrategy> {
    let parsed: CryptoRlPolicyStrategyToml = toml::from_str(config_str)
        .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;
    if parsed.strategy.name != STRATEGY_NAME {
        return Err(PloyError::Validation(format!(
            "strategy.name must be \"{STRATEGY_NAME}\", got \"{}\"",
            parsed.strategy.name
        )));
    }

    let mut cfg = parsed.crypto_rl_policy;
    cfg.normalize();
    let symbols = cfg.configured_symbols();
    if symbols.is_empty() {
        return Err(PloyError::Validation(
            "crypto_rl_policy requires at least one supported coin".to_string(),
        ));
    }
    let series_ids = cfg.configured_series_ids();
    if series_ids.is_empty() {
        return Err(PloyError::Validation(
            "crypto_rl_policy requires at least one supported crypto series".to_string(),
        ));
    }

    #[cfg(feature = "onnx")]
    let policy_model = load_policy_model(&id, &mut cfg)?;

    Ok(CryptoRlPolicyStrategy {
        id,
        enabled: parsed.strategy.enabled.unwrap_or(true),
        cfg,
        symbols,
        series_ids,
        spot_prices: HashMap::<String, SpotPrice>::new(),
        l2_by_symbol: HashMap::<String, LobSnapshot>::new(),
        quotes: HashMap::<String, Quote>::new(),
        active_events: HashMap::new(),
        last_signal: None,
        last_reason: None,
        last_error: None,
        last_logged_at: HashMap::new(),
        #[cfg(feature = "onnx")]
        policy_model,
    })
}

fn normalize_symbol(input: &str) -> Option<String> {
    let raw = input.trim().to_ascii_uppercase();
    let symbol = if raw.ends_with("USDT") {
        raw
    } else {
        format!("{raw}USDT")
    };
    known_binance_symbols()
        .iter()
        .any(|candidate| *candidate == symbol)
        .then_some(symbol)
}
