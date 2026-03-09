use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::adapters::SpotPrice;
use crate::collector::LobSnapshot;
use crate::domain::Quote;
use crate::error::{PloyError, Result};
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::strategy::crypto::{known_binance_symbols, series_ids_for_symbol, series_info};
use crate::strategy::crypto_rl_policy::core;
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyControlAction, StrategyEvent, StrategyEventType, StrategyStateInfo,
};

const STRATEGY_NAME: &str = "crypto_rl_policy";
const SIGNAL_LOG_INTERVAL_SECS: i64 = 30;

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

#[derive(Debug, Clone)]
struct RlTrackedEvent {
    event_id: String,
    series_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct RlSignalSummary {
    event_id: String,
    symbol: String,
    series_id: String,
    action: core::DiscreteAction,
    policy_source: String,
    desired_shares: u64,
    up_ask: Decimal,
    down_ask: Decimal,
    remaining_secs: i64,
    obs_version: u32,
    momentum_1s: Decimal,
    momentum_5s: Decimal,
    at: DateTime<Utc>,
}

pub struct CryptoRlPolicyStrategy {
    id: String,
    enabled: bool,
    cfg: CryptoRlPolicyStrategyConfig,
    symbols: Vec<String>,
    series_ids: Vec<String>,
    quote_tokens: HashSet<String>,
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
            quote_tokens: HashSet::new(),
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

    fn track_event(&mut self, update: &MarketUpdate) -> Vec<StrategyAction> {
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
            return Vec::new();
        };

        let Some(info) = series_info(series_id) else {
            return Vec::new();
        };
        if !self.symbols.iter().any(|symbol| symbol == info.symbol) {
            return Vec::new();
        }

        self.active_events.insert(
            event_id.clone(),
            RlTrackedEvent {
                event_id: event_id.clone(),
                series_id: series_id.clone(),
                symbol: info.symbol.to_string(),
                up_token: up_token.clone(),
                down_token: down_token.clone(),
                end_time: *end_time,
                price_to_beat: *price_to_beat,
                title: title.clone(),
            },
        );

        let mut new_tokens = Vec::new();
        for token in [up_token, down_token] {
            if self.quote_tokens.insert(token.clone()) {
                new_tokens.push(token.clone());
            }
        }

        if new_tokens.is_empty() {
            Vec::new()
        } else {
            vec![StrategyAction::LegacyControl(
                StrategyControlAction::SubscribeFeed {
                    feed: DataFeed::PolymarketQuotes { tokens: new_tokens },
                },
            )]
        }
    }

