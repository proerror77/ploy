//! Liquidity Vacuum backtest engine.
//!
//! Strategy intent:
//! - Detect short-term panic/crowded flow regimes.
//! - Enter against crowd direction when dislocation from EMA is extreme.
//! - Exit on EMA-band mean reversion or hard stop.
//!
//! Note:
//! Historical feed currently lacks full taker-side aggressor flow for Polymarket.
//! This engine uses deterministic proxy signals from quote deltas + side skew so
//! replay can run on existing DB datasets.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{
    BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType,
};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;

#[path = "liquidity_vacuum_backtest/reporting.rs"]
mod reporting;

mod lifecycle;
mod signal_logic;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityVacuumBacktestConfig {
    pub symbols: Vec<String>,
    pub initial_capital: Decimal,
    pub shares_per_trade: u64,
    pub max_concurrent_positions: usize,
    pub cooldown_secs: u64,
    pub max_daily_trades: u32,
    pub window_secs: u64,
    pub ema_period: u64,
    pub sentiment_offset: Decimal,
    pub price_move_threshold: Decimal,
    pub volume_multiplier_threshold: Decimal,
    pub order_concentration_threshold: Decimal,
    pub entry_deviation_threshold: Decimal,
    pub entry_zscore_threshold: Decimal,
    pub zscore_lookback_samples: usize,
    pub stop_loss_pct: Decimal,
    pub take_profit_ema_band_pct: Decimal,
    pub take_profit_zscore_threshold: Decimal,
    pub stop_loss_zscore_threshold: Decimal,
    pub max_holding_secs: u64,
    pub force_exit_before_resolution_secs: u64,
    pub max_spread_bps: u32,
    pub min_liquidity_shares: u64,
    pub max_quote_age_ms: i64,
    pub flow_weight: Decimal,
    pub book_weight: Decimal,
    pub flow_scale: Decimal,
    pub volume_baseline_samples: usize,
    pub default_trade_quantity: Decimal,
    pub min_edge_buffer: Decimal,
}

impl Default for LiquidityVacuumBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            cooldown_secs: 60,
            max_daily_trades: 20,
            window_secs: 90,
            ema_period: 200,
            sentiment_offset: Decimal::ZERO,
            price_move_threshold: dec!(0.02),
            volume_multiplier_threshold: dec!(3.0),
            order_concentration_threshold: dec!(0.70),
            entry_deviation_threshold: dec!(0.12),
            entry_zscore_threshold: Decimal::ZERO,
            zscore_lookback_samples: 180,
            stop_loss_pct: dec!(0.25),
            take_profit_ema_band_pct: dec!(0.03),
            take_profit_zscore_threshold: Decimal::ZERO,
            stop_loss_zscore_threshold: Decimal::ZERO,
            max_holding_secs: 0,
            force_exit_before_resolution_secs: 30,
            // PM quote replay often has wider synthetic spread proxy than live best bid/ask spread.
            max_spread_bps: 1500,
            min_liquidity_shares: 1_000,
            max_quote_age_ms: 1500,
            flow_weight: dec!(0.70),
            book_weight: dec!(0.30),
            flow_scale: dec!(0.02),
            volume_baseline_samples: 120,
            default_trade_quantity: dec!(1.0),
            min_edge_buffer: dec!(0.01),
        }
    }
}

impl LiquidityVacuumBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    end_time: DateTime<Utc>,
    s0: Decimal,
}

