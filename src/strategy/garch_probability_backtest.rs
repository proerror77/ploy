//! EWMA-GARCH (IGARCH) probability backtest for Polymarket UP/DOWN binaries.
//!
//! Model:
//! - Treat each event as a digital option: pays $1 if ST >= S0 else $0.
//! - Estimate P(ST >= S0) with a log-normal (normal CDF) model:
//!     p_hat = Φ( ln(ST/S0) / (sigma * sqrt(dt)) )
//!   where `sigma` is a 15-minute log-return volatility, and dt = time_remaining / 900.
//! - Estimate `sigma` online using an EWMA variance rate on log returns
//!   (a practical IGARCH/EWMA approximation): var_rate_t = decay*var_rate_{t-1} + (1-decay)*r^2/dt.
//! - Compare fair value (p_hat or 1-p_hat) to PM asks after fee/spread buffers.
//!
//! This is designed to work with the integrated DB replay feed:
//! - spot: `sync_records.bn_mid_price` (preferred) or `binance_price_ticks`
//! - quotes: `clob_quote_ticks`
//! - settlement: `pm_token_settlements`

#[path = "garch_probability_backtest/entry_lifecycle.rs"]
mod entry_lifecycle;
#[path = "garch_probability_backtest/position_lifecycle.rs"]
mod position_lifecycle;
#[path = "garch_probability_backtest/reporting.rs"]
mod reporting;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{BacktestRecorder, NullRecorder};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GarchProbabilityBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent open positions
    pub max_concurrent_positions: usize,
    /// Minimum edge required (fair_value - pm_ask - costs), e.g. 0.03 = 3%
    pub entry_threshold: f64,
    /// Don't buy below this (filters illiquid extremes)
    pub min_entry_price: Decimal,
    /// Don't buy above this (avoid paying too much for near-certain outcomes)
    pub max_entry_price: Decimal,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds)
    pub max_time_remaining_secs: u64,
    /// Drift term (units: log-return per 15m). Keep 0 unless calibrated.
    pub mu: f64,

    // ── Volatility model (EWMA-GARCH / IGARCH) ────────────────────────────
    /// EWMA half-life in seconds (how fast the variance adapts)
    pub ewma_half_life_secs: f64,
    /// Initial 15-minute sigma used before enough observations
    pub initial_sigma_15m: f64,
    /// Floor on 15-minute sigma
    pub vol_floor_15m: f64,
    /// Extra 15-minute sigma to account for Binance↔Chainlink basis/noise (added in quadrature)
    pub basis_sigma_15m: f64,

    // ── Window filter ───────────────────────────────────────────────────
    /// Allowed window durations (seconds). For PM BTC up/down 5m use `[300]`.
    pub allowed_window_durations: Vec<u64>,
    /// Duration tolerance when matching inferred window duration (seconds)
    pub window_duration_tolerance: u64,

    // ── Execution + costs ───────────────────────────────────────────────
    /// Market depth used by execution simulator
    pub market_depth_shares: u64,
    /// Assumed bid/ask spread (price units, e.g. 0.02 = 2¢)
    pub assumed_spread: Decimal,
}

impl Default for GarchProbabilityBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.03,
            min_entry_price: dec!(0.10),
            max_entry_price: dec!(0.85),
            cooldown_secs: 30,
            min_time_remaining_secs: 45,
            max_time_remaining_secs: 240,
            mu: 0.0,
            ewma_half_life_secs: 60.0,
            initial_sigma_15m: 0.005,
            vol_floor_15m: 0.003,
            basis_sigma_15m: 0.0,
            allowed_window_durations: vec![300],
            window_duration_tolerance: 30,
            market_depth_shares: 10_000,
            assumed_spread: dec!(0.02),
        }
    }
}

impl GarchProbabilityBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Internal types
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    s0: Decimal,
    end_time: DateTime<Utc>,
    window_duration_secs: i64,
}

#[derive(Debug, Clone)]
struct OpenPosition {
    symbol: String,
    event_slug: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    s0: Decimal,
    event_end_time: DateTime<Utc>,
    entry_p_hat: f64,
    entry_ev_net: f64,
    entry_sigma_15m: f64,
    latest_pm_price: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GarchProbabilityClosedTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    // diagnostics
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma_15m: f64,
}

#[derive(Debug, Clone)]
struct EwmaGarchVol {
    half_life_secs: f64,
    var_rate_per_sec: f64,
    last_price: Option<f64>,
    last_ts: Option<DateTime<Utc>>,
}

impl EwmaGarchVol {
    fn new(half_life_secs: f64, initial_sigma_15m: f64) -> Self {
        let sigma = initial_sigma_15m.max(1e-9);
        let var_15m = sigma * sigma;
        let var_rate = (var_15m / 900.0).max(1e-18);
        Self {
            half_life_secs: half_life_secs.max(1.0),
            var_rate_per_sec: var_rate,
            last_price: None,
            last_ts: None,
        }
    }

