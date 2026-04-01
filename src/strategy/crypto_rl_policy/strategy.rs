use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::collections::HashMap;

use crate::adapters::SpotPrice;
use crate::collector::LobSnapshot;
use crate::domain::Quote;
use crate::error::{PloyError, Result};
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::strategy::crypto::{known_binance_symbols, series_ids_for_symbol};
use crate::strategy::crypto_rl_policy::core;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};
#[path = "signal_flow.rs"]
mod signal_flow;
use self::signal_flow::{RlSignalSummary, RlTrackedEvent};

const STRATEGY_NAME: &str = "crypto_rl_policy";

fn default_policy_output() -> String {
    "continuous".to_string()
}

fn default_observation_version() -> u32 {
    2
}

fn default_max_entry_price() -> Decimal {
    dec!(0.70)
}

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct CryptoRlPolicyStrategyConfig {
    coins: Vec<String>,
    min_time_remaining_secs: u64,
    max_time_remaining_secs: u64,
    default_shares: u64,
    max_entry_price: Decimal,
    max_lob_snapshot_age_secs: u64,
    tick_interval_ms: u64,
    #[serde(default = "default_observation_version")]
    observation_version: u32,
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    #[serde(default)]
    policy_model_path: Option<String>,
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    #[serde(default = "default_policy_output")]
    policy_output: String,
    #[serde(default)]
    policy_model_version: Option<String>,
}

