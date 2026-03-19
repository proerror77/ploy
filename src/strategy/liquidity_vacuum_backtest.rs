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

use std::collections::HashMap;

use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{
    BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType,
};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::trace;

#[path = "liquidity_vacuum_backtest/reporting.rs"]
mod reporting;

mod lifecycle;
mod signal_logic;
mod state_support;

use state_support::SymbolState;

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

fn clamp_decimal(v: Decimal, min_v: Decimal, max_v: Decimal) -> Decimal {
    state_support::clamp_decimal(v, min_v, max_v)
}

fn fair_up_probability_from_spot(spot: Decimal, strike: Decimal) -> Decimal {
    state_support::fair_up_probability_from_spot(spot, strike)
}

fn is_valid_binary_quote_price(px: Decimal) -> bool {
    state_support::is_valid_binary_quote_price(px)
}