#[derive(Debug, Clone, Default)]
struct EventQuoteState {
    symbol: String,
    up_ask: Option<Decimal>,
    down_ask: Option<Decimal>,
    up_ts: Option<DateTime<Utc>>,
    down_ts: Option<DateTime<Utc>>,
    prev_up_ask: Option<Decimal>,
    prev_down_ask: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct LiquidityVacuumPosition {
    symbol: String,
    event_slug: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    event_end_time: DateTime<Utc>,
    latest_pm_price: Decimal,
    entry_crowd_vote: Decimal,
    entry_deviation: Decimal,
    s0: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityVacuumClosedTrade {
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
    pub entry_crowd_vote: Decimal,
    pub entry_deviation: Decimal,
    pub s0: Decimal,
}

#[derive(Debug, Clone)]
struct EmaState {
    alpha: Decimal,
    period: u64,
    value: Option<Decimal>,
    samples: u64,
}

impl EmaState {
    fn new(period: u64) -> Self {
        let alpha = Decimal::from(2u64) / Decimal::from(period + 1);
        Self {
            alpha,
            period,
            value: None,
            samples: 0,
        }
    }

    fn update(&mut self, price: Decimal) -> Decimal {
        self.samples += 1;
        let next = match self.value {
            Some(v) => self.alpha * price + (Decimal::ONE - self.alpha) * v,
            None => price,
        };
        self.value = Some(next);
        next
    }

    fn warm_value(&self) -> Option<Decimal> {
        if self.samples >= self.period {
            self.value
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
struct SymbolState {
    spot: SpotPrice,
    ema: EmaState,
    volume_samples: VecDeque<(DateTime<Utc>, Decimal)>,
    volume_window_history: VecDeque<Decimal>,
    flow_samples: VecDeque<(DateTime<Utc>, Decimal)>,
    deviation_samples: VecDeque<Decimal>,
    latest_lob_depth: Option<u64>,
    last_volume_window_ts: Option<DateTime<Utc>>,
    daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
}

impl SymbolState {
    fn new(
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
        ema_period: u64,
        default_qty: Decimal,
    ) -> Self {
        let mut ema = EmaState::new(ema_period);
        ema.update(price);

        let mut volume_samples = VecDeque::new();
        volume_samples.push_back((ts, quantity.unwrap_or(default_qty).max(Decimal::ZERO)));

        Self {
            spot: SpotPrice::new(price, quantity, ts),
            ema,
            volume_samples,
            volume_window_history: VecDeque::new(),
            flow_samples: VecDeque::new(),
            deviation_samples: VecDeque::new(),
            latest_lob_depth: None,
            last_volume_window_ts: None,
            daily_trade_count: 0,
            daily_trade_date: Some(ts.date_naive()),
        }
    }

    fn update_spot(
        &mut self,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
        default_qty: Decimal,
    ) {
        self.spot.update(price, quantity, ts);
        self.ema.update(price);
        self.volume_samples
            .push_back((ts, quantity.unwrap_or(default_qty).max(Decimal::ZERO)));
    }

    fn record_flow_sample(&mut self, ts: DateTime<Utc>, value: Decimal) {
        self.flow_samples.push_back((ts, value));
    }

    fn update_lob_depth(&mut self, depth: u64) {
        self.latest_lob_depth = Some(depth);
    }

    fn prune_old(&mut self, now: DateTime<Utc>, window_secs: u64, baseline_samples: usize) {
        let keep_cutoff = now - chrono::Duration::seconds((window_secs * 4) as i64);

        while let Some((ts, _)) = self.volume_samples.front() {
            if *ts < keep_cutoff {
                let _ = self.volume_samples.pop_front();
            } else {
                break;
            }
        }

        while let Some((ts, _)) = self.flow_samples.front() {
            if *ts < keep_cutoff {
                let _ = self.flow_samples.pop_front();
            } else {
                break;
            }
        }

        while self.volume_window_history.len() > baseline_samples + 2 {
            let _ = self.volume_window_history.pop_front();
        }
    }

    fn volume_in_window(&self, now: DateTime<Utc>, window_secs: u64) -> Decimal {
        let cutoff = now - chrono::Duration::seconds(window_secs as i64);
        self.volume_samples
            .iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .map(|(_, q)| *q)
            .sum()
    }

    fn maybe_sample_volume_window(
        &mut self,
        now: DateTime<Utc>,
        window_secs: u64,
        baseline_samples: usize,
    ) -> Decimal {
        let current = self.volume_in_window(now, window_secs);
        let should_sample = self
            .last_volume_window_ts
            .map(|ts| (now - ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_sample {
            self.volume_window_history.push_back(current);
            self.last_volume_window_ts = Some(now);
            while self.volume_window_history.len() > baseline_samples + 2 {
                let _ = self.volume_window_history.pop_front();
            }
        }
        current
    }

    fn volume_ratio(&self) -> Option<Decimal> {
        if self.volume_window_history.len() < 10 {
            return None;
        }
        let latest = *self.volume_window_history.back()?;
        let baseline_count = self.volume_window_history.len().saturating_sub(1);
        if baseline_count == 0 {
            return None;
        }
        let baseline_sum: Decimal = self
            .volume_window_history
            .iter()
            .take(baseline_count)
            .copied()
            .sum();
        let baseline = baseline_sum / Decimal::from(baseline_count as u64);
        if baseline <= Decimal::ZERO {
            return None;
        }
        Some(latest / baseline)
    }

    fn flow_component(&self, now: DateTime<Utc>, window_secs: u64) -> Option<Decimal> {
        let cutoff = now - chrono::Duration::seconds(window_secs as i64);
        let mut sum = Decimal::ZERO;
        let mut n: u64 = 0;
        for (ts, value) in &self.flow_samples {
            if *ts >= cutoff {
                sum += *value;
                n += 1;
            }
        }
        if n == 0 {
            None
        } else {
            Some(sum / Decimal::from(n))
        }
    }

    fn record_deviation_sample(
        &mut self,
        signed_deviation: Decimal,
        lookback_samples: usize,
    ) -> Option<Decimal> {
        let cap = lookback_samples.max(2);
        self.deviation_samples.push_back(signed_deviation);
        while self.deviation_samples.len() > cap {
            let _ = self.deviation_samples.pop_front();
        }
        self.latest_deviation_zscore()
    }

    fn latest_deviation_zscore(&self) -> Option<Decimal> {
        compute_abs_zscore(&self.deviation_samples)
    }

    fn reset_daily_counter_if_needed(&mut self, now: DateTime<Utc>) {
        let d = now.date_naive();
        if self.daily_trade_date != Some(d) {
            self.daily_trade_count = 0;
            self.daily_trade_date = Some(d);
        }
    }
}

pub struct LiquidityVacuumBacktestEngine {
    config: LiquidityVacuumBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    symbol_state: HashMap<String, SymbolState>,
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    quotes_by_event: HashMap<String, EventQuoteState>,
    positions: Vec<LiquidityVacuumPosition>,
    closed_trades: Vec<LiquidityVacuumClosedTrade>,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    last_logic_ts: HashMap<String, DateTime<Utc>>,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

impl LiquidityVacuumBacktestEngine {
    pub fn new(config: LiquidityVacuumBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            symbol_state: HashMap::new(),
            active_events: HashMap::new(),
            quotes_by_event: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            last_logic_ts: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn new_without_recorder(config: LiquidityVacuumBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
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

            if let Some(events) = self.active_events.get_mut(&update.symbol) {
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
                    if let Some(up_won) = outcome {
                        self.resolve_positions(
                            &update.symbol,
                            event_slug,
                            *up_won,
                            update.timestamp,
                        );
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.quotes_by_event.remove(event_slug);
                    } else if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                        let events = self.active_events.entry(update.symbol.clone()).or_default();
                        if !events.iter().any(|e| e.event_slug == *event_slug) {
                            events.push(ActiveWindowInfo {
                                event_slug: event_slug.clone(),
                                end_time: *end,
                                s0: *s0,
                            });
                        }
                    }
                }
                UpdateType::LobSnapshot {
                    ask_depth_shares, ..
                } => {
                    if let Some(state) = self.symbol_state.get_mut(&update.symbol) {
                        state.update_lob_depth(*ask_depth_shares);
                    }
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
                UpdateType::BinanceL2 { .. } => {
                    self.maybe_run_symbol_logic(&update.symbol, update.timestamp);
                }
            }
        }

        self.close_remaining_positions();
        let _ = self.recorder.flush();
        self.build_results()
    }

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.symbol_state
            .entry(symbol.to_string())
            .and_modify(|s| {
                s.update_spot(price, quantity, ts, self.config.default_trade_quantity);
                s.prune_old(
                    ts,
                    self.config.window_secs,
                    self.config.volume_baseline_samples,
                );
            })
            .or_insert_with(|| {
                SymbolState::new(
                    price,
                    quantity,
                    ts,
                    self.config.ema_period,
                    self.config.default_trade_quantity,
                )
            });

        self.maybe_run_symbol_logic(symbol, ts);
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let valid_best_ask = best_ask.and_then(|ask| {
            if is_valid_binary_quote_price(ask) {
                Some(ask)
            } else {
                trace!(
                    symbol = symbol,
                    event_slug = event_slug,
                    side = ?quote_side,
                    ask = %ask,
                    "dropping_invalid_pm_quote"
                );
                None
            }
        });

        let entry = self
            .quotes_by_event
            .entry(event_slug.to_string())
            .or_insert_with(|| EventQuoteState {
                symbol: symbol.to_string(),
                ..Default::default()
            });
        if entry.symbol.is_empty() {
            entry.symbol = symbol.to_string();
        }

        let mut signed_flow = None::<Decimal>;

        match quote_side {
            Side::Up => {
                if let Some(ask) = valid_best_ask {
                    if let Some(prev) = entry.up_ask {
                        if prev > Decimal::ZERO {
                            let delta_rel = (ask - prev) / prev;
                            signed_flow = Some(delta_rel);
                        }
                    }
                    entry.prev_up_ask = entry.up_ask;
                    entry.up_ask = Some(ask);
                    entry.up_ts = Some(ts);
                }
            }
            Side::Down => {
                if let Some(ask) = valid_best_ask {
                    if let Some(prev) = entry.down_ask {
                        if prev > Decimal::ZERO {
                            let delta_rel = (ask - prev) / prev;
                            // Rising DOWN ask implies crowd is chasing DOWN.
                            signed_flow = Some(-delta_rel);
                        }
                    }
                    entry.prev_down_ask = entry.down_ask;
                    entry.down_ask = Some(ask);
                    entry.down_ts = Some(ts);
                }
            }
        }

        if let Some(flow) = signed_flow {
            if let Some(state) = self.symbol_state.get_mut(symbol) {
                let scaled = clamp_decimal(flow / self.config.flow_scale, dec!(-1), dec!(1));
                state.record_flow_sample(ts, scaled);
                state.prune_old(
                    ts,
                    self.config.window_secs,
                    self.config.volume_baseline_samples,
                );
            }
        }

        for pos in &mut self.positions {
            if pos.event_slug != event_slug || pos.symbol != symbol {
                continue;
            }
            match (pos.direction, quote_side, valid_best_ask) {
                (Direction::Up, Side::Up, Some(px)) => pos.latest_pm_price = px,
                (Direction::Down, Side::Down, Some(px)) => pos.latest_pm_price = px,
                _ => {}
            }
        }

        self.maybe_run_symbol_logic(symbol, ts);
    }

    fn maybe_run_symbol_logic(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let should_run = self
            .last_logic_ts
            .get(symbol)
            .map(|last| (ts - *last).num_seconds() >= 1)
            .unwrap_or(true);
        if !should_run {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        self.try_entry(symbol, ts);
        self.check_exits(symbol, ts);
        self.record_equity(ts);
    }

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

fn compute_abs_zscore(samples: &VecDeque<Decimal>) -> Option<Decimal> {
    if samples.len() < 30 {
        return None;
    }
    let values: Vec<f64> = samples
        .iter()
        .map(|v| v.to_f64())
        .collect::<Option<Vec<_>>>()?;
    let n = values.len() as f64;
    if n <= 1.0 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    if variance <= 1e-12 {
        return None;
    }
    let std = variance.sqrt();
    let latest = *values.last()?;
    Decimal::from_f64(((latest - mean) / std).abs())
}

fn clamp_decimal(v: Decimal, min_v: Decimal, max_v: Decimal) -> Decimal {
    if v < min_v {
        min_v
    } else if v > max_v {
        max_v
    } else {
        v
    }
}

fn fair_up_probability_from_spot(spot: Decimal, strike: Decimal) -> Decimal {
    if strike <= Decimal::ZERO {
        return dec!(0.5);
    }
    let rel_move = (spot - strike) / strike;
    // 2% relative move maps roughly from 0.5 toward an extreme.
    let scale = dec!(0.02);
    let p = dec!(0.5) + rel_move * dec!(0.5) / scale;
    clamp_decimal(p, dec!(0.01), dec!(0.99))
}

fn is_valid_binary_quote_price(px: Decimal) -> bool {
    px >= dec!(0.01) && px <= dec!(0.99)
}
