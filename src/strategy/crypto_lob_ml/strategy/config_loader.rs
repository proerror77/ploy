use super::{
    core, CryptoLobMlStrategy, LobMlInferenceSummary, LobMlL2Snapshot, LobMlTrackedEvent,
    PloyError, Result, STRATEGY_NAME,
};
use crate::adapters::SpotPrice;
use crate::domain::Quote;
use crate::strategy::crypto::{known_binance_symbols, series_ids_for_symbol};
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(super) struct CryptoLobMlStrategyConfig {
    pub(crate) coins: Vec<String>,
    pub(crate) min_time_remaining_secs: u64,
    pub(crate) max_time_remaining_secs: u64,
    pub(crate) max_time_remaining_secs_5m: u64,
    pub(crate) max_time_remaining_secs_15m: u64,
    pub(crate) require_price_to_beat: bool,
    pub(crate) max_lob_snapshot_age_secs: u64,
    pub(crate) feature_offsets: Vec<f32>,
    pub(crate) feature_scales: Vec<f32>,
    pub(crate) oracle_lag_buffer_secs: u64,
    pub(crate) tick_interval_ms: u64,
}

impl Default for CryptoLobMlStrategyConfig {
    fn default() -> Self {
        Self {
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            max_time_remaining_secs_5m: 120,
            max_time_remaining_secs_15m: 240,
            require_price_to_beat: true,
            max_lob_snapshot_age_secs: 2,
            feature_offsets: Vec::new(),
            feature_scales: Vec::new(),
            oracle_lag_buffer_secs: 3,
            tick_interval_ms: 1000,
        }
    }
}

impl CryptoLobMlStrategyConfig {
    fn normalize(&mut self) {
        if self.coins.is_empty() {
            self.coins = vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()];
        }
        if self.tick_interval_ms == 0 {
            self.tick_interval_ms = 1000;
        }
        if self.min_time_remaining_secs == 0 {
            self.min_time_remaining_secs = 60;
        }
        self.max_time_remaining_secs = self
            .max_time_remaining_secs
            .max(self.min_time_remaining_secs);
        self.max_time_remaining_secs_5m = self
            .max_time_remaining_secs_5m
            .max(self.min_time_remaining_secs)
            .min(self.max_time_remaining_secs);
        self.max_time_remaining_secs_15m = self
            .max_time_remaining_secs_15m
            .max(self.min_time_remaining_secs)
            .min(self.max_time_remaining_secs);
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
struct CryptoLobMlStrategyToml {
    strategy: StrategySection,
    #[serde(default)]
    crypto_lob_ml: CryptoLobMlStrategyConfig,
}

impl CryptoLobMlStrategy {
    pub fn from_toml(id: String, config_str: &str, _dry_run: bool) -> Result<Self> {
        let parsed: CryptoLobMlStrategyToml = toml::from_str(config_str)
            .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;
        if parsed.strategy.name != STRATEGY_NAME {
            return Err(PloyError::Validation(format!(
                "strategy.name must be \"{STRATEGY_NAME}\", got \"{}\"",
                parsed.strategy.name
            )));
        }

        let mut cfg = parsed.crypto_lob_ml;
        cfg.normalize();
        let symbols = cfg.configured_symbols();
        if symbols.is_empty() {
            return Err(PloyError::Validation(
                "crypto_lob_ml requires at least one supported coin".to_string(),
            ));
        }
        let series_ids = cfg.configured_series_ids();
        if series_ids.is_empty() {
            return Err(PloyError::Validation(
                "crypto_lob_ml requires at least one supported crypto series".to_string(),
            ));
        }

        Ok(Self {
            id,
            enabled: parsed.strategy.enabled.unwrap_or(true),
            cfg,
            symbols,
            series_ids,
            spot_prices: HashMap::<String, SpotPrice>::new(),
            l2_by_symbol: HashMap::<String, LobMlL2Snapshot>::new(),
            quotes: HashMap::<String, Quote>::new(),
            active_events: HashMap::<String, LobMlTrackedEvent>::new(),
            sequence_cache: HashMap::<String, VecDeque<core::SequenceSnapshot>>::new(),
            last_inference: None::<LobMlInferenceSummary>,
            last_reason: None,
            last_error: None,
            last_logged_at: HashMap::new(),
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::traits::DataFeed;
    use crate::strategy::traits::Strategy;

    fn minimal_toml() -> &'static str {
        r#"
[strategy]
name = "crypto_lob_ml"
enabled = true

[crypto_lob_ml]
coins = ["BTC"]
tick_interval_ms = 1000
min_time_remaining_secs = 60
max_time_remaining_secs = 300
max_time_remaining_secs_5m = 120
max_lob_snapshot_age_secs = 2
require_price_to_beat = true
"#
    }

    #[test]
    fn from_toml_builds_expected_feeds() {
        let strategy = CryptoLobMlStrategy::from_toml("lob-test".to_string(), minimal_toml(), true)
            .expect("strategy");
        let feeds = strategy.required_feeds();
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::BinanceSpot { symbols } if symbols == &vec!["BTCUSDT".to_string()]
        )));
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids } if series_ids == &vec!["10192".to_string(), "10684".to_string()]
        )));
    }
}