    fn should_emit_signal_log(&mut self, event_id: &str, now: DateTime<Utc>) -> bool {
        match self.last_logged_at.get(event_id) {
            None => {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            Some(last)
                if now.signed_duration_since(*last).num_seconds() >= SIGNAL_LOG_INTERVAL_SECS =>
            {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            _ => false,
        }
    }

    fn action_label(action: core::DiscreteAction) -> &'static str {
        match action {
            core::DiscreteAction::Hold => "hold",
            core::DiscreteAction::BuyUp => "buy_up",
            core::DiscreteAction::BuyDown => "buy_down",
            core::DiscreteAction::SellPosition => "sell_position",
            core::DiscreteAction::EnterHedge => "enter_hedge",
        }
    }

    fn evaluate_event(
        &mut self,
        now: DateTime<Utc>,
        event: &RlTrackedEvent,
    ) -> Result<Option<RlSignalSummary>> {
        let remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
        if remaining_secs < self.cfg.min_time_remaining_secs as i64 {
            self.last_reason = Some(format!("{} below_min_remaining", event.symbol));
            return Ok(None);
        }
        if remaining_secs > self.cfg.max_time_remaining_secs as i64 {
            self.last_reason = Some(format!("{} above_max_remaining", event.symbol));
            return Ok(None);
        }

        let Some(spot) = self.spot_prices.get(&event.symbol) else {
            self.last_reason = Some(format!("{} waiting_spot", event.symbol));
            return Ok(None);
        };
        let Some(l2) = self.l2_by_symbol.get(&event.symbol) else {
            self.last_reason = Some(format!("{} waiting_l2", event.symbol));
            return Ok(None);
        };
        if now.signed_duration_since(l2.timestamp).num_seconds()
            > self.cfg.max_lob_snapshot_age_secs as i64
        {
            self.last_reason = Some(format!("{} stale_l2", event.symbol));
            return Ok(None);
        }

        let Some(up_quote) = self.quotes.get(&event.up_token) else {
            self.last_reason = Some(format!("{} waiting_up_quote", event.symbol));
            return Ok(None);
        };
        let Some(down_quote) = self.quotes.get(&event.down_token) else {
            self.last_reason = Some(format!("{} waiting_down_quote", event.symbol));
            return Ok(None);
        };

        #[cfg(feature = "onnx")]
        let (up_bid, up_ask, down_bid, down_ask) = match (
            up_quote.best_bid,
            up_quote.best_ask,
            down_quote.best_bid,
            down_quote.best_ask,
        ) {
            (Some(up_bid), Some(up_ask), Some(down_bid), Some(down_ask))
                if up_ask > Decimal::ZERO && down_ask > Decimal::ZERO =>
            {
                (up_bid, up_ask, down_bid, down_ask)
            }
            _ => {
                self.last_reason = Some(format!("{} incomplete_quotes", event.symbol));
                return Ok(None);
            }
        };
        #[cfg(not(feature = "onnx"))]
        let (_up_bid, up_ask, _down_bid, down_ask) = match (
            up_quote.best_bid,
            up_quote.best_ask,
            down_quote.best_bid,
            down_quote.best_ask,
        ) {
            (Some(up_bid), Some(up_ask), Some(down_bid), Some(down_ask))
                if up_ask > Decimal::ZERO && down_ask > Decimal::ZERO =>
            {
                (up_bid, up_ask, down_bid, down_ask)
            }
            _ => {
                self.last_reason = Some(format!("{} incomplete_quotes", event.symbol));
                return Ok(None);
            }
        };

        let momentum_1s = spot.momentum(1).unwrap_or(Decimal::ZERO);
        let momentum_5s = spot.momentum(5).unwrap_or(Decimal::ZERO);

        #[cfg(feature = "onnx")]
        let mut policy_source = "rule_based".to_string();
        #[cfg(not(feature = "onnx"))]
        let policy_source = "rule_based".to_string();

        #[cfg(feature = "onnx")]
        let action = if let Some(model) = &self.policy_model {
            let obs = if self.cfg.observation_version == 2 {
                core::build_observation_v2(
                    self.cfg.default_shares,
                    self.cfg.max_time_remaining_secs,
                    now,
                    spot.price,
                    momentum_1s,
                    momentum_5s,
                    l2,
                    up_bid,
                    up_ask,
                    down_bid,
                    down_ask,
                    None,
                    remaining_secs,
                    l2.obi_1,
                    l2.obi_2,
                    l2.obi_3,
                    l2.obi_20,
                )
            } else {
                core::build_observation_v1(
                    self.cfg.default_shares,
                    self.cfg.max_time_remaining_secs,
                    now,
                    spot.price,
                    momentum_1s,
                    momentum_5s,
                    l2,
                    up_bid,
                    up_ask,
                    down_bid,
                    down_ask,
                    None,
                    remaining_secs,
                )
            };

            match model
                .predict(&obs)
                .ok()
                .and_then(|output| core::action_from_policy_output(self.cfg.policy_output.as_str(), &output))
            {
                Some(action) => {
                    policy_source = "onnx".to_string();
                    action
                }
                None => core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None),
            }
        } else {
            core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None)
        };

