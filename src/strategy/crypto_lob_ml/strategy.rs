use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};

use crate::adapters::SpotPrice;
use crate::domain::Quote;
use crate::error::{PloyError, Result};
use crate::strategy::crypto::{known_binance_symbols, series_ids_for_symbol, series_info};
use crate::strategy::crypto_lob_ml::core::{self, SequenceSnapshot};
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};

mod inference;

const STRATEGY_NAME: &str = "crypto_lob_ml";
const INFERENCE_LOG_INTERVAL_SECS: i64 = 30;

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct CryptoLobMlStrategyConfig {
    coins: Vec<String>,
    min_time_remaining_secs: u64,
    max_time_remaining_secs: u64,
    max_time_remaining_secs_5m: u64,
    max_time_remaining_secs_15m: u64,
    require_price_to_beat: bool,
    max_lob_snapshot_age_secs: u64,
    feature_offsets: Vec<f32>,
    feature_scales: Vec<f32>,
    oracle_lag_buffer_secs: u64,
    tick_interval_ms: u64,
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

#[derive(Debug, Clone)]
struct LobMlL2Snapshot {
    obi_5: Decimal,
    obi_10: Decimal,
    spread_bps: Decimal,
    bid_volume_5: Decimal,
    ask_volume_5: Decimal,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct LobMlTrackedEvent {
    event_id: String,
    series_id: String,
    symbol: String,
    horizon: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct LobMlInferenceSummary {
    event_id: String,
    symbol: String,
    horizon: String,
    p_gbm_anchor: Decimal,
    remaining_secs: i64,
    sequence_snapshots: usize,
    up_mid: Option<Decimal>,
    down_mid: Option<Decimal>,
    at: DateTime<Utc>,
}

pub struct CryptoLobMlStrategy {
    id: String,
    enabled: bool,
    cfg: CryptoLobMlStrategyConfig,
    symbols: Vec<String>,
    series_ids: Vec<String>,
    spot_prices: HashMap<String, SpotPrice>,
    l2_by_symbol: HashMap<String, LobMlL2Snapshot>,
    quotes: HashMap<String, Quote>,
    active_events: HashMap<String, LobMlTrackedEvent>,
    sequence_cache: HashMap<String, VecDeque<SequenceSnapshot>>,
    last_inference: Option<LobMlInferenceSummary>,
    last_reason: Option<String>,
    last_error: Option<String>,
    last_logged_at: HashMap<String, DateTime<Utc>>,
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
            spot_prices: HashMap::new(),
            l2_by_symbol: HashMap::new(),
            quotes: HashMap::new(),
            active_events: HashMap::new(),
            sequence_cache: HashMap::new(),
            last_inference: None,
            last_reason: None,
            last_error: None,
            last_logged_at: HashMap::new(),
        })
    }

    fn track_event(&mut self, update: &MarketUpdate) {
        let MarketUpdate::EventDiscovered {
            event_id,
            series_id,
            up_token,
            down_token,
            end_time,
            price_to_beat,
            title,
            ..
        } = update
        else {
            return;
        };

        let Some(info) = series_info(series_id) else {
            return;
        };
        if !self.symbols.iter().any(|symbol| symbol == info.symbol) {
            return;
        }

        self.active_events.insert(
            event_id.clone(),
            LobMlTrackedEvent {
                event_id: event_id.clone(),
                series_id: series_id.clone(),
                symbol: info.symbol.to_string(),
                horizon: info.horizon.to_string(),
                up_token: up_token.clone(),
                down_token: down_token.clone(),
                end_time: *end_time,
                price_to_beat: *price_to_beat,
                title: title.clone(),
            },
        );
    }
}