    fn update(&mut self, price: Decimal, ts: DateTime<Utc>) {
        let Some(p) = price.to_f64() else {
            self.last_ts = Some(ts);
            return;
        };
        if !(p.is_finite() && p > 0.0) {
            self.last_ts = Some(ts);
            return;
        }

        if let (Some(prev_p), Some(prev_ts)) = (self.last_price, self.last_ts) {
            let dt_ms = (ts - prev_ts).num_milliseconds();
            if dt_ms > 0 {
                let dt = (dt_ms as f64) / 1000.0;
                let r = (p / prev_p).ln();
                if r.is_finite() && dt.is_finite() && dt > 0.0 {
                    // E[r^2] ≈ var_rate * dt for log returns
                    let obs_var_rate = (r * r) / dt;
                    if obs_var_rate.is_finite() && obs_var_rate >= 0.0 {
                        // decay = 0.5^(dt/half_life)
                        let decay = (0.5f64).powf(dt / self.half_life_secs);
                        let v = decay * self.var_rate_per_sec + (1.0 - decay) * obs_var_rate;
                        if v.is_finite() && v > 0.0 {
                            self.var_rate_per_sec = v.max(1e-18);
                        }
                    }
                }
            }
        }

        self.last_price = Some(p);
        self.last_ts = Some(ts);
    }

    fn sigma_15m(&self) -> f64 {
        (self.var_rate_per_sec * 900.0).sqrt()
    }
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct GarchProbabilityBacktestEngine {
    config: GarchProbabilityBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    vol_models: HashMap<String, EwmaGarchVol>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    // Positions & trades
    positions: Vec<OpenPosition>,
    closed_trades: Vec<GarchProbabilityClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
    // Throttle: last timestamp we ran entry logic per symbol
    last_logic_ts: HashMap<String, DateTime<Utc>>,
}

impl GarchProbabilityBacktestEngine {
    pub fn new(
        config: GarchProbabilityBacktestConfig,
        recorder: Box<dyn BacktestRecorder>,
    ) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            vol_models: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            active_events: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
            last_logic_ts: HashMap::new(),
        }
    }

    pub fn new_without_recorder(config: GarchProbabilityBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &GarchProbabilityBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[GarchProbabilityClosedTrade] {
        &self.closed_trades
    }

    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }

    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            // Prune expired events (keep positions until settlement)
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(
                        &update.symbol,
                        event_slug,
                        *side,
                        *best_ask,
                        update.timestamp,
                    );
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                    }

                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            let duration_secs = (*end - update.timestamp).num_seconds();
                            let allowed = if self.config.allowed_window_durations.is_empty() {
                                true
                            } else {
                                let tol = self.config.window_duration_tolerance as i64;
                                self.config
                                    .allowed_window_durations
                                    .iter()
                                    .any(|&d| (duration_secs - d as i64).abs() <= tol)
                            };
                            if allowed {
                                let events =
                                    self.active_events.entry(update.symbol.clone()).or_default();
                                if !events.iter().any(|e| e.event_slug == *event_slug) {
                                    events.push(ActiveWindowInfo {
                                        event_slug: event_slug.clone(),
                                        s0: *s0,
                                        end_time: *end,
                                        window_duration_secs: duration_secs,
                                    });
                                }
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot { .. } => {
                    // Depth snapshots are optional; this engine uses a fixed depth for sim.
                }
                UpdateType::BinanceL2 { .. } => {
                    // Binance L2 features are ignored by this backtest.
                }
            }
        }

        self.close_remaining_positions();
        let _ = self.recorder.flush();
        self.build_results()
    }

    // ─── Market handlers ────────────────────────────────────

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));

        self.vol_models
            .entry(symbol.to_string())
            .or_insert_with(|| {
                EwmaGarchVol::new(
                    self.config.ewma_half_life_secs,
                    self.config.initial_sigma_15m,
                )
            })
            .update(price, ts);
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update asks per event slug
        let entry = self
            .pm_asks_by_event
            .entry(event_slug.to_string())
            .or_insert((None, None));
        match quote_side {
            Side::Up => {
                if best_ask.is_some() {
                    entry.0 = best_ask;
                }
            }
            Side::Down => {
                if best_ask.is_some() {
                    entry.1 = best_ask;
                }
            }
        }

        // Update mark-to-market
        for pos in &mut self.positions {
            if pos.symbol == symbol && pos.event_slug == event_slug {
                match pos.direction {
                    Direction::Up => {
                        if quote_side == Side::Up {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                    Direction::Down => {
                        if quote_side == Side::Down {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                }
            }
        }

        // Throttle entry logic to once per second per symbol
        let should_run_logic = match self.last_logic_ts.get(symbol) {
            Some(last) => (ts - *last).num_seconds() >= 1,
            None => true,
        };
        if !should_run_logic {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        self.try_entry(symbol, ts);
        self.record_equity(ts);
    }

    // ─── Equity tracking ────────────────────────────────────

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let drawdown = if self.peak_equity > Decimal::ZERO {
            (self.peak_equity - self.equity) / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        let should_record = self
            .equity_curve
            .last()
            .map(|(last_ts, _)| (ts - *last_ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_record {
            self.equity_curve.push((ts, self.equity));
        }
    }
}
