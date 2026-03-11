use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};

use crate::adapters::SpotPrice;
use crate::domain::{OrderStatus, OrderType, Quote, Side, TimeInForce};
use crate::error::{PloyError, Result};
use crate::platform::Domain;
use crate::strategy::crypto::{horizon_for_series, known_binance_symbols, series_ids_for_symbol};
use crate::strategy::fee_model::FeeModel;
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};
use crate::strategy::volatility::normal_cdf;

const STRATEGY_NAME: &str = "pm_5m_directional";
const PROB_FLOOR: f64 = 1e-6;

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct Pm5mDirectionalConfig {
    symbols: Vec<String>,
    tick_interval_ms: u64,
    min_time_remaining_secs: u64,
    max_time_remaining_secs: u64,
    final_no_entry_secs: u64,
    vol_lookback_secs: u64,
    vol_floor: f64,
    p_entry: f64,
    min_edge: f64,
    min_abs_z: f64,
    min_obi: f64,
    min_flow_2s: f64,
    obi_weight: f64,
    obi_shape_weight: f64,
    flow_weight: f64,
    microgap_weight: f64,
    max_l2_age_secs: u64,
    max_pm_spread: Decimal,
    min_pm_ask_size: Decimal,
    max_entry_price: Decimal,
    no_trade_price_min: Decimal,
    no_trade_price_max: Decimal,
    no_trade_override_z: f64,
    no_trade_override_flow: f64,
    shares_per_trade: u64,
    min_shares: u64,
    cooldown_secs: u64,
    max_daily_trades: u32,
    use_kelly_sizing: bool,
    kelly_fraction_scale: f64,
    kelly_fraction_cap: f64,
}

impl Default for Pm5mDirectionalConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            tick_interval_ms: 1000,
            min_time_remaining_secs: 15,
            max_time_remaining_secs: 300,
            final_no_entry_secs: 10,
            vol_lookback_secs: 30,
            vol_floor: 0.0005,
            p_entry: 0.62,
            min_edge: 0.03,
            min_abs_z: 0.35,
            min_obi: 0.05,
            min_flow_2s: 0.10,
            obi_weight: 0.75,
            obi_shape_weight: 0.30,
            flow_weight: 1.10,
            microgap_weight: 0.40,
            max_l2_age_secs: 2,
            max_pm_spread: dec!(0.03),
            min_pm_ask_size: dec!(25),
            max_entry_price: dec!(0.80),
            no_trade_price_min: dec!(0.45),
            no_trade_price_max: dec!(0.55),
            no_trade_override_z: 0.90,
            no_trade_override_flow: 0.45,
            shares_per_trade: 25,
            min_shares: 5,
            cooldown_secs: 30,
            max_daily_trades: 0,
            use_kelly_sizing: true,
            kelly_fraction_scale: 0.15,
            kelly_fraction_cap: 0.25,
        }
    }
}

impl Pm5mDirectionalConfig {
    fn normalize(&mut self) {
        if self.tick_interval_ms == 0 {
            self.tick_interval_ms = 1000;
        }
        if self.symbols.is_empty() {
            self.symbols = vec!["BTCUSDT".to_string()];
        }
        self.symbols = self
            .symbols
            .iter()
            .filter_map(|symbol| normalize_symbol(symbol))
            .collect();
        self.symbols.sort();
        self.symbols.dedup();
        if self.symbols.is_empty() {
            self.symbols = vec!["BTCUSDT".to_string()];
        }
        self.max_time_remaining_secs = self
            .max_time_remaining_secs
            .max(self.min_time_remaining_secs);
        self.final_no_entry_secs = self
            .final_no_entry_secs
            .min(self.max_time_remaining_secs.saturating_sub(1));
        self.vol_floor = self.vol_floor.max(1e-6);
        self.p_entry = self.p_entry.clamp(0.5, 0.99);
        self.min_edge = self.min_edge.max(0.0);
        self.min_abs_z = self.min_abs_z.max(0.0);
        self.min_obi = self.min_obi.max(0.0);
        self.min_flow_2s = self.min_flow_2s.max(0.0);
        self.no_trade_override_z = self.no_trade_override_z.max(self.min_abs_z);
        self.no_trade_override_flow = self.no_trade_override_flow.max(self.min_flow_2s);
        self.min_shares = self.min_shares.max(1);
        self.shares_per_trade = self.shares_per_trade.max(self.min_shares);
        self.kelly_fraction_scale = self.kelly_fraction_scale.max(0.0);
        self.kelly_fraction_cap = self.kelly_fraction_cap.clamp(0.01, 1.0);
    }

    fn configured_symbols(&self) -> Vec<String> {
        let mut symbols = self.symbols.clone();
        if symbols.is_empty() {
            symbols = vec!["BTCUSDT".to_string()];
        }
        symbols
    }

