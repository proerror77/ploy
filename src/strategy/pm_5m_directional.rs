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
use crate::strategy::pm_5m_bayesian::BayesianPrior;
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
    /// Enable Bayesian posterior gate (default: true)
    use_bayesian: bool,
    /// z-score for Bayesian lower-bound credible interval (default: 1.645 = 95%)
    bayesian_credible_z: f64,
    /// EWMA lambda for volatility estimation (0.94 = RiskMetrics standard)
    ewma_lambda: f64,
    /// Enable early exit on adverse signal reversal
    enable_early_exit: bool,
    /// Exit when Binance price reverses by this fraction (e.g. 0.003 = 0.3%)
    exit_reversion_pct: f64,
    /// Exit when OBI sign flips against position direction
    exit_obi_flip: bool,
    // ── Route C: Binance perp funding + liquidation ──
    /// Enable funding rate as additional confirmation (Route C)
    use_funding_signal: bool,
    /// Extreme funding rate threshold (e.g. 0.0001 = 0.01%); contrarian signal
    funding_extreme_threshold: f64,
    /// Rolling liquidation window in seconds for cascade detection
    liquidation_window_secs: u64,
    /// Minimum liquidation volume (USD) to count as a cascade signal
    min_liquidation_cascade_usd: f64,
    // ── Route B: Deribit IV regime filter ──
    /// Enable Deribit IV as regime filter (Route B)
    use_deribit_regime: bool,
    /// ATM IV above this → reduce position size (high vol regime)
    deribit_high_vol_threshold: f64,
    /// ATM IV above this → pause trading (extreme vol regime)
    deribit_extreme_vol_threshold: f64,
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
            use_bayesian: true,
            bayesian_credible_z: 1.645,
            ewma_lambda: 0.94,
            enable_early_exit: true,
            exit_reversion_pct: 0.003,
            exit_obi_flip: true,
            use_funding_signal: false,
            funding_extreme_threshold: 0.0001,
            liquidation_window_secs: 120,
            min_liquidation_cascade_usd: 500_000.0,
            use_deribit_regime: false,
            deribit_high_vol_threshold: 0.80,
            deribit_extreme_vol_threshold: 1.20,
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
    obi_3: Decimal,
    spread_bps: Decimal,
    bid_volume_5: Decimal,
    ask_volume_5: Decimal,
    timestamp: DateTime<Utc>,
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
    entry_sigma: f64,
    entry_time_remaining_secs: f64,
    entry_price: Decimal,
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
    /// Sigma at entry time — needed for Bayesian bucket lookup at settlement
    entry_sigma: f64,
    /// Time remaining at entry — needed for Bayesian bucket lookup at settlement
    entry_time_remaining_secs: f64,
    /// Binance spot price at entry — needed for early exit reversion check
    entry_spot: Decimal,
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
    bayesian: BayesianPrior,
    /// EWMA variance per symbol (per-second log returns, not annualized)
    ewma_var: HashMap<String, f64>,
    /// Last spot price per symbol for EWMA return calculation
    ewma_last_price: HashMap<String, f64>,
    /// Latest funding rate per symbol (Route C)
    funding_rate: HashMap<String, f64>,
    /// Rolling liquidation volume per symbol: (timestamp, side, usd_value)
    liquidation_history: HashMap<String, VecDeque<(DateTime<Utc>, Side, f64)>>,
    /// Latest Deribit ATM IV per symbol (Route B)
    deribit_atm_iv: HashMap<String, f64>,
    /// Latest Deribit 25-delta skew per symbol (Route B)
    deribit_skew: HashMap<String, f64>,
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
            bayesian: BayesianPrior::new(),
            ewma_var: HashMap::new(),
            ewma_last_price: HashMap::new(),
            funding_rate: HashMap::new(),
            liquidation_history: HashMap::new(),
            deribit_atm_iv: HashMap::new(),
            deribit_skew: HashMap::new(),
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

    /// Update EWMA variance with a new price observation.
    /// Returns the current annualized sigma estimate.
    fn update_ewma_vol(&mut self, symbol: &str, price_f: f64) -> f64 {
        let lambda = self.cfg.ewma_lambda;
        if let Some(prev) = self.ewma_last_price.get(symbol).copied() {
            if prev > 0.0 {
                let log_ret = (price_f / prev).ln();
                let var = self.ewma_var.entry(symbol.to_string()).or_insert(0.0);
                *var = lambda * *var + (1.0 - lambda) * log_ret * log_ret;
            }
        }
        self.ewma_last_price.insert(symbol.to_string(), price_f);
        self.ewma_sigma_annualized(symbol)
    }

    /// Current annualized sigma from EWMA variance.
    fn ewma_sigma_annualized(&self, symbol: &str) -> f64 {
        // Per-second variance → annualize: σ_annual = sqrt(var_1s * seconds_per_year)
        const SECS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;
        let var = self.ewma_var.get(symbol).copied().unwrap_or(0.0);
        (var * SECS_PER_YEAR).sqrt().max(self.cfg.vol_floor)
    }

    /// Route C: funding rate directional bias.
    /// Returns Some(1.0) for bullish, Some(-1.0) for bearish, None if no signal.
    /// Contrarian: extreme positive funding → bearish (overleveraged longs).
    fn funding_signal(&self, symbol: &str) -> Option<f64> {
        if !self.cfg.use_funding_signal {
            return None;
        }
        let rate = self.funding_rate.get(symbol).copied()?;
        let threshold = self.cfg.funding_extreme_threshold;
        if rate > threshold {
            Some(-1.0) // extreme positive funding → contrarian bearish
        } else if rate < -threshold {
            Some(1.0) // extreme negative funding → contrarian bullish
        } else {
            None // neutral zone, no signal
        }
    }

    /// Route C: rolling liquidation cascade signal.
    /// Returns net signed liquidation volume (positive = more longs liquidated = bearish).
    fn liquidation_cascade(&self, symbol: &str, now: DateTime<Utc>) -> f64 {
        let history = match self.liquidation_history.get(symbol) {
            Some(h) => h,
            None => return 0.0,
        };
        let cutoff = now - chrono::Duration::seconds(self.cfg.liquidation_window_secs as i64);
        let mut long_liq = 0.0f64;
        let mut short_liq = 0.0f64;
        for (ts, side, usd) in history.iter() {
            if *ts >= cutoff {
                match side {
                    Side::Up => long_liq += usd,   // long liquidation
                    Side::Down => short_liq += usd, // short liquidation
                }
            }
        }
        // Positive = more long liquidations (bearish pressure)
        long_liq - short_liq
    }

    /// Route B: Deribit regime position size multiplier.
    /// Returns 1.0 (normal), 0.5 (high vol), 0.0 (extreme vol → pause).
    fn deribit_size_multiplier(&self, symbol: &str) -> f64 {
        if !self.cfg.use_deribit_regime {
            return 1.0;
        }
        let atm_iv = match self.deribit_atm_iv.get(symbol).copied() {
            Some(iv) => iv,
            None => return 1.0, // no data, don't restrict
        };
        if atm_iv >= self.cfg.deribit_extreme_vol_threshold {
            0.0 // pause trading
        } else if atm_iv >= self.cfg.deribit_high_vol_threshold {
            0.5 // reduce size
        } else {
            1.0 // normal
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

    fn compute_probability(
        &self,
        spot: &SpotPrice,
        event: &TrackedEvent,
        now: DateTime<Utc>,
    ) -> Option<(f64, f64, f64)> {
        // Use EWMA annualized vol; fall back to rolling vol if EWMA not yet warm
        let symbol = &event.symbol;
        let sigma_annual = {
            let ewma = self.ewma_sigma_annualized(symbol);
            if ewma > self.cfg.vol_floor {
                ewma
            } else {
                // Fallback: annualize the old rolling vol
                // rolling vol is per-second std dev, annualize it
                const SECS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;
                spot.volatility(self.cfg.vol_lookback_secs)
                    .and_then(|value| value.to_f64())
                    .map(|v| (v * v * SECS_PER_YEAR).sqrt())
                    .unwrap_or(self.cfg.vol_floor)
                    .max(self.cfg.vol_floor)
            }
        };

        let spot_f = spot.price.to_f64()?;
        let beat_f = event.price_to_beat.to_f64()?;
        if spot_f <= 0.0 || beat_f <= 0.0 {
            return None;
        }

        let remaining_secs = (event.end_time - now).num_seconds().max(0) as f64;
        // Correct: annualized time to expiry
        let tau_years = remaining_secs / (365.25 * 24.0 * 3600.0);
        let d_t = (spot_f / beat_f).ln();
        let z = d_t / (sigma_annual * tau_years.sqrt()).max(PROB_FLOOR);
        let p_base = normal_cdf(z).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
        Some((p_base, sigma_annual, z))
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
        entry_sigma: f64,
        entry_time_remaining_secs: f64,
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
        let entry_spot = self
            .spot_prices
            .get(&pending.symbol)
            .map(|s| s.price)
            .unwrap_or(Decimal::ZERO);
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
                entry_sigma,
                entry_time_remaining_secs,
                entry_spot,
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
        let adjusted_logit = logit_base
            + self.cfg.obi_weight * l2.obi_3.to_f64().unwrap_or(0.0)
            + self.cfg.flow_weight * flow_2s
            + self.cfg.microgap_weight * microgap;
        let p_hat = (1.0 / (1.0 + (-adjusted_logit).exp())).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
        let (side, effective_p) = Self::select_side(p_hat);

        if effective_p < self.cfg.p_entry || z.abs() < self.cfg.min_abs_z {
            self.last_reason = Some(format!("{symbol}:weak_probability"));
            return None;
        }

        let obi_3 = l2.obi_3.to_f64().unwrap_or(0.0);
        match side {
            Side::Up => {
                if obi_3 < self.cfg.min_obi || flow_2s < self.cfg.min_flow_2s || microgap < 0.0 {
                    self.last_reason = Some(format!("{symbol}:up_confirmation_failed"));
                    return None;
                }
            }
            Side::Down => {
                if obi_3 > -self.cfg.min_obi || flow_2s > -self.cfg.min_flow_2s || microgap > 0.0 {
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

        // Bayesian gate: require posterior lower bound >= p_entry
        // This prevents overconfidence when the bucket has few observations.
        let remaining_secs_f = (event.end_time - now).num_seconds().max(0) as f64;
        let ask_f = ask.to_f64().unwrap_or(0.5);
        let bayes_lb = if self.cfg.use_bayesian {
            self.bayesian.posterior_lower_bound(
                ask_f,
                remaining_secs_f,
                sigma,
                effective_p,
                self.cfg.bayesian_credible_z,
            )
        } else {
            effective_p
        };
        if bayes_lb < self.cfg.p_entry {
            self.last_reason = Some(format!("{symbol}:bayesian_lb_below_threshold"));
            return None;
        }

        // Route B: Deribit regime filter — pause or reduce in high-vol environments
        let deribit_mult = self.deribit_size_multiplier(symbol);
        if deribit_mult == 0.0 {
            self.last_reason = Some(format!("{symbol}:deribit_extreme_vol"));
            return None;
        }

        // Route C: funding rate confirmation (optional, contrarian)
        if self.cfg.use_funding_signal {
            if let Some(funding_dir) = self.funding_signal(symbol) {
                let side_dir = match side { Side::Up => 1.0, Side::Down => -1.0 };
                if funding_dir != side_dir {
                    // Funding signal contradicts our direction — skip
                    self.last_reason = Some(format!("{symbol}:funding_signal_conflict"));
                    return None;
                }
            }
        }

        let shares =
            self.kelly_scaled_shares(self.cfg.shares_per_trade, effective_p, effective_cost);
        // Apply Deribit regime multiplier to position size
        let shares = ((shares as f64 * deribit_mult).floor() as u64).max(0);
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
                entry_sigma: sigma,
                entry_time_remaining_secs: remaining_secs_f,
                entry_price: ask,
            },
        );
        self.last_reason = Some(format!("{symbol}:submit"));

        let mut metadata = HashMap::new();
        metadata.insert("strategy".to_string(), STRATEGY_NAME.to_string());
        metadata.insert("signal_type".to_string(), "directional_entry".to_string());
        metadata.insert("event_id".to_string(), event.event_id);
        metadata.insert("p_base".to_string(), format!("{p_base:.6}"));
        metadata.insert("p_hat".to_string(), format!("{p_hat:.6}"));
        metadata.insert("effective_p".to_string(), format!("{effective_p:.6}"));
        metadata.insert("sigma".to_string(), format!("{sigma:.6}"));
        metadata.insert("z".to_string(), format!("{z:.6}"));
        metadata.insert("obi_3".to_string(), format!("{obi_3:.6}"));
        metadata.insert("flow_2s".to_string(), format!("{flow_2s:.6}"));
        metadata.insert("microgap_proxy".to_string(), format!("{microgap:.6}"));
        metadata.insert("edge".to_string(), format!("{edge:.6}"));
        metadata.insert("bayes_lb".to_string(), format!("{bayes_lb:.6}"));
        metadata.insert(
            "bayes_obs".to_string(),
            self.bayesian
                .bucket_obs(ask_f, remaining_secs_f, sigma)
                .to_string(),
        );
        metadata.insert("dry_run".to_string(), self.dry_run.to_string());
        // Route B/C signals
        metadata.insert("deribit_mult".to_string(), format!("{deribit_mult:.2}"));
        if let Some(fr) = self.funding_rate.get(symbol) {
            metadata.insert("funding_rate".to_string(), format!("{fr:.6}"));
        }
        let liq_cascade = self.liquidation_cascade(symbol, now);
        if liq_cascade.abs() > 0.0 {
            metadata.insert("liq_cascade_usd".to_string(), format!("{liq_cascade:.0}"));
        }

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

    /// Check if any open position for this symbol should be exited early.
    fn check_early_exit(
        &self,
        symbol: &str,
        current_spot: Decimal,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        let positions_to_exit: Vec<_> = self
            .positions
            .values()
            .filter(|p| p.symbol == symbol)
            .filter(|p| {
                if p.entry_spot.is_zero() {
                    return false;
                }
                let spot_f = current_spot.to_f64().unwrap_or(0.0);
                let entry_f = p.entry_spot.to_f64().unwrap_or(0.0);
                if entry_f == 0.0 {
                    return false;
                }
                let pct_change = (spot_f - entry_f) / entry_f;

                // Price reversion check
                let price_reversed = match p.side {
                    Side::Up => pct_change < -self.cfg.exit_reversion_pct,
                    Side::Down => pct_change > self.cfg.exit_reversion_pct,
                };

                // OBI flip check
                let obi_flipped = if self.cfg.exit_obi_flip {
                    self.l2_by_symbol.get(symbol).map_or(false, |l2| {
                        let obi = l2.obi_3.to_f64().unwrap_or(0.0);
                        match p.side {
                            Side::Up => obi < -self.cfg.min_obi as f64,
                            Side::Down => obi > self.cfg.min_obi as f64,
                        }
                    })
                } else {
                    false
                };

                price_reversed || obi_flipped
            })
            .cloned()
            .collect();

        if positions_to_exit.is_empty() {
            return None;
        }

        let mut actions = Vec::new();
        for pos in &positions_to_exit {
            // Sell at best bid via IOC
            let bid = self
                .quotes
                .get(&pos.token_id)
                .and_then(|q| q.best_bid)
                .unwrap_or(dec!(0.01));

            let client_order_id = format!(
                "{}_exit_{}_{}",
                self.id,
                pos.symbol.to_ascii_lowercase(),
                now.timestamp_millis()
            );

            let mut metadata = HashMap::new();
            metadata.insert("strategy".to_string(), STRATEGY_NAME.to_string());
            metadata.insert("signal_type".to_string(), "early_exit".to_string());
            metadata.insert("event_id".to_string(), pos.event_id.clone());

            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::ExitTriggered,
                    format!(
                        "{STRATEGY_NAME} early_exit {} {} shares={} bid={}",
                        pos.symbol, pos.side, pos.shares, bid
                    ),
                ),
            });
            actions.push(StrategyAction::SubmitIntent {
                intent: StrategyOrderIntent {
                    client_order_id,
                    domain: Domain::Crypto,
                    market_slug: pos.symbol.clone(),
                    token_id: pos.token_id.clone(),
                    side: pos.side,
                    is_buy: false,
                    shares: pos.shares,
                    limit_price: bid,
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::IOC,
                    priority: 8,
                    metadata,
                },
            });
        }
        Some(actions)
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

                // Update EWMA vol estimate
                if let Some(price_f) = price.to_f64() {
                    self.update_ewma_vol(symbol, price_f);
                }

                // Check early exit for open positions
                if self.cfg.enable_early_exit {
                    if let Some(exit_actions) = self.check_early_exit(symbol, *price, *timestamp) {
                        actions.extend(exit_actions);
                    }
                }

                if let Some(mut entry_actions) = self.evaluate_symbol(symbol, *timestamp) {
                    actions.append(&mut entry_actions);
                }
            }
            MarketUpdate::BinanceL2 {
                symbol,
                obi_1,
                obi_3,
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
                            obi_3: *obi_3,
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
                // Record Bayesian outcomes using actual spot vs price_to_beat
                // comparison, not stale PM quotes which may not reflect settlement.
                let settled: Vec<_> = self
                    .positions
                    .values()
                    .filter(|p| p.event_id == *event_id)
                    .filter_map(|p| {
                        let event = self.active_events.get(event_id)?;
                        let spot = self.spot_prices.get(&p.symbol)?;
                        let spot_f = spot.price.to_f64()?;
                        let beat_f = event.price_to_beat.to_f64()?;
                        // Actual outcome: did spot finish above price_to_beat?
                        let spot_above = spot_f > beat_f;
                        let won = match p.side {
                            Side::Up => spot_above,
                            Side::Down => !spot_above,
                        };
                        let entry_price_f = p.entry_price.to_f64().unwrap_or(0.5);
                        Some((entry_price_f, p.entry_time_remaining_secs, p.entry_sigma, won))
                    })
                    .collect();
                for (price, time_rem, sigma, won) in settled {
                    self.bayesian.record_outcome(price, time_rem, sigma, won);
                }

                self.active_events.remove(event_id);
                self.pending_orders
                    .retain(|_, pending| pending.event_id != *event_id);
                self.positions
                    .retain(|_, position| position.event_id != *event_id);
            }
            MarketUpdate::BinanceKline { .. } => {}
            MarketUpdate::BinanceFunding {
                symbol,
                funding_rate,
                timestamp: _,
                ..
            } => {
                if self.symbols.iter().any(|s| s == symbol) {
                    self.funding_rate.insert(symbol.clone(), *funding_rate);
                }
            }
            MarketUpdate::BinanceLiquidation {
                symbol,
                side,
                qty,
                price,
                timestamp,
            } => {
                if self.symbols.iter().any(|s| s == symbol) {
                    let usd_value = qty.to_f64().unwrap_or(0.0) * price.to_f64().unwrap_or(0.0);
                    let history = self.liquidation_history.entry(symbol.clone()).or_default();
                    history.push_back((*timestamp, *side, usd_value));
                    // Prune old entries
                    let cutoff = *timestamp - chrono::Duration::seconds(self.cfg.liquidation_window_secs as i64 * 2);
                    while history.front().map(|(ts, _, _)| *ts < cutoff).unwrap_or(false) {
                        history.pop_front();
                    }
                }
            }
            MarketUpdate::DeribitIV {
                symbol,
                atm_iv,
                skew_25d,
                timestamp: _,
                ..
            } => {
                if self.symbols.iter().any(|s| s == symbol) {
                    self.deribit_atm_iv.insert(symbol.clone(), *atm_iv);
                    self.deribit_skew.insert(symbol.clone(), *skew_25d);
                }
            }
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
                        pending.entry_sigma,
                        pending.entry_time_remaining_secs,
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
                        pending.entry_sigma,
                        pending.entry_time_remaining_secs,
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
                        pending.entry_sigma,
                        pending.entry_time_remaining_secs,
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
        metrics.insert(
            "bayes_total_obs".to_string(),
            self.bayesian.total_observations().to_string(),
        );
        metrics.insert(
            "bayes_mature_buckets".to_string(),
            self.bayesian.mature_buckets().len().to_string(),
        );

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

    fn nav_kelly_toml() -> &'static str {
        r#"
[strategy]
name = "pm_5m_directional"
enabled = true

[pm_5m_directional]
symbols = ["BTCUSDT"]
shares_per_trade = 500
min_shares = 1
min_time_remaining_secs = 15
max_time_remaining_secs = 300
kelly_fraction_scale = 1.0
kelly_fraction_cap = 0.25
initial_nav_usd = 1000
max_nav_fraction_per_trade = 0.01
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

    fn l2_update(
        ts: DateTime<Utc>,
        obi_1: Decimal,
        obi_3: Decimal,
        spread_bps: Decimal,
    ) -> MarketUpdate {
        MarketUpdate::BinanceL2 {
            symbol: "BTCUSDT".to_string(),
            obi_1,
            obi_2: obi_1,
            obi_3,
            obi_5: obi_3,
            obi_10: obi_3,
            obi_20: obi_3,
            bid_volume_5: dec!(1200),
            ask_volume_5: dec!(800),
            spread_bps,
            timestamp: ts,
        }
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
            .on_market_update(&l2_update(anchor, dec!(0.20), dec!(0.18), dec!(2.0)))
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
            if actions.iter().any(|action| {
                matches!(
                    action,
                    StrategyAction::SubmitIntent { intent }
                        if intent.token_id == "up-token" && intent.time_in_force == TimeInForce::IOC
                )
            }) {
                saw_submit = true;
                break;
            }
        }

        assert!(saw_submit, "expected IOC submit intent for up side");
    }

    #[tokio::test]
    async fn nav_based_kelly_sizing_uses_current_nav_cap() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), nav_kelly_toml(), true)
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
            .on_market_update(&up_quote(anchor, dec!(0.20), dec!(0.19)))
            .await
            .expect("up quote");
        strategy
            .on_market_update(&down_quote(anchor, dec!(0.82), dec!(0.81)))
            .await
            .expect("down quote");
        strategy
            .on_market_update(&l2_update(anchor, dec!(0.30), dec!(0.25), dec!(1.0)))
            .await
            .expect("l2");

        let mut submitted_shares = None;
        for step in 0..8 {
            let ts = anchor + chrono::Duration::milliseconds((step as i64) * 250);
            let price = dec!(100.220) + Decimal::new(step as i64, 3);
            let actions = strategy
                .on_market_update(&price_update(ts, price))
                .await
                .expect("price");
            for action in actions {
                if let StrategyAction::SubmitIntent { intent } = action {
                    submitted_shares = Some(intent.shares);
                    break;
                }
            }
            if submitted_shares.is_some() {
                break;
            }
        }

        assert_eq!(submitted_shares, Some(50));
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
            dec!(100.190),
            dec!(100.200),
            dec!(100.190),
            dec!(100.200),
            dec!(100.190),
            dec!(100.200),
            dec!(100.190),
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
                dec!(100.200),
            ))
            .await
            .expect("final price");
        let submit_count = actions
            .iter()
            .filter(|action| matches!(action, StrategyAction::SubmitIntent { .. }))
            .count();

        assert_eq!(submit_count, 0, "mid-band weak setup should be filtered");
        assert_eq!(
            strategy.last_reason.as_deref(),
            Some("BTCUSDT:no_trade_zone")
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

    #[tokio::test]
    async fn event_expiry_realizes_marked_pnl_into_strategy_state() {
        let mut strategy =
            Pm5mDirectionalStrategy::from_toml("pm5-test".to_string(), nav_kelly_toml(), true)
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
                current_price: Some(dec!(0.75)),
                opened_at: start,
                end_time: start + chrono::Duration::seconds(180),
            },
        );
        strategy.active_events.insert(
            "evt-btc-5m".to_string(),
            TrackedEvent {
                event_id: "evt-btc-5m".to_string(),
                symbol: "BTCUSDT".to_string(),
                up_token_id: "up-token".to_string(),
                down_token_id: "down-token".to_string(),
                end_time: start + chrono::Duration::seconds(180),
                price_to_beat: dec!(100),
            },
        );

        strategy
            .on_market_update(&MarketUpdate::EventExpired {
                event_id: "evt-btc-5m".to_string(),
            })
            .await
            .expect("event expired");

        let state = strategy.state();
        assert_eq!(state.realized_pnl_today, dec!(3.50));
        assert!(strategy.positions.is_empty());
    }
}
