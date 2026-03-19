use super::PatternMemoryStrategy;
use crate::error::{PloyError, Result};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub(super) struct MarketMapping {
    pub(super) symbol: String,
    pub(super) series_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct TimingConfig {
    pub(super) target_remaining_secs: i64,
    pub(super) tolerance_secs: i64,
    pub(super) min_remaining_secs: i64,
}

#[derive(Debug, Clone)]
pub(super) struct Filter15mConfig {
    pub(super) enabled: bool,
    pub(super) min_confidence: f64,
    pub(super) min_n_eff: f64,
}

#[derive(Debug, Clone)]
pub(super) struct PatternConfig {
    pub(super) corr_threshold: f64,
    pub(super) alpha: f64,
    pub(super) beta: f64,
    pub(super) min_matches: usize,
    pub(super) min_n_eff: f64,
    pub(super) min_confidence: f64,
    pub(super) age_decay_lambda: f64,
    pub(super) max_samples: usize,
}

#[derive(Debug, Clone)]
pub(super) struct TradeConfig {
    pub(super) shares: u64,
    pub(super) max_entry_price: Decimal,
    pub(super) min_net_ev: Decimal,
    pub(super) cooldown_secs: i64,
}

#[derive(Debug, Clone)]
pub(super) struct Config {
    pub(super) markets: Vec<MarketMapping>,
    pub(super) timing: TimingConfig,
    pub(super) pattern: PatternConfig,
    pub(super) filter_15m: Filter15mConfig,
    pub(super) trade: TradeConfig,
}

impl PatternMemoryStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value = toml::from_str(config_str)
            .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;

        let markets = config
            .get("markets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| PloyError::Internal("Missing [[markets]]".to_string()))?;

        let mut parsed_markets: Vec<MarketMapping> = Vec::new();
        for m in markets {
            let symbol = m
                .get("symbol")
                .and_then(|v| v.as_str())
                .ok_or_else(|| PloyError::Internal("markets.symbol missing".to_string()))?;
            let series_id = m
                .get("series_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| PloyError::Internal("markets.series_id missing".to_string()))?;
            parsed_markets.push(MarketMapping {
                symbol: symbol.to_string(),
                series_id: series_id.to_string(),
            });
        }

        let empty = Value::Table(Default::default());
        let pattern = config.get("pattern").unwrap_or(&empty);
        let filter_15m = config.get("filter_15m").unwrap_or(&empty);
        let trade = config.get("trade").unwrap_or(&empty);
        let timing = config.get("timing").unwrap_or(&empty);

        let cfg = Config {
            markets: parsed_markets,
            timing: TimingConfig {
                target_remaining_secs: timing
                    .get("target_remaining_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(300) as i64,
                tolerance_secs: timing
                    .get("tolerance_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(45) as i64,
                min_remaining_secs: timing
                    .get("min_remaining_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(60) as i64,
            },
            pattern: PatternConfig {
                corr_threshold: pattern
                    .get("corr_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.70),
                alpha: pattern
                    .get("alpha")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                beta: pattern
                    .get("beta")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                min_matches: pattern
                    .get("min_matches")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(10) as usize,
                min_n_eff: pattern
                    .get("min_n_eff")
                    .and_then(|v| v.as_float())
                    .unwrap_or(5.0),
                min_confidence: pattern
                    .get("min_confidence")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.60),
                age_decay_lambda: pattern
                    .get("age_decay_lambda")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.001),
                max_samples: pattern
                    .get("max_samples")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(2000) as usize,
            },
            filter_15m: Filter15mConfig {
                enabled: filter_15m
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                min_confidence: filter_15m
                    .get("min_confidence")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.55),
                min_n_eff: filter_15m
                    .get("min_n_eff")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.0),
            },
            trade: TradeConfig {
                shares: trade
                    .get("shares")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(100) as u64,
                max_entry_price: Decimal::from_f64(
                    trade
                        .get("max_entry_price")
                        .and_then(|v| v.as_float())
                        .unwrap_or(0.55),
                )
                .unwrap_or(dec!(0.55)),
                min_net_ev: Decimal::from_f64(
                    trade
                        .get("min_net_ev")
                        .and_then(|v| v.as_float())
                        .unwrap_or(0.0),
                )
                .unwrap_or(Decimal::ZERO),
                cooldown_secs: trade
                    .get("cooldown_secs")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(30) as i64,
            },
        };

        let mut symbol_by_series = HashMap::new();
        let mut series_by_symbol = HashMap::new();
        for m in &cfg.markets {
            symbol_by_series.insert(m.series_id.clone(), m.symbol.clone());
            series_by_symbol.insert(m.symbol.clone(), m.series_id.clone());
        }

        Ok(Self {
            id,
            dry_run,
            cfg,
            enabled: true,
            symbol_by_series,
            series_by_symbol,
            mem_5m: HashMap::new(),
            mem_15m: HashMap::new(),
            quotes: HashMap::new(),
            events: HashMap::new(),
            traded_events: HashSet::new(),
            cooldowns: HashMap::new(),
            last_decision: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::traits::{DataFeed, Strategy};

    fn pattern_memory_test_config() -> &'static str {
        r#"
[[markets]]
symbol = "BTC"
series_id = "series-btc"

[pattern]
min_matches = 0
min_n_eff = 0.0
min_confidence = 0.0

[filter_15m]
enabled = false

[trade]
shares = 42
max_entry_price = 0.55
min_net_ev = 0.0
"#
    }

    #[test]
    fn from_toml_builds_expected_feeds() {
        let strategy = PatternMemoryStrategy::from_toml(
            "pattern-memory-test".to_string(),
            pattern_memory_test_config(),
            true,
        )
        .expect("strategy config should parse");

        let feeds = strategy.required_feeds();
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::BinanceKlines { symbols, intervals, closed_only }
                if symbols.as_slice() == ["BTC".to_string()]
                    && intervals.as_slice() == ["5m".to_string(), "15m".to_string()]
                    && *closed_only
        )));
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids }
                if series_ids.as_slice() == ["series-btc".to_string()]
        )));
    }

    #[test]
    fn from_toml_preserves_pattern_runtime_settings() {
        let strategy = PatternMemoryStrategy::from_toml(
            "pattern-memory-test".to_string(),
            pattern_memory_test_config(),
            true,
        )
        .expect("strategy config should parse");

        assert_eq!(strategy.cfg.trade.shares, 42);
        assert_eq!(strategy.cfg.trade.max_entry_price, dec!(0.55));
        assert_eq!(strategy.cfg.pattern.max_samples, 2000);
        assert!(!strategy.cfg.filter_15m.enabled);
        assert_eq!(strategy.cfg.timing.target_remaining_secs, 300);
        assert_eq!(strategy.mem_5m.len(), 0);
        assert_eq!(strategy.mem_15m.len(), 0);
        assert_eq!(strategy.events.len(), 0);
        assert_eq!(strategy.last_decision.len(), 0);
    }
}