    fn configured_series_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for symbol in self.configured_symbols() {
            ids.extend(
                series_ids_for_symbol(&symbol)
                    .into_iter()
                    .filter(|series_id| horizon_for_series(series_id) == "5m"),
            );
        }
        ids.sort();
        ids.dedup();
        ids
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Pm5mDirectionalToml {
    strategy: StrategySection,
    #[serde(default)]
    pm_5m_directional: Pm5mDirectionalConfig,
}

#[derive(Debug, Clone)]
struct BinanceL2State {
    obi_1: Decimal,
    obi_2: Decimal,
    obi_3: Decimal,
    obi_5: Decimal,
    obi_10: Decimal,
    obi_20: Decimal,
    spread_bps: Decimal,
    bid_volume_5: Decimal,
    ask_volume_5: Decimal,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
struct ObiFactors {
    main: f64,
    obi_10: f64,
    obi_20: f64,
    shape: f64,
    micro: f64,
    slope: f64,
}

#[derive(Debug, Clone)]
struct TrackedEvent {
    event_id: String,
    symbol: String,
    up_token_id: String,
    down_token_id: String,
    end_time: DateTime<Utc>,
    price_to_beat: Decimal,
}

#[derive(Debug, Clone)]
struct TickObservation {
    price: Decimal,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct PendingOrder {
    symbol: String,
    token_id: String,
    side: Side,
    shares: u64,
    event_id: String,
}

#[derive(Debug, Clone)]
struct DirectionalPosition {
    event_id: String,
    token_id: String,
    symbol: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    current_price: Option<Decimal>,
    opened_at: DateTime<Utc>,
    end_time: DateTime<Utc>,
}

pub struct Pm5mDirectionalStrategy {
    id: String,
    enabled: bool,
    dry_run: bool,
    cfg: Pm5mDirectionalConfig,
    symbols: Vec<String>,
    series_ids: Vec<String>,
    fee_model: FeeModel,
    spot_prices: HashMap<String, SpotPrice>,
    l2_by_symbol: HashMap<String, BinanceL2State>,
    quotes: HashMap<String, Quote>,
    active_events: HashMap<String, TrackedEvent>,
    recent_ticks: HashMap<String, VecDeque<TickObservation>>,
    pending_orders: HashMap<String, PendingOrder>,
    positions: HashMap<String, DirectionalPosition>,
    last_trade_at: HashMap<String, DateTime<Utc>>,
    daily_trades: u32,
    last_reset: DateTime<Utc>,
    last_reason: Option<String>,
}

impl Pm5mDirectionalStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let parsed: Pm5mDirectionalToml = toml::from_str(config_str)
            .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;
        if parsed.strategy.name != STRATEGY_NAME {
            return Err(PloyError::Validation(format!(
                "strategy.name must be \"{STRATEGY_NAME}\", got \"{}\"",
                parsed.strategy.name
            )));
        }

        let mut cfg = parsed.pm_5m_directional;
        cfg.normalize();
        let symbols = cfg.configured_symbols();
        let series_ids = cfg.configured_series_ids();
        if series_ids.is_empty() {
            return Err(PloyError::Validation(
                "pm_5m_directional requires at least one supported 5m crypto series".to_string(),
            ));
        }

        Ok(Self {
            id,
            enabled: parsed.strategy.enabled.unwrap_or(true),
            dry_run,
            cfg,
            symbols,
            series_ids,
            fee_model: FeeModel::crypto(),
            spot_prices: HashMap::new(),
            l2_by_symbol: HashMap::new(),
            quotes: HashMap::new(),
            active_events: HashMap::new(),
            recent_ticks: HashMap::new(),
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            last_trade_at: HashMap::new(),
            daily_trades: 0,
            last_reset: Utc::now(),
            last_reason: None,
        })
    }

    fn best_event_for(&self, symbol: &str, now: DateTime<Utc>) -> Option<&TrackedEvent> {
        self.active_events
            .values()
            .filter(|event| event.symbol == symbol)
            .filter(|event| {
                let remaining = (event.end_time - now).num_seconds();
                remaining >= self.cfg.min_time_remaining_secs as i64
                    && remaining <= self.cfg.max_time_remaining_secs as i64
            })
            .min_by_key(|event| event.end_time)
    }

    fn reset_daily_counter_if_needed(&mut self, now: DateTime<Utc>) {
        if now.date_naive() != self.last_reset.date_naive() {
            self.daily_trades = 0;
            self.last_reset = now;
        }
    }

    fn daily_limit_reached(&mut self, now: DateTime<Utc>) -> bool {
        self.reset_daily_counter_if_needed(now);
        self.cfg.max_daily_trades > 0 && self.daily_trades >= self.cfg.max_daily_trades
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        self.last_trade_at
            .get(symbol)
            .map(|last| (now - *last).num_seconds() < self.cfg.cooldown_secs as i64)
            .unwrap_or(false)
    }

    fn has_open_symbol_risk(&self, symbol: &str) -> bool {
        self.positions
            .values()
            .any(|position| position.symbol == symbol)
            || self
                .pending_orders
                .values()
                .any(|pending| pending.symbol == symbol)
    }

    fn update_tick_history(&mut self, symbol: &str, price: Decimal, timestamp: DateTime<Utc>) {
        let ticks = self.recent_ticks.entry(symbol.to_string()).or_default();
        ticks.push_back(TickObservation { price, timestamp });
        let cutoff = timestamp - chrono::Duration::seconds(5);
        while ticks
            .front()
            .map(|tick| tick.timestamp < cutoff)
            .unwrap_or(false)
        {
            let _ = ticks.pop_front();
        }
    }

    fn signed_flow_2s(&self, symbol: &str, now: DateTime<Utc>) -> Option<f64> {
        let ticks = self.recent_ticks.get(symbol)?;
        let cutoff = now - chrono::Duration::seconds(2);
        let mut last_price: Option<Decimal> = None;
        let mut last_sign = 0.0f64;
        let mut signed = 0.0f64;
        let mut count = 0u64;

        for tick in ticks.iter().filter(|tick| tick.timestamp >= cutoff) {
            if let Some(previous) = last_price {
                let sign = if tick.price > previous {
                    1.0
                } else if tick.price < previous {
                    -1.0
                } else {
                    last_sign
                };
                signed += sign;
                count += 1;
                last_sign = sign;
            }
            last_price = Some(tick.price);
        }

        if count == 0 {
            None
        } else {
            Some((signed / count as f64).clamp(-1.0, 1.0))
        }
    }

    fn microgap_proxy(l2: &BinanceL2State) -> f64 {
        let obi = l2.obi_1.to_f64().unwrap_or(0.0);
        let spread_scale = (l2.spread_bps.to_f64().unwrap_or(0.0) / 5.0).clamp(0.0, 1.0);
        (obi * spread_scale).clamp(-1.0, 1.0)
    }