impl Default for CryptoRlPolicyStrategyConfig {
    fn default() -> Self {
        Self {
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into(), "DOGE".into(), "HYPE".into(), "BNB".into()],
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
pub struct CryptoRlPolicyStrategy {
    id: String,
    enabled: bool,
    cfg: CryptoRlPolicyStrategyConfig,
    symbols: Vec<String>,
    series_ids: Vec<String>,
    spot_prices: HashMap<String, SpotPrice>,
    l2_by_symbol: HashMap<String, LobSnapshot>,
    quotes: HashMap<String, Quote>,
    active_events: HashMap<String, RlTrackedEvent>,
    last_signal: Option<RlSignalSummary>,
    last_reason: Option<String>,
    last_error: Option<String>,
    last_logged_at: HashMap<String, DateTime<Utc>>,

    #[cfg(feature = "onnx")]
    policy_model: Option<OnnxModel>,
}

impl CryptoRlPolicyStrategy {
    pub fn from_toml(id: String, config_str: &str, _dry_run: bool) -> Result<Self> {
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

        Ok(Self {
            id,
            enabled: parsed.strategy.enabled.unwrap_or(true),
            cfg,
            symbols,
            series_ids,
            spot_prices: HashMap::new(),
            l2_by_symbol: HashMap::new(),
            quotes: HashMap::new(),
            active_events: HashMap::new(),
            last_signal: None,
            last_reason: None,
            last_error: None,
            last_logged_at: HashMap::new(),
            #[cfg(feature = "onnx")]
            policy_model,
        })
    }
}

#[cfg(feature = "onnx")]
fn load_policy_model(
    strategy_id: &str,
    cfg: &mut CryptoRlPolicyStrategyConfig,
) -> Result<Option<OnnxModel>> {
    use tracing::warn;

    let Some(path) = cfg.policy_model_path.as_deref() else {
        return Ok(None);
    };
    if path.trim().is_empty() {
        return Ok(None);
    }

    let primary_version = cfg.observation_version;
    let primary_dim = if primary_version == 1 {
        core::OBS_DIM_V1
    } else {
        core::OBS_DIM_V2
    };

    match OnnxModel::load_for_vec_input(path, primary_dim) {
        Ok(model) => Ok(Some(model)),
        Err(primary_error) => {
            let fallback_version = if primary_version == 1 { 2 } else { 1 };
            let fallback_dim = if fallback_version == 1 {
                core::OBS_DIM_V1
            } else {
                core::OBS_DIM_V2
            };

            match OnnxModel::load_for_vec_input(path, fallback_dim) {
                Ok(model) => {
                    cfg.observation_version = fallback_version;
                    Ok(Some(model))
                }
                Err(fallback_error) => {
                    warn!(
                        strategy = strategy_id,
                        model_path = %path,
                        error = %primary_error,
                        "failed to load crypto RL policy model (primary schema)"
                    );
                    warn!(
                        strategy = strategy_id,
                        model_path = %path,
                        error = %fallback_error,
                        "failed to load crypto RL policy model (fallback schema)"
                    );
                    Ok(None)
                }
            }
        }
    }
}

#[async_trait]
impl Strategy for CryptoRlPolicyStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Canonical no-submit crypto RL policy wrapper for runtime migration"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::BinanceSpot {
                symbols: self.symbols.clone(),
            },
            DataFeed::PolymarketEvents {
                series_ids: self.series_ids.clone(),
            },
            DataFeed::Tick {
                interval_ms: self.cfg.tick_interval_ms,
            },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => match self.spot_prices.get_mut(symbol) {
                Some(existing) => existing.update(*price, None, *timestamp),
                None => {
                    self.spot_prices
                        .insert(symbol.clone(), SpotPrice::new(*price, None, *timestamp));
                }
            },
            MarketUpdate::BinanceL2 {
                symbol,
                obi_1,
                obi_2,
                obi_3,
                obi_5,
                obi_10,
                obi_20,
                bid_volume_5,
                ask_volume_5,
                spread_bps,
                timestamp,
            } => {
                self.l2_by_symbol.insert(
                    symbol.clone(),
                    LobSnapshot {
                        timestamp: *timestamp,
                        symbol: symbol.clone(),
                        best_bid: Decimal::ZERO,
                        best_ask: Decimal::ZERO,
                        mid_price: Decimal::ZERO,
                        spread_bps: *spread_bps,
                        obi_1: *obi_1,
                        obi_2: *obi_2,
                        obi_3: *obi_3,
                        obi_5: *obi_5,
                        obi_10: *obi_10,
                        obi_20: *obi_20,
                        bid_volume_5: *bid_volume_5,
                        ask_volume_5: *ask_volume_5,
                        update_id: 0,
                    },
                );
            }
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                self.quotes.insert(token_id.clone(), *quote);
            }
            MarketUpdate::EventDiscovered { .. } => self.track_event(update),
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                self.last_logged_at.remove(event_id);
            }
            MarketUpdate::BinanceKline { .. } => {}
        }

        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, _update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.enabled {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        let event_ids: Vec<String> = self.active_events.keys().cloned().collect();
        for event_id in event_ids {
            let Some(event) = self.active_events.get(&event_id).cloned() else {
                continue;
            };

            match self.evaluate_event(now, &event) {
                Ok(Some(signal)) => {
                    self.last_signal = Some(signal.clone());
                    self.last_reason = Some(format!(
                        "{} {} ready",
                        signal.symbol,
                        Self::action_label(signal.action)
                    ));
                    self.last_error = None;

                    if self.should_emit_signal_log(&event_id, now) {
                        actions.push(StrategyAction::LogEvent {
                            event: self.signal_event(&event, &signal),
                        });
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    self.last_error = Some(err.to_string());
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::Error,
                            format!("crypto_rl_policy evaluation failed for {}", event.event_id),
                        )
                        .with_data("event_id", &event.event_id)
                        .with_data("symbol", &event.symbol)
                        .with_data("error", err.to_string()),
                    });
                }
            }
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert("symbols".to_string(), self.symbols.join(","));
        metrics.insert(
            "active_events".to_string(),
            self.active_events.len().to_string(),
        );
        metrics.insert("quote_count".to_string(), self.quotes.len().to_string());
        metrics.insert(
            "l2_symbols".to_string(),
            self.l2_by_symbol.len().to_string(),
        );
        if let Some(reason) = &self.last_reason {
            metrics.insert("last_reason".to_string(), reason.clone());
        }
        if let Some(error) = &self.last_error {
            metrics.insert("last_error".to_string(), error.clone());
        }
        if let Some(signal) = &self.last_signal {
            metrics.insert("last_event_id".to_string(), signal.event_id.clone());
            metrics.insert("last_symbol".to_string(), signal.symbol.clone());
            metrics.insert(
                "last_action".to_string(),
                Self::action_label(signal.action).to_string(),
            );
            metrics.insert(
                "last_policy_source".to_string(),
                signal.policy_source.clone(),
            );
            metrics.insert(
                "last_desired_shares".to_string(),
                signal.desired_shares.to_string(),
            );
            metrics.insert(
                "last_remaining_secs".to_string(),
                signal.remaining_secs.to_string(),
            );
            metrics.insert("last_at".to_string(), signal.at.to_rfc3339());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "observe_only".to_string(),
            enabled: self.enabled,
            active: !self.active_events.is_empty(),
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        Vec::new()
    }

    fn is_active(&self) -> bool {
        !self.active_events.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(Vec::new())
    }

    fn reset(&mut self) {
        self.spot_prices.clear();
        self.l2_by_symbol.clear();
        self.quotes.clear();
        self.active_events.clear();
        self.last_signal = None;
        self.last_reason = None;
        self.last_error = None;
        self.last_logged_at.clear();
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
    use crate::domain::Side;

    fn minimal_toml() -> &'static str {
        r#"
[strategy]
name = "crypto_rl_policy"
enabled = true

[crypto_rl_policy]
coins = ["BTC"]
tick_interval_ms = 1000
min_time_remaining_secs = 60
max_time_remaining_secs = 300
max_lob_snapshot_age_secs = 2
max_entry_price = 0.70
"#
    }

    fn event_update(now: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::EventDiscovered {
            event_id: "evt-btc-5m".to_string(),
            series_id: "10684".to_string(),
            up_token: "up-token".to_string(),
            down_token: "down-token".to_string(),
            end_time: now + chrono::Duration::seconds(120),
            price_to_beat: Some(dec!(102500)),
            title: Some("BTC above 102500".to_string()),
            condition_id: None,
        }
    }

    fn price_update(price: i64, ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::BinancePrice {
            symbol: "BTCUSDT".to_string(),
            price: Decimal::from(price),
            timestamp: ts,
        }
    }

    fn l2_update(ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::BinanceL2 {
            symbol: "BTCUSDT".to_string(),
            obi_1: dec!(0.12),
            obi_2: dec!(0.11),
            obi_3: dec!(0.10),
            obi_5: dec!(0.08),
            obi_10: dec!(0.06),
            obi_20: dec!(0.04),
            bid_volume_5: dec!(1200),
            ask_volume_5: dec!(1150),
            spread_bps: dec!(1.8),
            timestamp: ts,
        }
    }

    fn quote_update(
        token_id: &str,
        side: Side,
        bid: Decimal,
        ask: Decimal,
        ts: DateTime<Utc>,
    ) -> MarketUpdate {
        MarketUpdate::PolymarketQuote {
            token_id: token_id.to_string(),
            side,
            quote: Quote {
                side,
                best_bid: Some(bid),
                best_ask: Some(ask),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: ts,
            },
            timestamp: ts,
        }
    }

    #[test]
    fn from_toml_builds_expected_feeds() {
        let strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
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

    #[tokio::test]
    async fn event_discovered_tracks_event_without_legacy_control_actions() {
        let mut strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");

        let actions = strategy
            .on_market_update(&event_update(Utc::now()))
            .await
            .expect("event tracked");

        assert!(actions.is_empty());
        assert_eq!(strategy.active_events.len(), 1);
    }

    #[tokio::test]
    async fn on_tick_emits_buy_up_signal_log_when_rule_based_policy_triggers() {
        let mut strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();
        let tick = start + chrono::Duration::seconds(5);

        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event tracked");
        strategy
            .on_market_update(&price_update(100000, start))
            .await
            .expect("initial price");
        strategy
            .on_market_update(&price_update(101000, tick))
            .await
            .expect("latest price");
        strategy
            .on_market_update(&l2_update(tick))
            .await
            .expect("l2 update");
        strategy
            .on_market_update(&quote_update(
                "up-token",
                Side::Up,
                dec!(0.45),
                dec!(0.46),
                tick,
            ))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&quote_update(
                "down-token",
                Side::Down,
                dec!(0.47),
                dec!(0.48),
                tick,
            ))
            .await
            .expect("down quote");

        let actions = strategy.on_tick(tick).await.expect("tick");
        assert!(actions.iter().any(|action| matches!(
            action,
            StrategyAction::LogEvent { event }
                if matches!(&event.event_type, StrategyEventType::Custom(kind) if kind == "crypto_rl_policy_signal")
        )));
        assert_eq!(
            strategy.state().metrics.get("last_action"),
            Some(&"buy_up".to_string())
        );
    }
}
