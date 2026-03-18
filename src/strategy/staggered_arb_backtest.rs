//! Staggered Arbitrage Backtest Engine — 時間差套利回測
//!
//! Polymarket binary options have `up_ask + down_ask > 1` at any point (market maker spread).
//! Buying both sides simultaneously always loses. But by using volatility prediction to time
//! entries — buying the side about to get expensive first, then buying the other side after
//! price movement — the total cost of both legs can be < $1, yielding risk-free arbitrage.
//!
//! When both legs are filled, they are immediately merged (redeemed) for $1.00 per share,
//! without waiting for settlement. This dramatically improves capital turnover.
//!
//! Usage:
//!   ploy strategy backtest staggered-arb --symbols BTCUSDT --save --json

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
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
use crate::strategy::gamma_scalping::greeks::{binary_greeks, BinaryGreeks};
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;

#[path = "staggered_arb_backtest/config.rs"]
mod config;
#[path = "staggered_arb_backtest/entry_logic.rs"]
mod entry_logic;
#[path = "staggered_arb_backtest/lifecycle.rs"]
mod lifecycle;
mod reporting;
#[path = "staggered_arb_backtest/runtime.rs"]
mod runtime;

pub use config::StaggeredArbBacktestConfig;

pub(crate) fn polymarket_order_meets_minimum(price: Decimal, shares: u64) -> bool {
    shares >= 5 && price > Decimal::ZERO && price * Decimal::from(shares) >= Decimal::ONE
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PmSideQuoteState {
    pub ask: Option<Decimal>,
    pub ask_size: Option<Decimal>,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl PmSideQuoteState {
    pub(crate) fn clear(&mut self) {
        self.ask = None;
        self.ask_size = None;
        self.first_seen_at = None;
        self.last_seen_at = None;
    }

    pub(crate) fn update(
        &mut self,
        ask: Option<Decimal>,
        ask_size: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let had_quote = self.ask.is_some();
        self.ask = ask;
        self.ask_size = ask_size;
        self.last_seen_at = Some(ts);
        match ask {
            Some(_) if !had_quote => self.first_seen_at = Some(ts),
            Some(_) => {}
            None => {
                self.first_seen_at = None;
                self.ask_size = None;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PmEventQuoteState {
    pub up: PmSideQuoteState,
    pub down: PmSideQuoteState,
}

impl PmEventQuoteState {
    pub(crate) fn side_mut(&mut self, side: Side) -> &mut PmSideQuoteState {
        match side {
            Side::Up => &mut self.up,
            Side::Down => &mut self.down,
        }
    }

    pub(crate) fn update(
        &mut self,
        side: Side,
        ask: Option<Decimal>,
        ask_size: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        match side {
            Side::Up => self.up.update(ask, ask_size, ts),
            Side::Down => self.down.update(ask, ask_size, ts),
        }
    }

    pub(crate) fn asks(&self) -> (Option<Decimal>, Option<Decimal>) {
        (self.up.ask, self.down.ask)
    }

    pub(crate) fn synthetic(
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) -> Self {
        let mut state = Self::default();
        state.up.update(up_ask, None, ts);
        state.down.update(down_ask, None, ts);
        state
    }
}

// ─────────────────────────────────────────────────────────────
// Position state machine
// ─────────────────────────────────────────────────────────────

/// Position lifecycle:
///   Idle → Leg1Filled → Settled (via merge or single-leg settlement)
///                     → Aborted (timeout / stop_loss / time_safety)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArbPositionState {
    Leg1Filled,
    Settled,
    Aborted,
}

impl fmt::Display for ArbPositionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leg1Filled => write!(f, "Leg1Filled"),
            Self::Settled => write!(f, "Settled"),
            Self::Aborted => write!(f, "Aborted"),
        }
    }
}

#[derive(Debug, Clone)]
struct StaggeredArbPosition {
    symbol: String,
    event_slug: String,
    /// Direction of Leg1 (the side we bought first)
    leg1_direction: Direction,
    leg1_price: Decimal,
    leg1_shares: u64,
    leg1_time: DateTime<Utc>,
    leg1_fee: Decimal,
    /// Deadline for Leg2 fill
    wait_deadline: DateTime<Utc>,
    /// Window open price (S0)
    s0: Decimal,
    /// Event end time
    event_end_time: DateTime<Utc>,
    /// Window duration in seconds
    window_duration_secs: i64,
    /// Model probability at Leg1 entry
    entry_p_hat: f64,
    /// Realized vol at entry
    entry_sigma: f64,
    /// Best sum seen during monitoring (for diagnostics)
    best_sum_seen: Decimal,
    /// Initial sum at entry (up_ask + down_ask)
    initial_sum: Decimal,
    /// Binance top-of-book OBI captured at Leg1 entry.
    entry_obi: Option<f64>,
    /// When the position first breached the protective stop-loss threshold.
    protective_stop_armed_at: Option<DateTime<Utc>>,
    // ── Greeks at entry ──
    /// Binary option greeks computed at Leg1 entry
    entry_greeks: Option<BinaryGreeks>,
    /// Current state
    state: ArbPositionState,
    // ── Leg2 (filled after monitoring) ──
    leg2_direction: Option<Direction>,
    leg2_price: Option<Decimal>,
    leg2_shares: Option<u64>,
    leg2_time: Option<DateTime<Utc>>,
    leg2_fee: Option<Decimal>,
    // ── Resolution ──
    exit_reason: Option<String>,
    pnl: Option<Decimal>,
}

/// A closed staggered arb trade for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbClosedTrade {
    pub symbol: String,
    pub leg1_direction: String,
    pub leg1_price: Decimal,
    pub leg1_time: DateTime<Utc>,
    pub leg2_price: Option<Decimal>,
    pub leg2_time: Option<DateTime<Utc>>,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub initial_sum: Decimal,
    pub final_sum: Option<Decimal>,
    pub entry_p_hat: f64,
    pub entry_sigma: f64,
    pub best_sum_seen: Decimal,
    pub s0: Decimal,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    pub window_duration_secs: i64,
    /// Greeks at entry (delta, gamma, theta, vega, fair_value)
    pub entry_delta: Option<f64>,
    pub entry_gamma: Option<f64>,
    pub entry_theta: Option<f64>,
    pub entry_fair_value: Option<f64>,
}
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    s0: Decimal,
    end_time: DateTime<Utc>,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    window_duration_secs: i64,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct StaggeredArbBacktestEngine {
    config: StaggeredArbBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    pm_quote_state_by_event: HashMap<String, PmEventQuoteState>,
    // Active events: symbol -> concurrent windows
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    // Positions & trades
    positions: Vec<StaggeredArbPosition>,
    closed_trades: Vec<StaggeredArbClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    /// Track how many trades have been opened per event slug
    event_trade_count: HashMap<String, usize>,
    /// Track ask-side depth per symbol (from LOB snapshots)
    lob_depth: HashMap<String, u64>,
    /// Latest Binance L2 OBI per symbol for live-parity replay filtering.
    binance_l2_obi_5: HashMap<String, Decimal>,
    /// Previous Binance L2 OBI per symbol for persistence / flip checks.
    binance_l2_obi_prev_5: HashMap<String, Decimal>,
    binance_l2_obi_ts: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

impl StaggeredArbBacktestEngine {
    pub fn new(config: StaggeredArbBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            pm_quote_state_by_event: HashMap::new(),
            active_events: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            event_trade_count: HashMap::new(),
            lob_depth: HashMap::new(),
            binance_l2_obi_5: HashMap::new(),
            binance_l2_obi_prev_5: HashMap::new(),
            binance_l2_obi_ts: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn new_without_recorder(config: StaggeredArbBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &StaggeredArbBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[StaggeredArbClosedTrade] {
        &self.closed_trades
    }

    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }
}
// ─────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "staggered_arb_backtest/tests.rs"]
mod tests;