        #[cfg(not(feature = "onnx"))]
        let action = core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None);

        let discrete = action.to_discrete();
        if matches!(
            discrete,
            core::DiscreteAction::Hold | core::DiscreteAction::SellPosition
        ) {
            self.last_reason = Some(format!(
                "{} {}",
                event.symbol,
                Self::action_label(discrete)
            ));
            return Ok(None);
        }

        match discrete {
            core::DiscreteAction::BuyUp if up_ask > self.cfg.max_entry_price => {
                self.last_reason = Some(format!("{} buy_up_above_max_entry", event.symbol));
                return Ok(None);
            }
            core::DiscreteAction::BuyDown if down_ask > self.cfg.max_entry_price => {
                self.last_reason = Some(format!("{} buy_down_above_max_entry", event.symbol));
                return Ok(None);
            }
            core::DiscreteAction::EnterHedge
                if up_ask > self.cfg.max_entry_price
                    || down_ask > self.cfg.max_entry_price
                    || up_ask + down_ask >= dec!(1.0) =>
            {
                self.last_reason = Some(format!("{} hedge_gate_reject", event.symbol));
                return Ok(None);
            }
            _ => {}
        }

        Ok(Some(RlSignalSummary {
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            series_id: event.series_id.clone(),
            action: discrete,
            policy_source,
            desired_shares: core::compute_shares(&action, self.cfg.default_shares),
            up_ask,
            down_ask,
            remaining_secs,
            obs_version: self.cfg.observation_version,
            momentum_1s,
            momentum_5s,
            at: now,
        }))
    }

    fn signal_event(&self, event: &RlTrackedEvent, signal: &RlSignalSummary) -> StrategyEvent {
        StrategyEvent::new(
            StrategyEventType::Custom("crypto_rl_policy_signal".to_string()),
            format!(
                "crypto_rl_policy {} {}",
                signal.symbol,
                Self::action_label(signal.action)
            ),
        )
        .with_data("event_id", &signal.event_id)
        .with_data("series_id", &signal.series_id)
        .with_data("symbol", &signal.symbol)
        .with_data("action", Self::action_label(signal.action))
        .with_data("policy_source", &signal.policy_source)
        .with_data("desired_shares", signal.desired_shares.to_string())
        .with_data("up_ask", signal.up_ask.to_string())
        .with_data("down_ask", signal.down_ask.to_string())
        .with_data("remaining_secs", signal.remaining_secs.to_string())
        .with_data("obs_version", signal.obs_version.to_string())
        .with_data("momentum_1s", signal.momentum_1s.to_string())
        .with_data("momentum_5s", signal.momentum_5s.to_string())
        .with_data(
            "policy_model_version",
            self.cfg
                .policy_model_version
                .clone()
                .unwrap_or_default(),
        )
        .with_data("title", event.title.clone().unwrap_or_default())
        .with_data("at", signal.at.to_rfc3339())
        .with_data(
            "price_to_beat",
            event.price_to_beat
                .map(|price| price.to_string())
                .unwrap_or_default(),
        )
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
            MarketUpdate::EventDiscovered { .. } => return Ok(self.track_event(update)),
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
            "quote_token_count".to_string(),
            self.quote_tokens.len().to_string(),
        );
        metrics.insert("l2_symbols".to_string(), self.l2_by_symbol.len().to_string());
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
            metrics.insert("last_policy_source".to_string(), signal.policy_source.clone());
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
        self.quote_tokens.clear();
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

    fn quote_update(token_id: &str, side: Side, bid: Decimal, ask: Decimal, ts: DateTime<Utc>) -> MarketUpdate {
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
    async fn event_discovered_requests_quote_subscription() {
        let mut strategy =
            CryptoRlPolicyStrategy::from_toml("rl-test".to_string(), minimal_toml(), true)
                .expect("strategy");

        let actions = strategy
            .on_market_update(&event_update(Utc::now()))
            .await
            .expect("event tracked");

        assert!(actions.iter().any(|action| matches!(
            action,
            StrategyAction::LegacyControl(StrategyControlAction::SubscribeFeed {
                feed: DataFeed::PolymarketQuotes { tokens }
            }) if tokens == &vec!["up-token".to_string(), "down-token".to_string()]
        )));
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