    fn obi_factors(l2: &BinanceL2State) -> ObiFactors {
        let obi_1 = l2.obi_1.to_f64().unwrap_or(0.0);
        let obi_2 = l2.obi_2.to_f64().unwrap_or(0.0);
        let obi_5 = l2.obi_5.to_f64().unwrap_or(0.0);
        let obi_10 = l2.obi_10.to_f64().unwrap_or(0.0);
        let obi_20 = l2.obi_20.to_f64().unwrap_or(0.0);
        let obi_micro = ((((obi_1 + obi_2) / 2.0) - obi_5)).clamp(-2.0, 2.0);
        let obi_slope = (obi_5 - obi_20).clamp(-2.0, 2.0);

        ObiFactors {
            main: ((0.60 * obi_5) + (0.25 * obi_10) + (0.15 * obi_20)).clamp(-1.0, 1.0),
            obi_10,
            obi_20,
            shape: (0.5 * (obi_micro + obi_slope)).clamp(-2.0, 2.0),
            micro: obi_micro,
            slope: obi_slope,
        }
    }

    fn compute_probability(
        &self,
        spot: &SpotPrice,
        event: &TrackedEvent,
        now: DateTime<Utc>,
    ) -> Option<(f64, f64, f64)> {
        let sigma = spot
            .volatility(self.cfg.vol_lookback_secs)
            .and_then(|value| value.to_f64())
            .unwrap_or(self.cfg.vol_floor)
            .max(self.cfg.vol_floor);

        let spot_f = spot.price.to_f64()?;
        let beat_f = event.price_to_beat.to_f64()?;
        if spot_f <= 0.0 || beat_f <= 0.0 {
            return None;
        }

        let remaining_secs = (event.end_time - now).num_seconds().max(0) as f64;
        let tau_scale = (remaining_secs / 300.0).max(PROB_FLOOR);
        let d_t = (spot_f / beat_f).ln();
        let z = d_t / (sigma * tau_scale.sqrt()).max(PROB_FLOOR);
        let p_base = normal_cdf(z).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
        Some((p_base, sigma, z))
    }

    fn quote_for_event_side(&self, event: &TrackedEvent, side: Side) -> Option<&Quote> {
        let token_id = match side {
            Side::Up => &event.up_token_id,
            Side::Down => &event.down_token_id,
        };
        self.quotes.get(token_id)
    }

    fn select_side(p_hat: f64) -> (Side, f64) {
        if p_hat >= 0.5 {
            (Side::Up, p_hat)
        } else {
            (Side::Down, 1.0 - p_hat)
        }
    }

    fn kelly_scaled_shares(&self, base_shares: u64, effective_p: f64, effective_cost: f64) -> u64 {
        if !self.cfg.use_kelly_sizing || effective_p <= effective_cost || effective_cost >= 1.0 {
            return base_shares.max(self.cfg.min_shares);
        }

        let full_kelly = ((effective_p - effective_cost) / (1.0 - effective_cost)).max(0.0);
        let fractional = full_kelly * self.cfg.kelly_fraction_scale;
        let multiplier = (fractional / self.cfg.kelly_fraction_cap).clamp(0.0, 1.0);
        let scaled = (base_shares as f64 * multiplier).floor() as u64;
        scaled.max(self.cfg.min_shares)
    }

    fn build_submit_intent(
        &self,
        client_order_id: String,
        symbol: &str,
        token_id: String,
        side: Side,
        shares: u64,
        limit_price: Decimal,
        metadata: HashMap<String, String>,
    ) -> StrategyAction {
        StrategyAction::SubmitIntent {
            intent: StrategyOrderIntent {
                client_order_id,
                domain: Domain::Crypto,
                market_slug: symbol.to_string(),
                token_id,
                side,
                is_buy: true,
                shares,
                limit_price,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::IOC,
                priority: 5,
                metadata,
            },
        }
    }

    fn materialize_position(
        &mut self,
        pending: &PendingOrder,
        cumulative_filled_qty: u64,
        fill_price: Decimal,
        timestamp: DateTime<Utc>,
    ) -> bool {
        if cumulative_filled_qty == 0 {
            return false;
        }

        let end_time = self
            .active_events
            .get(&pending.event_id)
            .map(|event| event.end_time)
            .unwrap_or(timestamp + chrono::Duration::seconds(300));
        let is_new = !self.positions.contains_key(&pending.token_id);
        self.positions
            .entry(pending.token_id.clone())
            .and_modify(|position| {
                position.shares = cumulative_filled_qty;
                position.entry_price = fill_price;
                position.current_price = Some(fill_price);
                position.end_time = end_time;
            })
            .or_insert_with(|| DirectionalPosition {
                event_id: pending.event_id.clone(),
                token_id: pending.token_id.clone(),
                symbol: pending.symbol.clone(),
                side: pending.side,
                shares: cumulative_filled_qty,
                entry_price: fill_price,
                current_price: Some(fill_price),
                opened_at: timestamp,
                end_time,
            });
        is_new
    }

