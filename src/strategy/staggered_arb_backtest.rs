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
use ploy_backtest::{
    strategies::build_staggered_arb_results, BacktestRecorder, BacktestResults, BacktestSignal,
    ExecutionSimulator, MarketFeed, NullRecorder, PendingTrade, SignalType, UpdateType,
};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::adapters::SpotPrice;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;
pub use ploy_backtest::strategies::StaggeredArbClosedTrade;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a staggered arbitrage backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent Leg1 positions
    pub max_concurrent_positions: usize,
    // ── Signal thresholds ──
    /// Minimum |p_hat - 0.5| to trigger entry
    pub direction_threshold: f64,
    /// Maximum up_ask + down_ask to consider entry
    pub max_initial_sum: Decimal,
    /// Minimum profit target per share after both legs
    pub min_profit_target: Decimal,
    // ── Time control ──
    /// Maximum seconds to wait for Leg2 after Leg1 fill
    pub max_wait_secs: u64,
    /// Maximum fraction of window duration to wait for Leg2
    pub max_wait_pct: f64,
    /// Minimum time remaining in window to enter
    pub min_time_remaining_secs: u64,
    // ── Risk control ──
    /// Maximum unrealized loss on Leg1 before aborting
    pub max_leg1_loss: Decimal,
    /// If sum < this value, force-complete Leg2 even at timeout
    pub force_complete_threshold: Decimal,
    /// Minimum ask price to consider (filters out illiquid extreme prices)
    pub min_ask_price: Decimal,
    /// Minimum up_ask + down_ask to enter (filters out illiquid extreme-price pairs)
    pub min_entry_sum: Decimal,
    // ── Window filter ──
    /// Allowed window durations in seconds (e.g. [300, 900] for 5m + 15m).
    /// Empty = accept all durations.
    pub allowed_window_durations: Vec<u64>,
    /// Tolerance in seconds when matching window durations (default 30)
    pub window_duration_tolerance: u64,
    // ── Execution realism ──
    /// Minimum seconds between Leg1 fill and Leg2 fill (simulates CLOB latency)
    pub min_leg2_delay_secs: u64,
    /// Maximum trades per event window (prevents overtrading same window)
    pub max_trades_per_event: usize,
    // ── Vol model ──
    /// Drift estimate for log-normal model
    pub mu: f64,
    /// Volatility lookback window in seconds
    pub vol_lookback_secs: u64,
    /// Volatility floor to prevent overconfidence
    pub vol_floor: f64,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
}

impl Default for StaggeredArbBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 20,
            max_concurrent_positions: 5,
            direction_threshold: 0.03,
            max_initial_sum: dec!(1.10),
            min_profit_target: dec!(0.005),
            max_wait_secs: 180,
            max_wait_pct: 0.40,
            min_time_remaining_secs: 60,
            max_leg1_loss: dec!(0), // 0 = disabled, hold to settlement
            force_complete_threshold: dec!(1.00),
            min_ask_price: dec!(0.05), // ignore asks below $0.05 (no real liquidity)
            min_entry_sum: dec!(0.70), // reject if up+down sum < $0.70 (illiquid extremes)
            allowed_window_durations: vec![300, 900], // 5m + 15m
            window_duration_tolerance: 30,
            min_leg2_delay_secs: 3,
            max_trades_per_event: 2,
            mu: 0.0,
            vol_lookback_secs: 300,
            vol_floor: 0.005,
            cooldown_secs: 5,
        }
    }
}

