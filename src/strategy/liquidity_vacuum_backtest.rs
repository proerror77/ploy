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
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, trace};

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

#[derive(Debug, Clone)]
struct CommonSignalState {
    spot_price: Decimal,
    price_move: Decimal,
    volume_ratio: Decimal,
    flow_component: Decimal,
    deviation_abs: Decimal,
    deviation_zscore: Option<Decimal>,
    liquidity_depth: Option<u64>,
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

    fn compute_common_signal_state(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
    ) -> Option<CommonSignalState> {
        let state = self.symbol_state.get_mut(symbol)?;
        state.prune_old(
            ts,
            self.config.window_secs,
            self.config.volume_baseline_samples,
        );
        state.reset_daily_counter_if_needed(ts);

        let ema = state.ema.warm_value()?;
        let expected_price = ema * (Decimal::ONE + self.config.sentiment_offset);
        if expected_price <= Decimal::ZERO {
            return None;
        }
        let signed_deviation = (state.spot.price - expected_price) / expected_price;
        let deviation_abs = signed_deviation.abs();
        let deviation_zscore =
            state.record_deviation_sample(signed_deviation, self.config.zscore_lookback_samples);

        let price_move = state.spot.momentum(self.config.window_secs)?.abs();

        let _current_vol = state.maybe_sample_volume_window(
            ts,
            self.config.window_secs,
            self.config.volume_baseline_samples,
        );
        let volume_ratio = state.volume_ratio()?;
        let flow_component = state.flow_component(ts, self.config.window_secs)?;

        Some(CommonSignalState {
            spot_price: state.spot.price,
            price_move,
            volume_ratio,
            flow_component,
            deviation_abs,
            deviation_zscore,
            liquidity_depth: state.latest_lob_depth,
        })
    }

    fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let windows = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => return,
        };

        let common = match self.compute_common_signal_state(symbol, ts) {
            Some(s) => s,
            None => return,
        };

        if common.price_move <= self.config.price_move_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "price_move_below_threshold",
            );
            return;
        }

        if common.volume_ratio <= self.config.volume_multiplier_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(common.volume_ratio),
                Some(common.spot_price),
                None,
                "volume_ratio_below_threshold",
            );
            return;
        }

        for window in windows {
            self.try_entry_for_window(symbol, &window, &common, ts);
        }
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        window: &ActiveWindowInfo,
        common: &CommonSignalState,
        ts: DateTime<Utc>,
    ) {
        let quote = match self.quotes_by_event.get(&window.event_slug) {
            Some(q) => q.clone(),
            None => return,
        };

        let (up_ask, down_ask, up_ts, down_ts) =
            match (quote.up_ask, quote.down_ask, quote.up_ts, quote.down_ts) {
                (Some(u), Some(d), Some(ut), Some(dt)) => (u, d, ut, dt),
                _ => return,
            };

        if !is_valid_binary_quote_price(up_ask) || !is_valid_binary_quote_price(down_ask) {
            let invalid_px = if !is_valid_binary_quote_price(up_ask) {
                up_ask
            } else {
                down_ask
            };
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                Some(invalid_px),
                "invalid_binary_quote_bounds",
            );
            return;
        }

        let up_age_ms = (ts - up_ts).num_milliseconds();
        let down_age_ms = (ts - down_ts).num_milliseconds();
        if up_age_ms > self.config.max_quote_age_ms || down_age_ms > self.config.max_quote_age_ms {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "stale_quote",
            );
            return;
        }

        let time_remaining = (window.end_time - ts).num_seconds();
        if time_remaining <= self.config.force_exit_before_resolution_secs as i64 {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "too_close_to_resolution",
            );
            return;
        }

        let ask_sum = up_ask + down_ask;
        if ask_sum <= Decimal::ZERO {
            return;
        }

        let spread_proxy_bps = ((ask_sum - Decimal::ONE).abs() * dec!(10000))
            .to_u32()
            .unwrap_or(0);
        if spread_proxy_bps > self.config.max_spread_bps {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(Decimal::from(spread_proxy_bps)),
                Some(common.spot_price),
                None,
                "spread_proxy_too_wide",
            );
            return;
        }

        if let Some(depth) = common.liquidity_depth {
            if depth < self.config.min_liquidity_shares {
                self.record_filtered(
                    symbol,
                    "",
                    ts,
                    Some(Decimal::from(depth)),
                    Some(common.spot_price),
                    None,
                    "insufficient_liquidity",
                );
                return;
            }
        }

        let book_skew = (up_ask - down_ask) / (up_ask + down_ask);
        let crowd_vote =
            self.config.flow_weight * common.flow_component + self.config.book_weight * book_skew;
        if crowd_vote.abs() < self.config.order_concentration_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(crowd_vote),
                Some(common.spot_price),
                None,
                "crowd_vote_below_threshold",
            );
            return;
        }

        let deviation = common.deviation_abs;
        if self.config.entry_zscore_threshold > Decimal::ZERO {
            let zscore = match common.deviation_zscore {
                Some(z) => z,
                None => {
                    self.record_filtered(
                        symbol,
                        "",
                        ts,
                        None,
                        Some(common.spot_price),
                        None,
                        "zscore_unavailable",
                    );
                    return;
                }
            };

            if zscore <= self.config.entry_zscore_threshold {
                self.record_filtered(
                    symbol,
                    "",
                    ts,
                    Some(zscore),
                    Some(common.spot_price),
                    None,
                    "zscore_below_threshold",
                );
                return;
            }
        } else if deviation <= self.config.entry_deviation_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(deviation),
                Some(common.spot_price),
                None,
                "deviation_below_threshold",
            );
            return;
        }

        let direction = if crowd_vote > Decimal::ZERO {
            Direction::Down
        } else {
            Direction::Up
        };
        let entry_price = match direction {
            Direction::Up => up_ask,
            Direction::Down => down_ask,
        };
        let fair_up_prob = fair_up_probability_from_spot(common.spot_price, window.s0);
        let fair_price = match direction {
            Direction::Up => fair_up_prob,
            Direction::Down => Decimal::ONE - fair_up_prob,
        };
        let expected_edge = fair_price - entry_price;
        let estimated_roundtrip_fee =
            self.fee_model.fee_shares(Decimal::ONE, entry_price) * entry_price * dec!(2);
        let min_required_edge = estimated_roundtrip_fee + self.config.min_edge_buffer;
        if expected_edge <= min_required_edge {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                Some(expected_edge),
                Some(common.spot_price),
                Some(entry_price),
                "edge_below_cost",
            );
            return;
        }

        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                self.record_filtered(
                    symbol,
                    &format!("{direction}"),
                    ts,
                    None,
                    Some(common.spot_price),
                    Some(entry_price),
                    "cooldown",
                );
                return;
            }
        }

        if self.positions.len() >= self.config.max_concurrent_positions {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "max_positions",
            );
            return;
        }

        if self
            .positions
            .iter()
            .any(|p| p.event_slug == window.event_slug && p.direction == direction)
        {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "already_holding",
            );
            return;
        }

        if let Some(state) = self.symbol_state.get_mut(symbol) {
            state.reset_daily_counter_if_needed(ts);
            if self.config.max_daily_trades > 0
                && state.daily_trade_count >= self.config.max_daily_trades
            {
                self.record_filtered(
                    symbol,
                    &format!("{direction}"),
                    ts,
                    None,
                    Some(common.spot_price),
                    Some(entry_price),
                    "max_daily_trades",
                );
                return;
            }
        }

        let depth_for_fill = common.liquidity_depth.unwrap_or(10_000);
        let sim = self.execution_sim.simulate_buy(
            entry_price,
            ts,
            self.config.shares_per_trade,
            depth_for_fill,
        );
        if sim.filled_shares == 0 {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "no_fill",
            );
            return;
        }

        let entry_cost = Decimal::from(sim.filled_shares) * sim.fill_price;
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(sim.filled_shares), sim.fill_price)
            * sim.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
            trace!(
                "Skipping entry: insufficient equity {} < {}",
                self.equity,
                total_entry_cost
            );
            return;
        }

        self.equity -= total_entry_cost;
        self.last_entry_time.insert(symbol.to_string(), ts);
        if let Some(state) = self.symbol_state.get_mut(symbol) {
            state.daily_trade_count += 1;
        }

        self.positions.push(LiquidityVacuumPosition {
            symbol: symbol.to_string(),
            event_slug: window.event_slug.clone(),
            direction,
            entry_price: sim.fill_price,
            entry_time: ts,
            shares: sim.filled_shares,
            event_end_time: window.end_time,
            latest_pm_price: entry_price,
            entry_crowd_vote: crowd_vote,
            entry_deviation: deviation,
            s0: window.s0,
        });

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{direction}"),
            timestamp: ts,
            p_hat: Some(
                ((crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            ev_net: Some(deviation.to_f64().unwrap_or(0.0)),
            sigma: Some(crowd_vote.to_f64().unwrap_or(0.0)),
            market_price: Some(sim.fill_price),
            spot_price: Some(common.spot_price),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining as f64),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "ENTRY {} {} @ {:.4} vote={:.3} dev={:.2}%",
            symbol,
            direction,
            sim.fill_price,
            crowd_vote,
            deviation * dec!(100)
        );
    }

    fn check_exits(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let (spot, depth, deviation_zscore) = match self.symbol_state.get(symbol) {
            Some(s) => (
                s.spot.price,
                s.latest_lob_depth.unwrap_or(10_000),
                s.latest_deviation_zscore(),
            ),
            None => return,
        };

        let mut to_close: Vec<(usize, Decimal, &'static str)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol {
                continue;
            }

            let mark = pos.latest_pm_price;
            if pos.entry_price <= Decimal::ZERO {
                continue;
            }

            let held_secs = (ts - pos.entry_time).num_seconds();
            let min_hold_secs = dynamic_min_hold_secs(pos.entry_time, pos.event_end_time);
            let can_take_profit_exit = held_secs >= min_hold_secs;
            let pnl_pct = (mark - pos.entry_price) / pos.entry_price;
            if pnl_pct <= -self.config.stop_loss_pct {
                to_close.push((i, mark, "stop_loss"));
                continue;
            }

            if self.config.stop_loss_zscore_threshold > Decimal::ZERO {
                if let Some(z) = deviation_zscore {
                    if z >= self.config.stop_loss_zscore_threshold {
                        to_close.push((i, mark, "stop_loss_zscore"));
                        continue;
                    }
                }
            }

            if can_take_profit_exit {
                if self.config.take_profit_zscore_threshold > Decimal::ZERO {
                    if let Some(z) = deviation_zscore {
                        if z <= self.config.take_profit_zscore_threshold {
                            to_close.push((i, mark, "take_profit_zscore"));
                            continue;
                        }
                    }
                }

                // Legacy config name retained for CLI compatibility:
                // take_profit_ema_band_pct now acts as target return threshold.
                if self.config.take_profit_ema_band_pct > Decimal::ZERO {
                    if pnl_pct >= self.config.take_profit_ema_band_pct {
                        to_close.push((i, mark, "take_profit_pnl_target"));
                        continue;
                    }
                }
            }

            if self.config.max_holding_secs > 0 {
                if held_secs >= self.config.max_holding_secs as i64 {
                    to_close.push((i, mark, "max_hold"));
                    continue;
                }
            }

            let remaining = (pos.event_end_time - ts).num_seconds();
            if remaining <= self.config.force_exit_before_resolution_secs as i64 {
                to_close.push((i, mark, "force_exit"));
                continue;
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, reason) in to_close {
            self.close_position(idx, exit_price, reason, ts, depth);
        }
    }

    fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        let mut to_close = Vec::new();
        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol == symbol && pos.event_slug == event_slug {
                let exit_price = match (&pos.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((i, exit_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, px) in to_close {
            self.close_position(idx, px, "settlement", ts, 10_000);
        }
    }

    fn close_position(
        &mut self,
        idx: usize,
        exit_price: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
        liquidity: u64,
    ) {
        let pos = self.positions.remove(idx);

        let (final_price, proceeds) = if reason == "settlement" {
            (exit_price, exit_price * Decimal::from(pos.shares))
        } else {
            let sim = self
                .execution_sim
                .simulate_sell(exit_price, ts, pos.shares, liquidity);
            let gross = Decimal::from(sim.filled_shares) * sim.fill_price;
            let sell_fee = self
                .fee_model
                .fee_shares(Decimal::from(sim.filled_shares), sim.fill_price)
                * sim.fill_price;
            (sim.fill_price, gross - sell_fee)
        };

        self.equity += proceeds;

        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(pos.shares), pos.entry_price)
            * pos.entry_price;
        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price - entry_fee;
        let holding_secs = (ts - pos.entry_time).num_seconds();
        let won = pnl > Decimal::ZERO;

        self.closed_trades.push(LiquidityVacuumClosedTrade {
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares,
            pnl,
            won,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_crowd_vote: pos.entry_crowd_vote,
            entry_deviation: pos.entry_deviation,
            s0: pos.s0,
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            timestamp: ts,
            p_hat: Some(
                ((pos.entry_crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            ev_net: Some(pos.entry_deviation.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_crowd_vote.to_f64().unwrap_or(0.0)),
            market_price: Some(final_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(final_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol: pos.symbol,
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares as i32,
            pnl,
            won,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(
                ((pos.entry_crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            entry_ev_net: Some(pos.entry_deviation.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_crowd_vote.to_f64().unwrap_or(0.0)),
            s0: Some(pos.s0),
        });
    }

    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or_else(Utc::now);
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let price = self.positions[idx].latest_pm_price;
            self.close_position(idx, price, "data_exhausted", ts, 10_000);
        }
    }

    fn record_filtered(
        &mut self,
        symbol: &str,
        direction: &str,
        ts: DateTime<Utc>,
        metric: Option<Decimal>,
        spot_price: Option<Decimal>,
        market_price: Option<Decimal>,
        reason: &str,
    ) {
        self.recorder.record_filtered(
            &BacktestSignal {
                signal_type: SignalType::Filtered,
                symbol: symbol.to_string(),
                direction: direction.to_string(),
                timestamp: ts,
                p_hat: None,
                ev_net: metric.and_then(|v| v.to_f64()),
                sigma: None,
                market_price,
                spot_price,
                s0: None,
                time_remaining_secs: None,
                filter_reason: Some(reason.to_string()),
                exit_reason: None,
                exit_price: None,
            },
            reason,
        );
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

    fn build_results(&self) -> BacktestResults {
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total.saturating_sub(winning);
        let total_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();
        let win_rate = if total > 0 {
            winning as f64 / total as f64
        } else {
            0.0
        };
        let avg_pnl = if total > 0 {
            total_pnl / Decimal::from(total)
        } else {
            Decimal::ZERO
        };

        let wins: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.pnl)
            .collect();
        let losses: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.pnl)
            .collect();

        let avg_win = if wins.is_empty() {
            Decimal::ZERO
        } else {
            wins.iter().copied().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().copied().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };
        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().copied().sum();
        let total_losses_abs: Decimal = losses.iter().map(|v| v.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding_time_secs = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|t| Decimal::from(t.shares) * t.entry_price)
            .sum();

        BacktestResults {
            start_time: self.data_range_start.unwrap_or_else(Utc::now),
            end_time: self.data_range_end.unwrap_or_else(Utc::now),
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: self.calculate_sharpe(),
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: self.equity_curve.clone(),
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        if self.closed_trades.len() < 2 {
            return 0.0;
        }
        let pnls: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|t| t.pnl.to_f64().unwrap_or(0.0))
            .collect();
        let n = pnls.len() as f64;
        let mean = pnls.iter().sum::<f64>() / n;
        let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std = variance.sqrt();
        if std < 1e-10 {
            return 0.0;
        }
        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std) * trades_per_year.sqrt()
    }

    pub fn print_liquidity_vacuum_summary(&self) {
        if self.closed_trades.is_empty() {
            info!("No liquidity-vacuum trades to summarize.");
            return;
        }

        let total = self.closed_trades.len();
        let sl_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "stop_loss" || t.exit_reason == "stop_loss_zscore")
            .count();
        let tp_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "take_profit" || t.exit_reason == "take_profit_zscore")
            .count();
        let max_hold_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "max_hold")
            .count();
        let settlement_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .count();

        let avg_vote = self
            .closed_trades
            .iter()
            .map(|t| t.entry_crowd_vote.to_f64().unwrap_or(0.0).abs())
            .sum::<f64>()
            / total as f64;
        let avg_dev = self
            .closed_trades
            .iter()
            .map(|t| t.entry_deviation.to_f64().unwrap_or(0.0))
            .sum::<f64>()
            / total as f64;

        info!(
            "Liquidity-vacuum summary: trades={} tp={} sl={} max_hold={} settlement={} avg|vote|={:.3} avg_dev={:.2}%",
            total,
            tp_count,
            sl_count,
            max_hold_count,
            settlement_count,
            avg_vote,
            avg_dev * 100.0
        );
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

fn dynamic_min_hold_secs(entry_time: DateTime<Utc>, event_end_time: DateTime<Utc>) -> i64 {
    let ttl_secs = (event_end_time - entry_time).num_seconds().max(0);
    // 5% of contract life, bounded for short-lived contracts.
    let mut min_hold = ttl_secs / 20;
    if min_hold < 5 {
        min_hold = 5;
    } else if min_hold > 30 {
        min_hold = 30;
    }
    min_hold
}

fn is_valid_binary_quote_price(px: Decimal) -> bool {
    px >= dec!(0.01) && px <= dec!(0.99)
}

impl fmt::Display for LiquidityVacuumClosedTrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} @ {} -> {} pnl={} reason={}",
            self.entry_time,
            self.symbol,
            self.direction,
            self.entry_price,
            self.exit_price,
            self.pnl,
            self.exit_reason
        )
    }
}