    fn evaluate_symbol(&mut self, symbol: &str, now: DateTime<Utc>) -> Option<Vec<StrategyAction>> {
        if !self.enabled {
            return None;
        }
        if self.daily_limit_reached(now) {
            self.last_reason = Some(format!("{symbol}:daily_limit"));
            return None;
        }
        if self.in_cooldown(symbol, now) {
            self.last_reason = Some(format!("{symbol}:cooldown"));
            return None;
        }
        if self.has_open_symbol_risk(symbol) {
            self.last_reason = Some(format!("{symbol}:already_active"));
            return None;
        }

        let event = self.best_event_for(symbol, now)?.clone();
        let remaining_secs = (event.end_time - now).num_seconds();
        if remaining_secs <= self.cfg.final_no_entry_secs as i64 {
            self.last_reason = Some(format!("{symbol}:final_no_entry"));
            return None;
        }

        let spot = self.spot_prices.get(symbol)?.clone();
        let l2 = self.l2_by_symbol.get(symbol)?.clone();
        if (now - l2.timestamp).num_seconds() > self.cfg.max_l2_age_secs as i64 {
            self.last_reason = Some(format!("{symbol}:stale_l2"));
            return None;
        }

        let flow_2s = self.signed_flow_2s(symbol, now).unwrap_or(0.0);
        let (p_base, sigma, z) = self.compute_probability(&spot, &event, now)?;
        let logit_base = (p_base / (1.0 - p_base)).ln();
        let microgap = Self::microgap_proxy(&l2);
        let obi = Self::obi_factors(&l2);
        let adjusted_logit = logit_base
            + self.cfg.obi_weight * obi.main
            + self.cfg.obi_shape_weight * obi.shape
            + self.cfg.flow_weight * flow_2s
            + self.cfg.microgap_weight * microgap;
        let p_hat = (1.0 / (1.0 + (-adjusted_logit).exp())).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
        let (side, effective_p) = Self::select_side(p_hat);

        if effective_p < self.cfg.p_entry || z.abs() < self.cfg.min_abs_z {
            self.last_reason = Some(format!("{symbol}:weak_probability"));
            return None;
        }

        match side {
            Side::Up => {
                if obi.main < self.cfg.min_obi
                    || flow_2s < self.cfg.min_flow_2s
                    || microgap < 0.0
                {
                    self.last_reason = Some(format!("{symbol}:up_confirmation_failed"));
                    return None;
                }
            }
            Side::Down => {
                if obi.main > -self.cfg.min_obi
                    || flow_2s > -self.cfg.min_flow_2s
                    || microgap > 0.0
                {
                    self.last_reason = Some(format!("{symbol}:down_confirmation_failed"));
                    return None;
                }
            }
        }

        let quote = self.quote_for_event_side(&event, side)?.to_owned();
        let ask = quote.best_ask?;
        let bid = quote.best_bid?;
        let ask_size = quote.ask_size.unwrap_or(Decimal::ZERO);
        if ask > self.cfg.max_entry_price {
            self.last_reason = Some(format!("{symbol}:ask_too_high"));
            return None;
        }
        if ask_size < self.cfg.min_pm_ask_size {
            self.last_reason = Some(format!("{symbol}:pm_ask_size"));
            return None;
        }
        if ask - bid > self.cfg.max_pm_spread {
            self.last_reason = Some(format!("{symbol}:pm_spread"));
            return None;
        }

        let in_no_trade_zone =
            ask >= self.cfg.no_trade_price_min && ask <= self.cfg.no_trade_price_max;
        if in_no_trade_zone
            && !(z.abs() >= self.cfg.no_trade_override_z
                && flow_2s.abs() >= self.cfg.no_trade_override_flow)
        {
            self.last_reason = Some(format!("{symbol}:no_trade_zone"));
            return None;
        }

        let depth_ratio = if ask_size > Decimal::ZERO {
            Decimal::from(self.cfg.shares_per_trade) / ask_size
        } else {
            Decimal::ONE
        };
        let costs = self.fee_model.all_in_cost(ask, bid, ask, depth_ratio);
        let effective_cost = ask.to_f64().unwrap_or(0.5) + costs.total.to_f64().unwrap_or(0.0);
        let edge = effective_p - effective_cost;
        if edge < self.cfg.min_edge {
            self.last_reason = Some(format!("{symbol}:edge_below_threshold"));
            return None;
        }

        let shares =
            self.kelly_scaled_shares(self.cfg.shares_per_trade, effective_p, effective_cost);
        if shares < self.cfg.min_shares {
            self.last_reason = Some(format!("{symbol}:shares_below_min"));
            return None;
        }

        let token_id = match side {
            Side::Up => event.up_token_id.clone(),
            Side::Down => event.down_token_id.clone(),
        };
        let client_order_id = format!(
            "{}_{}_{}_{}",
            self.id,
            symbol.to_ascii_lowercase(),
            side.as_str().to_ascii_lowercase(),
            now.timestamp_millis()
        );
        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrder {
                symbol: symbol.to_string(),
                token_id: token_id.clone(),
                side,
                shares,
                event_id: event.event_id.clone(),
            },
        );
        self.last_reason = Some(format!("{symbol}:submit"));