#[async_trait]
impl Strategy for CryptoLobMlStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Canonical no-submit crypto LOB ML wrapper for feed/cache/inference migration"
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
            } => {
                if self.symbols.iter().any(|tracked| tracked == symbol) {
                    self.spot_prices
                        .entry(symbol.clone())
                        .and_modify(|spot| spot.update(*price, None, *timestamp))
                        .or_insert_with(|| SpotPrice::new(*price, None, *timestamp));
                }
            }
            MarketUpdate::BinanceL2 {
                symbol,
                obi_5,
                obi_10,
                spread_bps,
                bid_volume_5,
                ask_volume_5,
                timestamp,
                ..
            } => {
                if self.symbols.iter().any(|tracked| tracked == symbol) {
                    self.l2_by_symbol.insert(
                        symbol.clone(),
                        LobMlL2Snapshot {
                            obi_5: *obi_5,
                            obi_10: *obi_10,
                            spread_bps: *spread_bps,
                            bid_volume_5: *bid_volume_5,
                            ask_volume_5: *ask_volume_5,
                            timestamp: *timestamp,
                        },
                    );
                }
            }
            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                self.quotes.insert(token_id.clone(), *quote);
            }
            MarketUpdate::EventDiscovered { .. } => {
                self.track_event(update);
            }
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                self.last_logged_at.remove(event_id);
            }
            MarketUpdate::BinanceTrade { .. }
            | MarketUpdate::BinanceKline { .. }
            | MarketUpdate::BinanceFunding { .. }
            | MarketUpdate::BinanceLiquidation { .. }
            | MarketUpdate::DeribitIV { .. } => {}
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
                Ok(Some(summary)) => {
                    self.last_inference = Some(summary.clone());
                    self.last_reason =
                        Some(format!("{}:{} ready", summary.symbol, summary.horizon));
                    self.last_error = None;

                    if self.should_emit_inference_log(&event_id, now) {
                        actions.push(StrategyAction::LogEvent {
                            event: self.inference_event(&event, &summary),
                        });
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    self.last_error = Some(err.to_string());
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::Error,
                            format!("crypto_lob_ml evaluation failed for {}", event.event_id),
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
        metrics.insert(
            "sequence_keys".to_string(),
            self.sequence_cache.len().to_string(),
        );
        metrics.insert("quote_count".to_string(), self.quotes.len().to_string());
        if let Some(reason) = &self.last_reason {
            metrics.insert("last_reason".to_string(), reason.clone());
        }
        if let Some(error) = &self.last_error {
            metrics.insert("last_error".to_string(), error.clone());
        }
        if let Some(inference) = &self.last_inference {
            metrics.insert("last_event_id".to_string(), inference.event_id.clone());
            metrics.insert("last_symbol".to_string(), inference.symbol.clone());
            metrics.insert("last_horizon".to_string(), inference.horizon.clone());
            metrics.insert(
                "last_remaining_secs".to_string(),
                inference.remaining_secs.to_string(),
            );
            metrics.insert(
                "last_sequence_snapshots".to_string(),
                inference.sequence_snapshots.to_string(),
            );
            metrics.insert(
                "last_p_gbm_anchor".to_string(),
                inference.p_gbm_anchor.to_string(),
            );
            metrics.insert("last_at".to_string(), inference.at.to_rfc3339());
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
        self.sequence_cache.clear();
        self.last_inference = None;
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
    use rust_decimal_macros::dec;

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

    fn price_update(ts: DateTime<Utc>, offset: i64) -> MarketUpdate {
        MarketUpdate::BinancePrice {
            symbol: "BTCUSDT".to_string(),
            price: Decimal::from(102000 + offset),
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

    #[tokio::test]
    async fn on_tick_emits_inference_log_once_sequence_is_ready() {
        let mut strategy =
            CryptoLobMlStrategy::from_toml("lob-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();
        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event tracked");

        let mut saw_inference_log = false;
        for second in 0..core::SEQ_LEN_5M {
            let ts = start + chrono::Duration::seconds(second as i64);
            strategy
                .on_market_update(&price_update(ts, second as i64))
                .await
                .expect("price update");
            strategy
                .on_market_update(&l2_update(ts))
                .await
                .expect("l2 update");

            let actions = strategy.on_tick(ts).await.expect("tick");
            if second + 1 == core::SEQ_LEN_5M {
                saw_inference_log = actions.iter().any(|action| matches!(
                    action,
                    StrategyAction::LogEvent { event }
                        if matches!(&event.event_type, StrategyEventType::Custom(kind) if kind == "crypto_lob_ml_inference")
                ));
            }
        }

        assert!(
            saw_inference_log,
            "expected inference log once sequence is warm"
        );
        let state = strategy.state();
        assert_eq!(
            state.metrics.get("last_symbol"),
            Some(&"BTCUSDT".to_string())
        );
        assert_eq!(state.metrics.get("last_horizon"), Some(&"5m".to_string()));
    }

    #[tokio::test]
    async fn on_tick_skips_events_without_price_to_beat_when_required() {
        let mut strategy =
            CryptoLobMlStrategy::from_toml("lob-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();
        strategy
            .on_market_update(&MarketUpdate::EventDiscovered {
                event_id: "evt-btc-5m".to_string(),
                series_id: "10684".to_string(),
                up_token: "up-token".to_string(),
                down_token: "down-token".to_string(),
                end_time: start + chrono::Duration::seconds(120),
                price_to_beat: None,
                title: Some("BTC above 102500".to_string()),
                condition_id: None,
            })
            .await
            .expect("event tracked");

        for second in 0..core::SEQ_LEN_5M {
            let ts = start + chrono::Duration::seconds(second as i64);
            strategy
                .on_market_update(&price_update(ts, second as i64))
                .await
                .expect("price update");
            strategy
                .on_market_update(&l2_update(ts))
                .await
                .expect("l2 update");
        }

        let actions = strategy
            .on_tick(start + chrono::Duration::seconds(60))
            .await
            .unwrap();
        assert!(actions.is_empty());
        assert_eq!(
            strategy.last_reason.as_deref(),
            Some("BTCUSDT:5m missing_price_to_beat")
        );
    }
}