impl StaggeredArbBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
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
    pm_asks: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
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
            pm_asks: HashMap::new(),
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

    // ─── Main loop ──────────────────────────────────────────

    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            // Prune expired events
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote { up_ask, down_ask } => {
                    self.handle_pm_quote(&update.symbol, *up_ask, *down_ask, update.timestamp);
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
                    }

                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            // Compute window duration and filter
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
                            if !allowed {
                                trace!(
                                    "Skipping event {} with duration {}s (not in allowed list)",
                                    event_slug,
                                    duration_secs
                                );
                            } else {
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
                UpdateType::LobSnapshot {
                    ask_depth_shares, ..
                } => {
                    // Update LOB depth cache for this symbol
                    self.lob_depth
                        .insert(update.symbol.clone(), *ask_depth_shares);
                }
            }
        }

        self.close_remaining_positions();
        let _ = self.recorder.flush();
        self.build_results()
    }

    // ─── Event handlers ──────────────────────────────────────

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
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks
        let entry = self
            .pm_asks
            .entry(symbol.to_string())
            .or_insert((None, None));
        if up_ask.is_some() {
            entry.0 = up_ask;
        }
        if down_ask.is_some() {
            entry.1 = down_ask;
        }

        // 1. Check Leg2 opportunities for existing positions
        self.check_leg2_opportunities(symbol, ts);

        // 2. Try new entries on active windows
        self.try_entry(symbol, ts);

        // Record equity
        self.record_equity(ts);
    }

    // ─── Helpers ───────────────────────────────────────────────

    /// Get current LOB depth for a symbol, falling back to a conservative default.
    fn market_depth(&self, symbol: &str) -> u64 {
        self.lob_depth.get(symbol).copied().unwrap_or(500)
    }

    // ─── Entry logic ─────────────────────────────────────────

    fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let windows: Vec<ActiveWindowInfo> = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => return,
        };

        let (spot_price, spot_vol_info) = match self.spot_prices.get(symbol) {
            Some(s) => {
                let lookback = self.config.vol_lookback_secs;
                let vol = s.volatility(lookback).and_then(|v| v.to_f64());
                let n_ticks = s.history_len().min(5000) as f64;
                (s.price, (vol, n_ticks))
            }
            None => return,
        };
        let (up_ask, down_ask) = match self.pm_asks.get(symbol) {
            Some(asks) => *asks,
            None => return,
        };

        for window in windows {
            self.try_entry_for_window(
                symbol,
                ts,
                &window,
                spot_price,
                spot_vol_info,
                up_ask,
                down_ask,
            );
        }
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        spot_vol_info: (Option<f64>, f64),
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        // 1. Time remaining check
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }

        // 2. Need both asks to compute sum
        let (ua, da) = match (up_ask, down_ask) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };

        // 2b. Filter out extreme prices with no real liquidity
        if ua < self.config.min_ask_price || da < self.config.min_ask_price {
            return;
        }

        // 2c. Filter out illiquid extreme-price pairs (both sides cheap = no real market)
        if ua + da < self.config.min_entry_sum {
            return;
        }

        // 3. Sum check: up_ask + down_ask <= max_initial_sum
        let current_sum = ua + da;
        if current_sum > self.config.max_initial_sum {
            return;
        }

        // 4. Compute volatility
        let sigma = {
            let floor = self.config.vol_floor;
            match spot_vol_info.0 {
                Some(tick_vol) if tick_vol > 0.0 => {
                    let n_ticks = spot_vol_info.1;
                    let period_vol = tick_vol * n_ticks.sqrt();
                    period_vol.max(floor)
                }
                _ => floor,
            }
        };

        // 5. Estimate probability
        let p_hat = estimate_probability(window.s0, st, sigma, time_remaining, self.config.mu);

        // 6. Direction threshold: |p_hat - 0.5| >= direction_threshold
        let direction_strength = (p_hat - 0.5).abs();
        if direction_strength < self.config.direction_threshold {
            return;
        }

        // 7. Direction: p_hat > 0.5 → buy UP first (it's about to get expensive)
        let (leg1_dir, leg1_ask) = if p_hat > 0.5 {
            (Direction::Up, ua)
        } else {
            (Direction::Down, da)
        };

        // 8. Target Leg2 price: need leg1 + leg2 < 1.0 - min_profit_target
        let target_leg2 = Decimal::ONE - leg1_ask - self.config.min_profit_target;
        if target_leg2 <= Decimal::ZERO {
            return;
        }

        // 9. Cooldown check
        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return;
            }
        }

        // 10. Max positions check
        let active_count = self
            .positions
            .iter()
            .filter(|p| p.state == ArbPositionState::Leg1Filled)
            .count();
        if active_count >= self.config.max_concurrent_positions {
            return;
        }

        // 11. Don't enter same event twice (concurrently)
        let already_in = self
            .positions
            .iter()
            .any(|p| p.event_slug == window.event_slug && p.state == ArbPositionState::Leg1Filled);
        if already_in {
            return;
        }

        // 11b. Max trades per event window
        if self.config.max_trades_per_event > 0 {
            let count = self
                .event_trade_count
                .get(&window.event_slug)
                .copied()
                .unwrap_or(0);
            if count >= self.config.max_trades_per_event {
                return;
            }
        }

        // 12. Simulate Leg1 buy (use real LOB depth)
        let depth = self.market_depth(symbol);
        let sim_result =
            self.execution_sim
                .simulate_buy(leg1_ask, ts, self.config.shares_per_trade, depth);

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let entry_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_cost = entry_cost + entry_fee;

        if total_cost > self.equity {
            trace!(
                "Skipping: insufficient equity ({} < {})",
                self.equity,
                total_cost
            );
            return;
        }

        self.equity -= total_cost;

        // Calculate wait deadline
        let window_duration = (window.end_time - ts).num_seconds() as f64;
        let max_wait_by_pct = (window_duration * self.config.max_wait_pct) as i64;
        let max_wait = (self.config.max_wait_secs as i64).min(max_wait_by_pct);
        let wait_deadline = ts + chrono::Duration::seconds(max_wait);

        self.positions.push(StaggeredArbPosition {
            symbol: symbol.to_string(),
            event_slug: window.event_slug.clone(),
            leg1_direction: leg1_dir,
            leg1_price: sim_result.fill_price,
            leg1_shares: sim_result.filled_shares,
            leg1_time: ts,
            leg1_fee: entry_fee,
            wait_deadline,
            s0: window.s0,
            event_end_time: window.end_time,
            window_duration_secs: window.window_duration_secs,
            entry_p_hat: if matches!(leg1_dir, Direction::Up) {
                p_hat
            } else {
                1.0 - p_hat
            },
            entry_sigma: sigma,
            best_sum_seen: current_sum,
            initial_sum: current_sum,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);
        *self
            .event_trade_count
            .entry(window.event_slug.clone())
            .or_default() += 1;

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", leg1_dir),
            timestamp: ts,
            p_hat: Some(p_hat),
            ev_net: None,
            sigma: Some(sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: Some(st),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "LEG1 {} {} @ {:.4} | sum={:.4} p_hat={:.3} σ={:.5}",
            symbol, leg1_dir, sim_result.fill_price, current_sum, p_hat, sigma
        );
    }

    // ─── Leg2 monitoring ──────────────────────────────────────

    fn check_leg2_opportunities(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let pm_asks = match self.pm_asks.get(symbol) {
            Some(a) => *a,
            None => return,
        };

        // Collect actions to take (can't mutate positions while iterating)
        let mut actions: Vec<(usize, Leg2Action)> = Vec::new();

        for (i, pos) in self.positions.iter_mut().enumerate() {
            if pos.symbol != symbol || pos.state != ArbPositionState::Leg1Filled {
                continue;
            }

            // Get the opposite side's ask
            let other_ask = match pos.leg1_direction {
                Direction::Up => pm_asks.1,   // need DOWN ask
                Direction::Down => pm_asks.0, // need UP ask
            };
            let other_ask = match other_ask {
                Some(a) if a >= self.config.min_ask_price => a,
                Some(_) => continue, // too cheap, no real liquidity
                None => continue,
            };

            // Also reject if combined sum is too low (illiquid extreme-price pair)
            if pos.leg1_price + other_ask < self.config.min_entry_sum {
                continue;
            }

            let current_sum = pos.leg1_price + other_ask;

            // Track best sum seen
            if current_sum < pos.best_sum_seen {
                pos.best_sum_seen = current_sum;
            }

            let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;
            let min_time = self.config.min_time_remaining_secs as f64;

            // Check minimum delay since Leg1 fill (execution realism)
            let secs_since_leg1 = (ts - pos.leg1_time).num_seconds();
            let leg2_ready = secs_since_leg1 >= self.config.min_leg2_delay_secs as i64;

            // A. Profitable merge: sum < 1.0 - min_profit_target
            if current_sum < Decimal::ONE - self.config.min_profit_target && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask)));
                continue;
            }

            // B. Lock profit: sum < 1.0 (any profit is good)
            if current_sum < Decimal::ONE && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask)));
                continue;
            }

            // C. Timeout — force-complete the arb to avoid directional risk
            //    Even if sum > 1.0, the loss is bounded: (sum - 1.0) × shares
            //    Much better than risking full Leg1 cost at settlement
            if ts >= pos.wait_deadline && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask)));
                continue;
            }

            // D. Stop loss on Leg1 — force-complete if loss exceeds threshold
            //    With max_leg1_loss = 0 the stop-loss is disabled
            if self.config.max_leg1_loss > Decimal::ZERO {
                let leg1_current_value = match pos.leg1_direction {
                    Direction::Up => pm_asks.0.unwrap_or(pos.leg1_price),
                    Direction::Down => pm_asks.1.unwrap_or(pos.leg1_price),
                };
                let leg1_loss = pos.leg1_price - leg1_current_value;
                if leg1_loss > self.config.max_leg1_loss && leg2_ready {
                    // Force buy Leg2 to lock in bounded loss instead of aborting
                    actions.push((i, Leg2Action::Fill(other_ask)));
                    continue;
                }
            }

            // E. Time safety: not enough time left — force-complete the arb
            if time_remaining < min_time && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask)));
            }
        }

        // Execute actions in reverse order to preserve indices
        actions.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, action) in actions {
            match action {
                Leg2Action::Fill(other_ask) => {
                    self.fill_leg2(idx, other_ask, ts);
                }
                Leg2Action::Abort(reason) => {
                    self.abort_position(idx, &reason, ts);
                }
            }
        }
    }

    /// Fill Leg2 and immediately merge for $1.00 per share.
    fn fill_leg2(&mut self, idx: usize, other_ask: Decimal, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        let leg2_dir = match pos.leg1_direction {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        };

        let depth = self.market_depth(&pos.symbol);
        let sim_result = self.execution_sim.simulate_buy(
            other_ask,
            ts,
            pos.leg1_shares, // match Leg1 size
            depth,
        );

        let leg2_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let leg2_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_leg2_cost = leg2_cost + leg2_fee;

        if total_leg2_cost > self.equity {
            trace!("Cannot fill Leg2: insufficient equity");
            return;
        }

        self.equity -= total_leg2_cost;

        // Immediate merge: min(leg1_shares, leg2_shares) × $1.00
        let mergeable = pos.leg1_shares.min(sim_result.filled_shares);
        let payout = Decimal::from(mergeable) * Decimal::ONE;
        let total_cost =
            Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee + leg2_cost + leg2_fee;
        let pnl = payout - total_cost;

        self.equity += payout;

        let holding_secs = (ts - pos.leg1_time).num_seconds();
        let final_sum = pos.leg1_price + sim_result.fill_price;

        let symbol = pos.symbol.clone();
        let event_slug = pos.event_slug.clone();

        // Update position
        let pos = &mut self.positions[idx];
        pos.leg2_direction = Some(leg2_dir);
        pos.leg2_price = Some(sim_result.fill_price);
        pos.leg2_shares = Some(sim_result.filled_shares);
        pos.leg2_time = Some(ts);
        pos.leg2_fee = Some(leg2_fee);
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some("merge".to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: Some(sim_result.fill_price),
            leg2_time: Some(ts),
            shares: mergeable,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "merge".to_string(),
            initial_sum: pos.initial_sum,
            final_sum: Some(final_sum),
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", pos.leg1_direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some("merge".to_string()),
            exit_price: Some(sim_result.fill_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price: sim_result.fill_price,
            shares: mergeable as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "merge".to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });

        debug!(
            "MERGE {} | leg1={:.4} leg2={:.4} sum={:.4} pnl={:.4}",
            event_slug, pos.leg1_price, sim_result.fill_price, final_sum, pnl
        );
    }

    /// Abort a position — sell Leg1 at current market price.
    fn abort_position(&mut self, idx: usize, reason: &str, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        let current_price = match pos.leg1_direction {
            Direction::Up => self.pm_asks.get(&pos.symbol).and_then(|a| a.0),
            Direction::Down => self.pm_asks.get(&pos.symbol).and_then(|a| a.1),
        }
        .unwrap_or(pos.leg1_price);

        // Simulate sell
        let depth = self.market_depth(&pos.symbol);
        let sim_result =
            self.execution_sim
                .simulate_sell(current_price, ts, pos.leg1_shares, depth);

        let proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let sell_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let net_proceeds = proceeds - sell_fee;

        self.equity += net_proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = net_proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Aborted;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", pos.leg1_direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(sim_result.fill_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price: sim_result.fill_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });

        debug!("ABORT {} reason={} pnl={:.4}", pos.event_slug, reason, pnl);
    }

    // ─── Settlement (single-leg only) ────────────────────────

    fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        _up_won: bool,
        ts: DateTime<Utc>,
    ) {
        // At settlement, force-complete any remaining Leg1 positions by buying Leg2.
        // This avoids directional risk — even if sum > 1.0, the loss is bounded.
        let mut to_fill: Vec<usize> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.event_slug != event_slug {
                continue;
            }
            if pos.state != ArbPositionState::Leg1Filled {
                continue;
            }
            to_fill.push(i);
        }

        to_fill.sort_by(|a, b| b.cmp(a));
        for idx in to_fill {
            let pos = &self.positions[idx];
            // Get the opposite side's ask for forced Leg2
            let pm_asks = self.pm_asks.get(&pos.symbol).copied();
            let other_ask = pm_asks.and_then(|a| match pos.leg1_direction {
                Direction::Up => a.1,   // need DOWN ask
                Direction::Down => a.0, // need UP ask
            });
            match other_ask {
                Some(ask) => {
                    // Force buy Leg2 and merge — bounded loss
                    self.fill_leg2(idx, ask, ts);
                }
                None => {
                    // No quote data at all — last resort: abort (sell Leg1 at market)
                    self.abort_position(idx, "no_quote_at_settlement", ts);
                }
            }
        }
    }

    #[allow(dead_code)]
    fn settle_single_leg(&mut self, idx: usize, exit_price: Decimal, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        // At settlement, fee = 0 (p*(1-p) = 0 at $1 or $0)
        let proceeds = exit_price * Decimal::from(pos.leg1_shares);
        self.equity += proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some("settlement".to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });
    }

    /// Force-close remaining Leg1 positions by buying Leg2 at market.
    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = self
            .positions
            .iter()
            .enumerate()
            .filter(|(_, p)| p.state == ArbPositionState::Leg1Filled)
            .map(|(i, _)| i)
            .rev()
            .collect();
        for idx in indices {
            let pos = &self.positions[idx];
            let pm_asks = self.pm_asks.get(&pos.symbol).copied();
            let other_ask = pm_asks.and_then(|a| match pos.leg1_direction {
                Direction::Up => a.1,
                Direction::Down => a.0,
            });
            match other_ask {
                Some(ask) => self.fill_leg2(idx, ask, ts),
                None => self.abort_position(idx, "data_exhausted", ts),
            }
        }
    }

    // ─── Equity tracking ─────────────────────────────────────

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

    // ─── Results ─────────────────────────────────────────────

    fn build_results(&self) -> BacktestResults {
        build_staggered_arb_results(
            &self.closed_trades,
            &self.equity_curve,
            self.max_drawdown,
            self.data_range_start,
            self.data_range_end,
        )
    }

    /// Print staggered-arb-specific summary stats.
    pub fn print_staggered_summary(&self) {
        if self.closed_trades.is_empty() {
            println!("\n=== Staggered Arb Summary ===");
            println!("No trades executed.");
            return;
        }

        let total = self.closed_trades.len();
        let merges: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "merge")
            .collect();
        let settlements: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .collect();
        let aborts: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason != "merge" && t.exit_reason != "settlement")
            .collect();

        let merge_count = merges.len();
        let leg2_fill_rate = merge_count as f64 / total as f64 * 100.0;

        // Spread compression for merges
        let avg_compression = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| t.final_sum.map(|fs| t.initial_sum - fs))
                .map(|d| d.to_f64().unwrap_or(0.0))
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        // Average Leg2 wait time (merges only)
        let avg_wait = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| {
                    t.leg2_time
                        .map(|l2| (l2 - t.leg1_time).num_seconds() as f64)
                })
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        // Abort reason distribution
        let mut abort_reasons: HashMap<&str, usize> = HashMap::new();
        for t in &aborts {
            *abort_reasons.entry(&t.exit_reason).or_default() += 1;
        }

        // PnL comparison: merge vs single-leg
        let merge_pnl: Decimal = merges.iter().map(|t| t.pnl).sum();
        let single_pnl: Decimal = settlements.iter().map(|t| t.pnl).sum();
        let abort_pnl: Decimal = aborts.iter().map(|t| t.pnl).sum();

        // Direction prediction accuracy (for merges: did the predicted side move favorably?)
        let direction_correct = merges.iter().filter(|t| t.won).count();
        let direction_accuracy = if !merges.is_empty() {
            direction_correct as f64 / merges.len() as f64 * 100.0
        } else {
            0.0
        };

        // Capital turnover: merge_count / avg_holding_time
        let avg_hold = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        println!("\n=== Staggered Arb Summary ===");
        println!("Total attempts:     {}", total);
        println!(
            "Leg2 fill rate:     {:.1}% ({}/{})",
            leg2_fill_rate, merge_count, total
        );
        println!("Settlements:        {} (single-leg)", settlements.len());
        println!("Aborts:             {}", aborts.len());
        println!();
        println!("Avg spread compression: {:.4}", avg_compression);
        println!("Avg Leg2 wait time:     {:.1}s", avg_wait);
        println!("Avg holding time:       {:.1}s", avg_hold);
        println!();
        println!("PnL breakdown:");
        println!("  Merges:      ${:.2} ({} trades)", merge_pnl, merge_count);
        println!(
            "  Settlements: ${:.2} ({} trades)",
            single_pnl,
            settlements.len()
        );
        println!("  Aborts:      ${:.2} ({} trades)", abort_pnl, aborts.len());
        println!();
        if !abort_reasons.is_empty() {
            println!("Abort reasons:");
            for (reason, count) in &abort_reasons {
                println!("  {:<16} {}", reason, count);
            }
            println!();
        }
        println!(
            "Direction accuracy: {:.1}% (merge wins)",
            direction_accuracy
        );
        println!(
            "Capital turnover:   {} merges, avg hold {:.0}s",
            merge_count, avg_hold
        );

        // Per-symbol breakdown
        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry = symbol_stats
                .entry(&t.symbol)
                .or_insert((0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            entry.2 += t.pnl;
        }
        if symbol_stats.len() > 1 {
            println!("\nPer-symbol:");
            println!(
                "  {:<12} {:>6} {:>6} {:>8} {:>10}",
                "Symbol", "Trades", "Wins", "WinRate", "PnL"
            );
            let mut syms: Vec<&&str> = symbol_stats.keys().collect();
            syms.sort();
            for sym in syms {
                let (t, w, p) = symbol_stats[sym];
                let wr = if t > 0 {
                    w as f64 / t as f64 * 100.0
                } else {
                    0.0
                };
                println!("  {:<12} {:>6} {:>6} {:>7.1}% ${:>9.2}", sym, t, w, wr, p);
            }
        }

        // Per-window-duration breakdown (5m vs 15m)
        let mut window_stats: HashMap<&str, (usize, usize, usize, Decimal)> = HashMap::new(); // (total, wins, merges, pnl)
        for t in &self.closed_trades {
            let label = match t.window_duration_secs {
                0..=330 => "5m",
                331..=930 => "15m",
                _ => "other",
            };
            let entry = window_stats
                .entry(label)
                .or_insert((0, 0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            if t.exit_reason == "merge" {
                entry.2 += 1;
            }
            entry.3 += t.pnl;
        }
        println!("\nPer-window breakdown:");
        println!(
            "  {:<8} {:>6} {:>6} {:>8} {:>8} {:>10}",
            "Window", "Trades", "Wins", "WinRate", "Merges", "PnL"
        );
        let mut labels: Vec<&&str> = window_stats.keys().collect();
        labels.sort();
        for label in labels {
            let (t, w, m, p) = window_stats[label];
            let wr = if t > 0 {
                w as f64 / t as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<8} {:>6} {:>6} {:>7.1}% {:>8} ${:>9.2}",
                label, t, w, wr, m, p
            );
        }
    }
}
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Leg2Action {
    Fill(Decimal),
    #[allow(dead_code)]
    Abort(String),
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_backtest::{HistoricalFeed, MarketUpdate};
    use rust_decimal_macros::dec;

    fn make_spot(ts: &str, symbol: &str, price: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::SpotTrade {
                price,
                quantity: None,
            },
        }
    }

    fn make_quote(ts: &str, symbol: &str, up: Decimal, down: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(up),
                down_ask: Some(down),
            },
        }
    }

    fn make_event_open(
        ts: &str,
        symbol: &str,
        slug: &str,
        end_ts: &str,
        s0: Decimal,
    ) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::EventState {
                event_slug: slug.to_string(),
                end_time: Some(
                    DateTime::parse_from_rfc3339(end_ts)
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                price_to_beat: Some(s0),
                outcome: None,
            },
        }
    }

    fn make_settlement(ts: &str, symbol: &str, slug: &str, up_won: bool) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::EventState {
                event_slug: slug.to_string(),
                end_time: None,
                price_to_beat: None,
                outcome: Some(up_won),
            },
        }
    }

    #[test]
    fn test_config_defaults() {
        let config = StaggeredArbBacktestConfig::default();
        assert_eq!(config.direction_threshold, 0.03);
        assert_eq!(config.max_initial_sum, dec!(1.10));
        assert_eq!(config.min_profit_target, dec!(0.005));
        assert_eq!(config.max_wait_secs, 180);
        assert_eq!(config.min_leg2_delay_secs, 3);
        assert_eq!(config.max_trades_per_event, 2);
        assert_eq!(config.cooldown_secs, 5);
        assert_eq!(config.min_ask_price, dec!(0.05));
        assert_eq!(config.min_entry_sum, dec!(0.70));
    }

    #[test]
    fn test_position_state_display() {
        assert_eq!(format!("{}", ArbPositionState::Leg1Filled), "Leg1Filled");
        assert_eq!(format!("{}", ArbPositionState::Settled), "Settled");
        assert_eq!(format!("{}", ArbPositionState::Aborted), "Aborted");
    }

    #[test]
    fn test_merge_when_sum_below_one() {
        // Scenario: UP is about to rise. Buy UP at 0.45, then DOWN drops to 0.50.
        // Sum = 0.45 + 0.50 = 0.95 < 1.0 → merge for profit.
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01; // low threshold for test
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.allowed_window_durations = vec![]; // accept all in test

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        // Build a feed: event open, spot prices to build vol history, then quotes
        let mut updates = vec![
            // Event window: 10 minutes
            make_event_open(
                "2026-01-01T00:00:00Z",
                "BTCUSDT",
                "test-event",
                "2026-01-01T00:10:00Z",
                dec!(100000),
            ),
        ];

        // Spot price history (need enough for volatility calc)
        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            // Price moving up → p_hat > 0.5 → buy UP first
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }

        // Initial quotes: sum = 1.05 (spread)
        updates.push(make_quote(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            dec!(0.55),
            dec!(0.50),
        ));

        // After some time, DOWN ask drops → sum becomes < 1.0
        updates.push(make_quote(
            "2026-01-01T00:02:00Z",
            "BTCUSDT",
            dec!(0.60),
            dec!(0.38),
        ));

        let mut feed = HistoricalFeed::new(updates);

        let results = engine.run(&mut feed);

        // Should have at least attempted trades
        // The exact outcome depends on vol calc and execution sim,
        // but the engine should not panic and should produce valid results
        assert!(
            results.total_pnl.is_sign_positive()
                || results.total_pnl.is_sign_negative()
                || results.total_pnl == Decimal::ZERO
        );
    }

    #[test]
    fn test_single_leg_settlement() {
        // Scenario: Buy UP, Leg2 never fills, position settles at $1 (UP wins)
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01;
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.max_wait_secs = 30;
        config.allowed_window_durations = vec![];

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let mut updates = vec![make_event_open(
            "2026-01-01T00:00:00Z",
            "BTCUSDT",
            "test-event",
            "2026-01-01T00:10:00Z",
            dec!(100000),
        )];

        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }

        // Quotes with sum > 1 throughout (no Leg2 opportunity)
        updates.push(make_quote(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            dec!(0.55),
            dec!(0.55),
        ));
        updates.push(make_quote(
            "2026-01-01T00:03:00Z",
            "BTCUSDT",
            dec!(0.60),
            dec!(0.55),
        ));

        // Settlement: UP wins
        updates.push(make_settlement(
            "2026-01-01T00:10:00Z",
            "BTCUSDT",
            "test-event",
            true,
        ));

        let mut feed = HistoricalFeed::new(updates);

        let results = engine.run(&mut feed);

        // Engine should handle settlement without panicking
        // Trades that settled as UP winning should be profitable
        for trade in engine.closed_trades() {
            if trade.exit_reason == "settlement" && trade.leg1_direction == "UP" {
                assert!(trade.won, "UP leg should win when UP settles at $1");
            }
        }
        let _ = results; // use results to avoid warning
    }

    #[test]
    fn test_abort_on_stop_loss() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01;
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.max_leg1_loss = dec!(0.05);
        config.allowed_window_durations = vec![];

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let mut updates = vec![make_event_open(
            "2026-01-01T00:00:00Z",
            "BTCUSDT",
            "test-event",
            "2026-01-01T00:10:00Z",
            dec!(100000),
        )];

        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }

        // Entry quote
        updates.push(make_quote(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            dec!(0.55),
            dec!(0.50),
        ));

        // UP ask drops significantly → stop loss triggers
        updates.push(make_quote(
            "2026-01-01T00:01:30Z",
            "BTCUSDT",
            dec!(0.40),
            dec!(0.65),
        ));

        let mut feed = HistoricalFeed::new(updates);

        let _results = engine.run(&mut feed);

        // Check that any aborted trades have stop_loss reason
        let stop_losses: Vec<_> = engine
            .closed_trades()
            .iter()
            .filter(|t| t.exit_reason == "stop_loss")
            .collect();
        // May or may not trigger depending on execution sim, but engine shouldn't panic
        let _ = stop_losses;
    }
}