        let mut metadata = HashMap::new();
        metadata.insert("strategy".to_string(), STRATEGY_NAME.to_string());
        metadata.insert("signal_type".to_string(), "directional_entry".to_string());
        metadata.insert("event_id".to_string(), event.event_id);
        metadata.insert("p_base".to_string(), format!("{p_base:.6}"));
        metadata.insert("p_hat".to_string(), format!("{p_hat:.6}"));
        metadata.insert("p_up".to_string(), format!("{p_hat:.6}"));
        metadata.insert("effective_p".to_string(), format!("{effective_p:.6}"));
        metadata.insert("sigma".to_string(), format!("{sigma:.6}"));
        metadata.insert("z".to_string(), format!("{z:.6}"));
        metadata.insert("obi_1".to_string(), format!("{:.6}", l2.obi_1));
        metadata.insert("obi_3".to_string(), format!("{:.6}", l2.obi_3));
        metadata.insert("obi_5".to_string(), format!("{:.6}", l2.obi_5));
        metadata.insert("obi_10".to_string(), format!("{:.6}", l2.obi_10));
        metadata.insert("obi_20".to_string(), format!("{:.6}", l2.obi_20));
        metadata.insert("obi_weighted".to_string(), format!("{:.6}", obi.main));
        metadata.insert("obi_shape".to_string(), format!("{:.6}", obi.shape));
        metadata.insert("obi_micro".to_string(), format!("{:.6}", obi.micro));
        metadata.insert("obi_slope".to_string(), format!("{:.6}", obi.slope));
        metadata.insert("lob_obi_5".to_string(), format!("{:.6}", l2.obi_5));
        metadata.insert("lob_obi_10".to_string(), format!("{:.6}", obi.obi_10));
        metadata.insert("lob_obi_20".to_string(), format!("{:.6}", obi.obi_20));
        metadata.insert("lob_obi_micro".to_string(), format!("{:.6}", obi.micro));
        metadata.insert("lob_obi_slope".to_string(), format!("{:.6}", obi.slope));
        metadata.insert("lob_obi_shape".to_string(), format!("{:.6}", obi.shape));
        metadata.insert(
            "lob_spread_bps".to_string(),
            format!("{:.6}", l2.spread_bps),
        );
        metadata.insert(
            "lob_bid_volume_5".to_string(),
            format!("{:.6}", l2.bid_volume_5),
        );
        metadata.insert(
            "lob_ask_volume_5".to_string(),
            format!("{:.6}", l2.ask_volume_5),
        );
        metadata.insert("flow_2s".to_string(), format!("{flow_2s:.6}"));
        metadata.insert("microgap_proxy".to_string(), format!("{microgap:.6}"));
        metadata.insert("edge".to_string(), format!("{edge:.6}"));
        metadata.insert("dry_run".to_string(), self.dry_run.to_string());

        Some(vec![
            StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::EntryTriggered,
                    format!(
                        "{STRATEGY_NAME} {} {} p_hat={:.3} z={:.3} edge={:.3}",
                        symbol, side, effective_p, z, edge
                    ),
                ),
            },
            self.build_submit_intent(
                client_order_id,
                symbol,
                token_id,
                side,
                shares,
                ask,
                metadata,
            ),
        ])
    }

    fn prune_expired_state(&mut self, now: DateTime<Utc>) {
        self.active_events.retain(|_, event| event.end_time > now);
        self.recent_ticks.retain(|_, ticks| {
            while ticks
                .front()
                .map(|tick| tick.timestamp < now - chrono::Duration::seconds(5))
                .unwrap_or(false)
            {
                let _ = ticks.pop_front();
            }
            !ticks.is_empty()
        });
        self.pending_orders
            .retain(|_, pending| self.active_events.contains_key(&pending.event_id));
        self.positions.retain(|_, position| position.end_time > now);
    }
}

#[async_trait]
impl Strategy for Pm5mDirectionalStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Polymarket 5m directional engine using Binance direction and Polymarket cost gates"
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
        let mut actions = Vec::new();

        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => {
                if !self.symbols.iter().any(|tracked| tracked == symbol) {
                    return Ok(actions);
                }

                self.spot_prices
                    .entry(symbol.clone())
                    .and_modify(|spot| spot.update(*price, None, *timestamp))
                    .or_insert_with(|| SpotPrice::new(*price, None, *timestamp));
                self.update_tick_history(symbol, *price, *timestamp);

                if let Some(mut entry_actions) = self.evaluate_symbol(symbol, *timestamp) {
                    actions.append(&mut entry_actions);
                }
            }
            MarketUpdate::BinanceL2 {
                symbol,
                obi_1,
                obi_2,
                obi_3,
                obi_5,
                obi_10,
                obi_20,
                spread_bps,
                bid_volume_5,
                ask_volume_5,
                timestamp,
                ..
            } => {
                if self.symbols.iter().any(|tracked| tracked == symbol) {
                    self.l2_by_symbol.insert(
                        symbol.clone(),
                        BinanceL2State {
                            obi_1: *obi_1,
                            obi_2: *obi_2,
                            obi_3: *obi_3,
                            obi_5: *obi_5,
                            obi_10: *obi_10,
                            obi_20: *obi_20,
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
                if let Some(position) = self.positions.get_mut(token_id) {
                    position.current_price = quote.best_bid.or(quote.best_ask);
                }
            }
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                price_to_beat,
                ..
            } => {
                if horizon_for_series(series_id) != "5m" {
                    return Ok(actions);
                }
                let Some(symbol) = self
                    .symbols
                    .iter()
                    .find(|tracked| series_ids_for_symbol(tracked).contains(series_id))
                    .cloned()
                else {
                    return Ok(actions);
                };
                let Some(price_to_beat) = *price_to_beat else {
                    return Ok(actions);
                };

                self.active_events.insert(
                    event_id.clone(),
                    TrackedEvent {
                        event_id: event_id.clone(),
                        symbol,
                        up_token_id: up_token.clone(),
                        down_token_id: down_token.clone(),
                        end_time: *end_time,
                        price_to_beat,
                    },
                );
            }
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                self.pending_orders
                    .retain(|_, pending| pending.event_id != *event_id);
                self.positions
                    .retain(|_, position| position.event_id != *event_id);
            }
            MarketUpdate::BinanceKline { .. } => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();
        let order_key = update
            .client_order_id
            .clone()
            .unwrap_or_else(|| update.order_id.clone());

        if let Some(pending) = self.pending_orders.get(&order_key).cloned() {
            match update.status {
                OrderStatus::PartiallyFilled => {
                    let cumulative_filled = update.filled_qty.min(pending.shares);
                    let is_new_position = self.materialize_position(
                        &pending,
                        cumulative_filled,
                        update.avg_fill_price.unwrap_or(dec!(0)),
                        update.timestamp,
                    );
                    if is_new_position {
                        self.daily_trades += 1;
                    }
                    if cumulative_filled > 0 {
                        self.last_trade_at
                            .insert(pending.symbol.clone(), update.timestamp);
                    }
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::OrderFilled,
                            format!(
                                "{STRATEGY_NAME} {} {} partial {} @ {}",
                                pending.symbol,
                                pending.side,
                                cumulative_filled,
                                update.avg_fill_price.unwrap_or(dec!(0))
                            ),
                        ),
                    });
                }
                OrderStatus::Filled => {
                    let fill_price = update.avg_fill_price.unwrap_or(dec!(0));
                    let cumulative_filled = if update.filled_qty > 0 {
                        update.filled_qty
                    } else {
                        pending.shares
                    };
                    let is_new_position = self.materialize_position(
                        &pending,
                        cumulative_filled,
                        fill_price,
                        update.timestamp,
                    );
                    if is_new_position {
                        self.daily_trades += 1;
                    }
                    if cumulative_filled > 0 {
                        self.last_trade_at
                            .insert(pending.symbol.clone(), update.timestamp);
                    }
                    self.pending_orders.remove(&order_key);
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::OrderFilled,
                            format!(
                                "{STRATEGY_NAME} {} {} filled {} @ {}",
                                pending.symbol, pending.side, cumulative_filled, fill_price
                            ),
                        ),
                    });
                }
                OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Failed
                | OrderStatus::Expired => {
                    let partial_fill_qty = update.filled_qty.min(pending.shares);
                    let is_new_position = self.materialize_position(
                        &pending,
                        partial_fill_qty,
                        update.avg_fill_price.unwrap_or(dec!(0)),
                        update.timestamp,
                    );
                    if is_new_position {
                        self.daily_trades += 1;
                    }
                    if partial_fill_qty > 0 {
                        self.last_trade_at
                            .insert(pending.symbol.clone(), update.timestamp);
                    }
                    self.pending_orders.remove(&order_key);
                    actions.push(StrategyAction::Alert {
                        level: AlertLevel::Warning,
                        message: format!(
                            "{STRATEGY_NAME} order {} {:?}: {:?}",
                            update.order_id, update.status, update.error
                        ),
                    });
                }
                _ => {}
            }
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.prune_expired_state(now);
        Ok(Vec::new())
    }

    fn state(&self) -> StrategyStateInfo {
        let total_exposure = self
            .positions
            .values()
            .map(|position| position.entry_price * Decimal::from(position.shares))
            .sum();
        let unrealized_pnl = self
            .positions
            .values()
            .map(|position| {
                position
                    .current_price
                    .map(|price| (price - position.entry_price) * Decimal::from(position.shares))
                    .unwrap_or(Decimal::ZERO)
            })
            .sum();
        let mut metrics = HashMap::new();
        metrics.insert("symbols".to_string(), self.symbols.join(","));
        metrics.insert(
            "active_events".to_string(),
            self.active_events.len().to_string(),
        );
        metrics.insert(
            "pending_orders".to_string(),
            self.pending_orders.len().to_string(),
        );
        if let Some(reason) = &self.last_reason {
            metrics.insert("last_reason".to_string(), reason.clone());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "running".to_string(),
            enabled: self.enabled,
            active: self.is_active(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure,
            unrealized_pnl,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions
            .values()
            .map(|position| {
                let mut info = PositionInfo::new(
                    position.token_id.clone(),
                    position.side,
                    position.shares,
                    position.entry_price,
                    self.id.clone(),
                );
                info.opened_at = position.opened_at;
                if let Some(price) = position.current_price {
                    info.update_price(price);
                }
                info.metadata
                    .insert("symbol".to_string(), position.symbol.clone());
                info.metadata
                    .insert("event_id".to_string(), position.event_id.clone());
                info
            })
            .collect()
    }

    fn is_active(&self) -> bool {
        !self.positions.is_empty() || !self.pending_orders.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(vec![StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                format!("{STRATEGY_NAME} shutdown"),
            ),
        }])
    }

    fn reset(&mut self) {
        self.spot_prices.clear();
        self.l2_by_symbol.clear();
        self.quotes.clear();
        self.active_events.clear();
        self.recent_ticks.clear();
        self.pending_orders.clear();
        self.positions.clear();
        self.last_trade_at.clear();
        self.daily_trades = 0;
        self.last_reset = Utc::now();
        self.last_reason = None;
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
name = "pm_5m_directional"
enabled = true

[pm_5m_directional]
symbols = ["BTCUSDT"]
shares_per_trade = 25
min_time_remaining_secs = 15
max_time_remaining_secs = 300
"#
    }

    fn no_trade_toml() -> &'static str {
        r#"
[strategy]
name = "pm_5m_directional"
enabled = true

[pm_5m_directional]
symbols = ["BTCUSDT"]
shares_per_trade = 25
min_time_remaining_secs = 15
max_time_remaining_secs = 300
no_trade_override_z = 99.0
no_trade_override_flow = 99.0
"#
    }

    fn event_update(now: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::EventDiscovered {
            event_id: "evt-btc-5m".to_string(),
            series_id: "10684".to_string(),
            up_token: "up-token".to_string(),
            down_token: "down-token".to_string(),
            end_time: now + chrono::Duration::seconds(180),
            price_to_beat: Some(dec!(100)),
            title: Some("BTC 5m".to_string()),
            condition_id: None,
        }
    }

    fn up_quote(ts: DateTime<Utc>, ask: Decimal, bid: Decimal) -> MarketUpdate {
        MarketUpdate::PolymarketQuote {
            token_id: "up-token".to_string(),
            side: Side::Up,
            quote: Quote {
                side: Side::Up,
                best_bid: Some(bid),
                best_ask: Some(ask),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: ts,
            },
            timestamp: ts,
        }
    }

    fn down_quote(ts: DateTime<Utc>, ask: Decimal, bid: Decimal) -> MarketUpdate {
        MarketUpdate::PolymarketQuote {
            token_id: "down-token".to_string(),
            side: Side::Down,
            quote: Quote {
                side: Side::Down,
                best_bid: Some(bid),
                best_ask: Some(ask),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                timestamp: ts,
            },
            timestamp: ts,
        }
    }

    fn l2_update_full(
        ts: DateTime<Utc>,
        obi_1: Decimal,
        obi_2: Decimal,
        obi_3: Decimal,
        obi_5: Decimal,
        obi_10: Decimal,
        obi_20: Decimal,
        spread_bps: Decimal,
    ) -> MarketUpdate {
        MarketUpdate::BinanceL2 {
            symbol: "BTCUSDT".to_string(),
            obi_1,
            obi_2,
            obi_3,
            obi_5,
            obi_10,
            obi_20,
            bid_volume_5: dec!(1200),
            ask_volume_5: dec!(800),
            spread_bps,
            timestamp: ts,
        }
    }

    fn l2_update(ts: DateTime<Utc>, obi_1: Decimal, obi_3: Decimal, spread_bps: Decimal) -> MarketUpdate {
        l2_update_full(ts, obi_1, obi_1, obi_3, obi_3, obi_3, obi_3, spread_bps)
    }

    fn price_update(ts: DateTime<Utc>, price: Decimal) -> MarketUpdate {
        MarketUpdate::BinancePrice {
            symbol: "BTCUSDT".to_string(),
            price,
            timestamp: ts,
        }
    }

    #[test]
    fn from_toml_builds_expected_feeds() {
        let strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let feeds = strategy.required_feeds();
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::BinanceSpot { symbols } if symbols == &vec!["BTCUSDT".to_string()]
        )));
        assert!(feeds.iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids } if series_ids == &vec!["10684".to_string()]
        )));
    }

    #[tokio::test]
    async fn emits_ioc_entry_when_directional_core_passes() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        for second in 0..30 {
            let ts = start + chrono::Duration::seconds(second);
            let price = dec!(100.05) + Decimal::new(second as i64, 3);
            strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("history price");
        }

        let anchor = start + chrono::Duration::seconds(31);
        strategy
            .on_market_update(&event_update(anchor))
            .await
            .expect("event");
        strategy
            .on_market_update(&up_quote(anchor, dec!(0.38), dec!(0.36)))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&down_quote(anchor, dec!(0.66), dec!(0.64)))
            .await
            .expect("down quote");
        strategy
            .on_market_update(&l2_update_full(
                anchor,
                dec!(0.22),
                dec!(0.21),
                dec!(0.20),
                dec!(0.19),
                dec!(0.16),
                dec!(0.12),
                dec!(2.0),
            ))
            .await
            .expect("l2");

        let mut submit_metadata = None;
        for step in 0..8 {
            let ts = anchor + chrono::Duration::milliseconds((step as i64) * 250);
            let price = dec!(100.180) + Decimal::new(step as i64, 3);
            let actions = strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("price");
            if let Some(metadata) = actions.iter().find_map(|action| match action {
                StrategyAction::SubmitIntent { intent }
                    if intent.token_id == "up-token"
                        && intent.time_in_force == TimeInForce::IOC =>
                {
                    Some(intent.metadata.clone())
                }
                _ => None,
            }) {
                submit_metadata = Some(metadata);
                break;
            }
        }

        let metadata = submit_metadata.expect("expected IOC submit intent for up side");
        assert!(metadata.contains_key("obi_3"));
        assert!(metadata.contains_key("obi_5"));
        assert!(metadata.contains_key("obi_10"));
        assert!(metadata.contains_key("obi_20"));
        assert!(metadata.contains_key("obi_weighted"));
        assert!(metadata.contains_key("obi_shape"));
        assert!(metadata.contains_key("obi_slope"));
        assert!(metadata.contains_key("p_up"));
        assert!(metadata.contains_key("lob_obi_5"));
        assert!(metadata.contains_key("lob_obi_10"));
        assert!(metadata.contains_key("lob_obi_20"));
        assert!(metadata.contains_key("lob_obi_micro"));
        assert!(metadata.contains_key("lob_obi_slope"));
        assert!(metadata.contains_key("lob_obi_shape"));
    }

    #[tokio::test]
    async fn no_trade_zone_blocks_weak_mid_price_entry() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), no_trade_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        for second in 0..30 {
            let ts = start + chrono::Duration::seconds(second);
            let price = dec!(100.10) + Decimal::new(second as i64, 3);
            strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("history price");
        }

        let anchor = start + chrono::Duration::seconds(31);
        strategy
            .on_market_update(&event_update(anchor))
            .await
            .expect("event");
        strategy
            .on_market_update(&up_quote(anchor, dec!(0.50), dec!(0.49)))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&down_quote(anchor, dec!(0.51), dec!(0.50)))
            .await
            .expect("down quote");
        strategy
            .on_market_update(&l2_update(anchor, dec!(0.06), dec!(0.05), dec!(2.0)))
            .await
            .expect("l2");

        let pre_sequence = [
            dec!(100.180),
            dec!(100.185),
            dec!(100.190),
            dec!(100.195),
            dec!(100.200),
            dec!(100.205),
            dec!(100.210),
            dec!(100.215),
        ];
        for (idx, price) in pre_sequence.into_iter().enumerate() {
            strategy
                .on_market_update(&price_update(
                    anchor + chrono::Duration::milliseconds((idx as i64) * 250),
                    price,
                ))
                .await
                .expect("pre-sequence price");
        }

        let actions = strategy
            .on_market_update(&price_update(
                anchor + chrono::Duration::seconds(2),
                dec!(100.220),
            ))
            .await
            .expect("final price");
        let submit_count = actions
            .iter()
            .filter(|action| matches!(action, StrategyAction::SubmitIntent { .. }))
            .count();

        assert_eq!(submit_count, 0, "mid-band weak setup should be filtered");
        assert!(
            matches!(
                strategy.last_reason.as_deref(),
                Some("BTCUSDT:no_trade_zone") | Some("BTCUSDT:up_confirmation_failed")
            ),
            "expected mid-band filter or confirmation failure, got {:?}",
            strategy.last_reason
        );
    }

    #[tokio::test]
    async fn far_book_divergence_blocks_entry_even_with_positive_obi_3() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        for second in 0..30 {
            let ts = start + chrono::Duration::seconds(second);
            let price = dec!(100.05) + Decimal::new(second as i64, 3);
            strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("history price");
        }

        let anchor = start + chrono::Duration::seconds(31);
        strategy
            .on_market_update(&event_update(anchor))
            .await
            .expect("event");
        strategy
            .on_market_update(&up_quote(anchor, dec!(0.38), dec!(0.36)))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&down_quote(anchor, dec!(0.66), dec!(0.64)))
            .await
            .expect("down quote");
        strategy
            .on_market_update(&l2_update_full(
                anchor,
                dec!(0.24),
                dec!(0.22),
                dec!(0.18),
                dec!(0.08),
                dec!(-0.02),
                dec!(-0.18),
                dec!(2.0),
            ))
            .await
            .expect("l2");

        let mut saw_submit = false;
        for step in 0..8 {
            let ts = anchor + chrono::Duration::milliseconds((step as i64) * 250);
            let price = dec!(100.180) + Decimal::new(step as i64, 3);
            let actions = strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("price");
            if actions.iter().any(|action| matches!(action, StrategyAction::SubmitIntent { .. })) {
                saw_submit = true;
                break;
            }
        }

        assert!(!saw_submit, "far-book divergence should block the entry");
        assert_eq!(
            strategy.last_reason.as_deref(),
            Some("BTCUSDT:up_confirmation_failed")
        );
    }

    #[tokio::test]
    async fn terminal_partial_fill_creates_position() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event");
        strategy.pending_orders.insert(
            "partial-order".to_string(),
            PendingOrder {
                symbol: "BTCUSDT".to_string(),
                token_id: "up-token".to_string(),
                side: Side::Up,
                shares: 25,
                event_id: "evt-btc-5m".to_string(),
            },
        );

        let actions = strategy
            .on_order_update(&OrderUpdate {
                order_id: "exchange-order".to_string(),
                client_order_id: Some("partial-order".to_string()),
                status: OrderStatus::Expired,
                filled_qty: 7,
                avg_fill_price: Some(dec!(0.41)),
                timestamp: start + chrono::Duration::seconds(1),
                error: Some("IOC remainder cancelled".to_string()),
            })
            .await
            .expect("order update");

        let position = strategy.positions.get("up-token").expect("position");
        assert_eq!(position.shares, 7);
        assert_eq!(position.entry_price, dec!(0.41));
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, StrategyAction::Alert { .. })),
            "terminal partial fill should still alert on the cancelled remainder"
        );
    }

    #[tokio::test]
    async fn partial_fill_keeps_pending_order_and_marks_strategy_active() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event");
        strategy.pending_orders.insert(
            "partial-order".to_string(),
            PendingOrder {
                symbol: "BTCUSDT".to_string(),
                token_id: "up-token".to_string(),
                side: Side::Up,
                shares: 25,
                event_id: "evt-btc-5m".to_string(),
            },
        );

        strategy
            .on_order_update(&OrderUpdate {
                order_id: "exchange-order".to_string(),
                client_order_id: Some("partial-order".to_string()),
                status: OrderStatus::PartiallyFilled,
                filled_qty: 7,
                avg_fill_price: Some(dec!(0.41)),
                timestamp: start + chrono::Duration::milliseconds(250),
                error: None,
            })
            .await
            .expect("order update");

        assert!(strategy.pending_orders.contains_key("partial-order"));
        assert!(strategy.positions.contains_key("up-token"));
        assert!(strategy.is_active());
    }

    #[tokio::test]
    async fn tick_does_not_drop_position_before_event_end() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        strategy
            .on_market_update(&event_update(start))
            .await
            .expect("event");
        strategy.positions.insert(
            "up-token".to_string(),
            DirectionalPosition {
                event_id: "evt-btc-5m".to_string(),
                token_id: "up-token".to_string(),
                symbol: "BTCUSDT".to_string(),
                side: Side::Up,
                shares: 10,
                entry_price: dec!(0.40),
                current_price: Some(dec!(0.45)),
                opened_at: start,
                end_time: start + chrono::Duration::seconds(180),
            },
        );

        strategy
            .on_tick(start + chrono::Duration::seconds(170))
            .await
            .expect("tick");

        assert!(strategy.positions.contains_key("up-token"));
        assert!(strategy.is_active());
    }

    #[tokio::test]
    async fn state_and_positions_report_unrealized_pnl() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        let start = Utc::now();

        strategy.positions.insert(
            "up-token".to_string(),
            DirectionalPosition {
                event_id: "evt-btc-5m".to_string(),
                token_id: "up-token".to_string(),
                symbol: "BTCUSDT".to_string(),
                side: Side::Up,
                shares: 10,
                entry_price: dec!(0.40),
                current_price: Some(dec!(0.45)),
                opened_at: start,
                end_time: start + chrono::Duration::seconds(180),
            },
        );

        let state = strategy.state();
        let positions = strategy.positions();

        assert_eq!(state.unrealized_pnl, dec!(0.50));
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].unrealized_pnl, dec!(0.50));
    }
}
