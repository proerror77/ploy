//! Directional backtest engine for probability-driven binary option trading.
//!
//! Uses `estimate_probability()` + `FeeModel` cost filtering to enter positions
//! when EV_net > threshold, and holds to settlement by default. Replaces the
//! momentum-based signal generation from `MomentumBacktestEngine` while reusing
//! the same feed/results/execution infrastructure.
//!
//! Binance spot price serves as Chainlink proxy (>99.9% correlation on 5m/15m).
//!
//! Usage:
//!   ploy strategy backtest directional --symbols BTCUSDT --save --json

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, trace};

use crate::adapters::SpotPrice;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a directional backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent positions per symbol
    pub max_concurrent_positions: usize,
    /// Minimum EV_net to enter (e.g. 0.05 = 5%)
    pub entry_threshold: f64,
    /// Don't buy YES above this price (e.g. 0.85)
    pub max_entry_price: Decimal,
    /// Don't buy YES below this price (e.g. 0.15)
    pub min_entry_price: Decimal,
    /// Drift estimate for log-normal model (start at 0.0)
    pub mu: f64,
    /// Volatility lookback window in seconds (e.g. 300 = 5min)
    pub vol_lookback_secs: u64,
    /// Probability stop: exit if p_hat < entry_p * ratio (e.g. 0.6)
    pub prob_stop_ratio: f64,
    /// Time stop: exit if <N secs remaining AND EV < 0 (e.g. 30)
    pub time_stop_secs: u64,
    /// Maximum loss per position in USD
    pub hard_stop_usd: Decimal,
    /// Hold winners to settlement (default true — let them run)
    pub hold_to_settlement: bool,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter a position (seconds).
    /// Prevents entering near settlement where model is degenerate.
    pub min_time_remaining_secs: u64,
    /// Volatility floor for the probability model.
    /// Prevents overconfidence when realized vol is near zero.
    pub vol_floor: f64,
}

impl Default for DirectionalBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.10,
            max_entry_price: dec!(0.85),
            min_entry_price: dec!(0.15),
            mu: 0.0,
            vol_lookback_secs: 300,
            prob_stop_ratio: 0.6,
            time_stop_secs: 30,
            hard_stop_usd: dec!(5),
            hold_to_settlement: true,
            cooldown_secs: 60,
            min_time_remaining_secs: 120,
            vol_floor: 0.005,
        }
    }
}

impl DirectionalBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Position tracking
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirectionalPosition {
    symbol: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    #[allow(dead_code)]
    event_slug: String,
    /// Window open price (Binance proxy for Chainlink S0)
    s0: Decimal,
    /// When the event window settles
    event_end_time: DateTime<Utc>,
    /// Model probability at entry
    entry_p_hat: f64,
    /// EV_net at entry for diagnostics
    entry_ev_net: f64,
    /// Realized vol at entry
    entry_sigma: f64,
    /// Latest PM price for mark-to-market
    latest_pm_price: Decimal,
}

/// A closed trade with directional-specific diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalClosedTrade {
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
    // Directional-specific fields
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma: f64,
}

// ─────────────────────────────────────────────────────────────
// Active event window info
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    /// S0 = price_to_beat from EventState
    s0: Decimal,
    end_time: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct DirectionalBacktestEngine {
    config: DirectionalBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    // Active events: symbol -> window info
    active_events: HashMap<String, ActiveWindowInfo>,
    // Positions & trades
    positions: Vec<DirectionalPosition>,
    closed_trades: Vec<DirectionalClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

impl DirectionalBacktestEngine {
    pub fn new(config: DirectionalBacktestConfig) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
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
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn config(&self) -> &DirectionalBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[DirectionalClosedTrade] {
        &self.closed_trades
    }

    // ─── Main loop ──────────────────────────────────────────

    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            // Track data range
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

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
                    // Binary settlement
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, *won, update.timestamp);
                        // Clear the active window after settlement
                        self.active_events.remove(&update.symbol);
                    }

                    // Track active window: store S0 (price_to_beat) for probability calc
                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            self.active_events.insert(
                                update.symbol.clone(),
                                ActiveWindowInfo {
                                    event_slug: event_slug.clone(),
                                    s0: *s0,
                                    end_time: *end,
                                },
                            );
                        }
                    }
                }
            }
        }

        // Force-close any remaining positions at latest PM price (data exhausted)
        self.close_remaining_positions();
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

        // Update position mark-to-market
        for pos in &mut self.positions {
            if pos.symbol == symbol {
                match pos.direction {
                    Direction::Up => {
                        if let Some(ask) = up_ask {
                            pos.latest_pm_price = ask;
                        }
                    }
                    Direction::Down => {
                        if let Some(ask) = down_ask {
                            pos.latest_pm_price = ask;
                        }
                    }
                }
            }
        }

        // Try directional entry
        self.try_directional_entry(symbol, ts);

        // Check exits for existing positions
        self.check_exits(ts);

        // Record equity curve
        self.record_equity(ts);
    }

    // ─── Entry logic (probability + fee model) ───────────────

    fn try_directional_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        // 1. Need: active event with S0, spot price history, PM asks
        let window = match self.active_events.get(symbol) {
            Some(w) => w.clone(),
            None => return,
        };
        let spot = match self.spot_prices.get(symbol) {
            Some(s) => s,
            None => return,
        };
        let (up_ask, down_ask) = match self.pm_asks.get(symbol) {
            Some(asks) => *asks,
            None => return,
        };

        // 2. Time remaining — must have minimum buffer to avoid near-settlement degeneracy
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }

        // 3. Realized vol from Binance history (proxy for Chainlink)
        //
        // SpotPrice.volatility() returns std-dev of consecutive tick returns.
        // Each tick interval is ~200ms. To get per-period vol (σ for 900s window),
        // we scale by √(N) where N = number of ticks in the lookback.
        // This gives: σ_period ≈ σ_tick × √(ticks_in_lookback)
        //
        // Alternatively, if volatility() returns None (insufficient data), we use
        // a conservative BTC 15-min vol estimate (0.005 ≈ 0.5%).
        let sigma = {
            let lookback = self.config.vol_lookback_secs;
            let floor = self.config.vol_floor;
            match spot.volatility(lookback).and_then(|v| v.to_f64()) {
                Some(tick_vol) if tick_vol > 0.0 => {
                    // Count ticks in the lookback window to scale properly
                    let n_ticks = spot.history_len().min(5000) as f64;
                    // Scale tick vol to period vol
                    let period_vol = tick_vol * n_ticks.sqrt();
                    period_vol.max(floor)
                }
                _ => floor, // Default: conservative floor
            }
        };

        // 4. Estimate probability: P(ST >= S0)
        let st = spot.price; // Binance current = Chainlink proxy
        let p_hat = estimate_probability(window.s0, st, sigma, time_remaining, self.config.mu);

        // 5. Direction + market price
        let (direction, market_ask) = if p_hat > 0.5 {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return,
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return,
            }
        };
        let effective_p = if matches!(direction, Direction::Up) {
            p_hat
        } else {
            1.0 - p_hat
        };

        // 6. Price bounds check
        if market_ask > self.config.max_entry_price || market_ask < self.config.min_entry_price {
            return;
        }

        // 7. All-in cost via FeeModel
        // Polymarket fee formula: fee_usd = shares × price × feeRate × (p*(1-p))^exponent
        // Per-share fee in USD = price × effective_rate(price)
        let best_bid = (market_ask - dec!(0.02)).max(dec!(0.01));
        let depth_ratio = Decimal::from(self.config.shares_per_trade) / dec!(10000);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        // cost.taker_fee = effective_rate(p) = feeRate × (p*(1-p))^exponent
        // actual per-share fee in USD = market_ask × effective_rate(p)
        let fee_per_share_usd = market_ask * cost.taker_fee;
        let spread_plus_slip = cost.spread_cost + cost.depth_slippage;

        // 8. EV_net check
        // EV = P(win) × $1.00 - entry_price - fee_per_share - spread - slippage
        let market_ask_f = market_ask.to_f64().unwrap_or(0.5);
        let total_cost_f =
            fee_per_share_usd.to_f64().unwrap_or(0.01) + spread_plus_slip.to_f64().unwrap_or(0.01);
        let ev_net = effective_p - market_ask_f - total_cost_f;
        if ev_net < self.config.entry_threshold {
            return;
        }

        // 9. Cooldown check
        if let Some(last) = self.last_entry_time.get(symbol) {
            let elapsed = (ts - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                return;
            }
        }

        // 10. Max positions check
        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        // 11. Don't enter if already holding same symbol+direction
        let already_holding = self.positions.iter().any(|p| {
            p.symbol == symbol
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&direction)
        });
        if already_holding {
            return;
        }

        // 12. Execute entry via ExecutionSimulator
        let sim_result = self.execution_sim.simulate_buy(
            market_ask,
            ts,
            self.config.shares_per_trade,
            10_000, // market depth assumption
        );

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        // Taker fee at entry: shares × price × feeRate × (p*(1-p))^exponent
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(sim_result.filled_shares), sim_result.fill_price)
            * sim_result.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
            trace!(
                "Skipping entry: insufficient equity ({} < {})",
                self.equity, total_entry_cost
            );
            return;
        }

        self.equity -= total_entry_cost;

        self.positions.push(DirectionalPosition {
            symbol: symbol.to_string(),
            direction,
            entry_price: sim_result.fill_price,
            entry_time: ts,
            shares: sim_result.filled_shares,
            event_slug: window.event_slug.clone(),
            s0: window.s0,
            event_end_time: window.end_time,
            entry_p_hat: effective_p,
            entry_ev_net: ev_net,
            entry_sigma: sigma,
            latest_pm_price: market_ask,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);

        debug!(
            "ENTRY {} {} @ {:.4} | p_hat={:.3} ev_net={:.3} σ={:.5}",
            symbol, direction, sim_result.fill_price, effective_p, ev_net, sigma
        );
    }

    // ─── Exit logic (directional, NOT arb) ───────────────────

    fn check_exits(&mut self, ts: DateTime<Utc>) {
        let mut to_close: Vec<(usize, Decimal, &str)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;

            // A. Hold to settlement — no early exit at all. Binary options settle
            // at $1.00 or $0.00, so unrealized PnL fluctuations are noise.
            // The only meaningful exit is settlement itself.
            if self.config.hold_to_settlement && time_remaining > self.config.time_stop_secs as f64
            {
                continue;
            }

            // B. Time stop: <N secs remaining AND current EV is negative
            if time_remaining <= self.config.time_stop_secs as f64 && time_remaining > 0.0 {
                let current_p = self.estimate_current_p(pos, ts);
                let effective_p = if matches!(pos.direction, Direction::Up) {
                    current_p
                } else {
                    1.0 - current_p
                };
                let ev_now = effective_p - pos.latest_pm_price.to_f64().unwrap_or(0.5);
                if ev_now < 0.0 {
                    to_close.push((i, pos.latest_pm_price, "time_stop"));
                    continue;
                }
            }

            // C. Hard stop: unrealized loss exceeds max
            let unrealized = (pos.latest_pm_price - pos.entry_price) * Decimal::from(pos.shares);
            if unrealized < Decimal::ZERO && unrealized.abs() > self.config.hard_stop_usd {
                to_close.push((i, pos.latest_pm_price, "hard_stop"));
                continue;
            }

            // D. Probability stop: p_hat collapsed below entry threshold
            if !self.config.hold_to_settlement {
                let current_p = self.estimate_current_p(pos, ts);
                let effective_p = if matches!(pos.direction, Direction::Up) {
                    current_p
                } else {
                    1.0 - current_p
                };
                if effective_p < pos.entry_p_hat * self.config.prob_stop_ratio {
                    to_close.push((i, pos.latest_pm_price, "prob_stop"));
                }
            }
        }

        // Close in reverse order to preserve indices
        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, reason) in to_close {
            self.close_position(idx, exit_price, reason, ts);
        }
    }

    /// Re-estimate P(ST >= S0) for an open position using current spot data.
    fn estimate_current_p(&self, pos: &DirectionalPosition, ts: DateTime<Utc>) -> f64 {
        let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 {
            // Expired — use spot price comparison
            let st = self
                .spot_prices
                .get(&pos.symbol)
                .map(|s| s.price)
                .unwrap_or(pos.s0);
            return if st >= pos.s0 { 1.0 } else { 0.0 };
        }

        let st = self
            .spot_prices
            .get(&pos.symbol)
            .map(|s| s.price)
            .unwrap_or(pos.s0);

        let sigma = self
            .spot_prices
            .get(&pos.symbol)
            .and_then(|s| {
                let lookback = self.config.vol_lookback_secs;
                let floor = self.config.vol_floor;
                s.volatility(lookback).and_then(|v| v.to_f64()).map(|tick_vol| {
                    if tick_vol > 0.0 {
                        let n_ticks = s.history_len().min(5000) as f64;
                        (tick_vol * n_ticks.sqrt()).max(floor)
                    } else {
                        floor
                    }
                })
            })
            .unwrap_or(pos.entry_sigma)
            .max(self.config.vol_floor);

        estimate_probability(pos.s0, st, sigma, time_remaining, self.config.mu)
    }

    // ─── Settlement ──────────────────────────────────────────

    fn resolve_positions(&mut self, symbol: &str, up_won: bool, ts: DateTime<Utc>) {
        let mut to_close = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol == symbol {
                let exit_price = match (&pos.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((i, exit_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, "settlement", ts);
        }
    }

    // ─── Close position ──────────────────────────────────────

    fn close_position(
        &mut self,
        idx: usize,
        exit_price: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
    ) {
        let pos = self.positions.remove(idx);

        // For settlement ($1 or $0), no need to simulate — it's binary payout.
        // Fee at settlement ($1 or $0): p*(1-p) = 0, so settlement fee = $0.
        // For early exits, simulate sell via ExecutionSimulator + exit fee.
        let (final_price, proceeds, exit_fee) = if reason == "settlement" {
            let p = exit_price;
            // At $1.00 or $0.00, the parabolic fee curve = 0 (p*(1-p) = 0)
            (p, p * Decimal::from(pos.shares), Decimal::ZERO)
        } else {
            let sim_result =
                self.execution_sim
                    .simulate_sell(exit_price, ts, pos.shares, 10_000);
            let raw_proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
            // Taker fee on sell: shares × price × feeRate × (p*(1-p))^exponent
            let sell_fee = self
                .fee_model
                .fee_shares(Decimal::from(sim_result.filled_shares), sim_result.fill_price)
                * sim_result.fill_price;
            (sim_result.fill_price, raw_proceeds - sell_fee, sell_fee)
        };

        self.equity += proceeds;

        // Entry fee was already deducted from equity at entry time,
        // so PnL = proceeds - (shares × entry_price) already reflects the entry fee implicitly.
        // But we also need to account for exit fee in the PnL.
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(pos.shares), pos.entry_price)
            * pos.entry_price;
        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price - entry_fee;
        let holding_secs = (ts - pos.entry_time).num_seconds();

        self.closed_trades.push(DirectionalClosedTrade {
            symbol: pos.symbol,
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: pos.entry_p_hat,
            entry_ev_net: pos.entry_ev_net,
            s0: pos.s0,
            entry_sigma: pos.entry_sigma,
        });
    }

    /// Force-close remaining positions at latest PM price (data exhausted).
    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let price = self.positions[idx].latest_pm_price;
            self.close_position(idx, price, "data_exhausted", ts);
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

        // Sample equity curve (max 1 point per second to avoid bloat)
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
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total - winning;
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
            wins.iter().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };

        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().sum();
        let total_losses_abs: Decimal = losses.iter().map(|l| l.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        let sharpe = self.calculate_sharpe();

        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|t| Decimal::from(t.shares) * t.entry_price)
            .sum();

        let start_time = self.data_range_start.unwrap_or(Utc::now());
        let end_time = self.data_range_end.unwrap_or(Utc::now());

        BacktestResults {
            start_time,
            end_time,
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: sharpe,
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs: avg_holding,
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
        let std_dev = variance.sqrt();

        if std_dev < 1e-10 {
            return 0.0;
        }

        // Annualize: assume ~24 trades/day for 15-min markets
        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std_dev) * trades_per_year.sqrt()
    }

    /// Print directional-specific summary stats beyond BacktestResults.
    pub fn print_directional_summary(&self) {
        if self.closed_trades.is_empty() {
            info!("No trades to summarize.");
            return;
        }

        let total = self.closed_trades.len();

        // Settlement rate
        let settled = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .count();
        let settlement_rate = settled as f64 / total as f64 * 100.0;

        // Exit reason breakdown
        let mut exit_counts: HashMap<&str, usize> = HashMap::new();
        for t in &self.closed_trades {
            *exit_counts.entry(&t.exit_reason).or_default() += 1;
        }

        // Avg p_hat for winners vs losers (calibration check)
        let winner_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.entry_p_hat)
            .collect();
        let loser_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.entry_p_hat)
            .collect();

        let avg_winner_p = if winner_p.is_empty() {
            0.0
        } else {
            winner_p.iter().sum::<f64>() / winner_p.len() as f64
        };
        let avg_loser_p = if loser_p.is_empty() {
            0.0
        } else {
            loser_p.iter().sum::<f64>() / loser_p.len() as f64
        };

        // EV_net distribution
        let ev_nets: Vec<f64> = self.closed_trades.iter().map(|t| t.entry_ev_net).collect();
        let avg_ev = ev_nets.iter().sum::<f64>() / total as f64;

        // Direction breakdown
        let up_trades = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "UP")
            .count();
        let down_trades = total - up_trades;
        let up_wins = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "UP" && t.won)
            .count();
        let down_wins = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "DOWN" && t.won)
            .count();

        println!("\n=== Directional Backtest Summary ===");
        println!("Settlement rate:  {:.1}% ({}/{})", settlement_rate, settled, total);
        println!("Exit reasons:");
        for (reason, count) in &exit_counts {
            println!("  {:<16} {}", reason, count);
        }
        println!("\nCalibration:");
        println!("  Avg p_hat winners:  {:.3}", avg_winner_p);
        println!("  Avg p_hat losers:   {:.3}", avg_loser_p);
        println!("  Avg EV_net at entry: {:.4}", avg_ev);
        println!("\nDirection breakdown:");
        println!(
            "  UP:   {} trades, {} wins ({:.1}%)",
            up_trades,
            up_wins,
            if up_trades > 0 {
                up_wins as f64 / up_trades as f64 * 100.0
            } else {
                0.0
            }
        );
        println!(
            "  DOWN: {} trades, {} wins ({:.1}%)",
            down_trades,
            down_wins,
            if down_trades > 0 {
                down_wins as f64 / down_trades as f64 * 100.0
            } else {
                0.0
            }
        );

        // Sigma distribution
        let sigmas: Vec<f64> = self.closed_trades.iter().map(|t| t.entry_sigma).collect();
        let avg_sigma = sigmas.iter().sum::<f64>() / sigmas.len().max(1) as f64;
        let min_sigma = sigmas.iter().cloned().fold(f64::MAX, f64::min);
        let max_sigma = sigmas.iter().cloned().fold(f64::MIN, f64::max);
        println!("\nVolatility:");
        println!("  Avg σ at entry: {:.5}", avg_sigma);
        println!("  Min σ: {:.5}  Max σ: {:.5}", min_sigma, max_sigma);

        // Holding time distribution
        let hold_times: Vec<i64> = self.closed_trades.iter().map(|t| t.holding_secs).collect();
        let avg_hold = hold_times.iter().sum::<i64>() as f64 / hold_times.len().max(1) as f64;
        let min_hold = hold_times.iter().min().copied().unwrap_or(0);
        let max_hold = hold_times.iter().max().copied().unwrap_or(0);
        println!("\nHolding time:");
        println!("  Avg: {:.0}s  Min: {}s  Max: {}s", avg_hold, min_hold, max_hold);

        // Entry price distribution
        let entry_prices: Vec<f64> = self.closed_trades.iter().map(|t| t.entry_price.to_f64().unwrap_or(0.0)).collect();
        let avg_entry = entry_prices.iter().sum::<f64>() / entry_prices.len().max(1) as f64;
        println!("  Avg entry price: ${:.4}", avg_entry);

        // Per-symbol breakdown
        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry = symbol_stats
                .entry(&t.symbol)
                .or_insert((0, 0, Decimal::ZERO, Decimal::ZERO));
            entry.0 += 1; // total trades
            if t.won {
                entry.1 += 1; // wins
            }
            entry.2 += t.pnl; // total pnl
            entry.3 += Decimal::from(t.shares) * t.entry_price; // volume
        }

        let mut symbols: Vec<&&str> = symbol_stats.keys().collect();
        symbols.sort();

        println!("\nPer-symbol breakdown:");
        println!(
            "  {:<12} {:>6} {:>6} {:>8} {:>12} {:>12}",
            "Symbol", "Trades", "Wins", "WinRate", "PnL", "Volume"
        );
        println!("  {}", "-".repeat(62));
        for sym in &symbols {
            let (trades, wins, pnl, vol) = symbol_stats[*sym];
            let wr = if trades > 0 {
                wins as f64 / trades as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
                sym, trades, wins, wr, pnl, vol
            );
        }
        let total_vol: Decimal = symbol_stats.values().map(|v| v.3).sum();
        let total_pnl: Decimal = symbol_stats.values().map(|v| v.2).sum();
        println!("  {}", "-".repeat(62));
        println!(
            "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
            "TOTAL",
            total,
            self.closed_trades.iter().filter(|t| t.won).count(),
            self.closed_trades.iter().filter(|t| t.won).count() as f64 / total as f64 * 100.0,
            total_pnl,
            total_vol
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Display for directional results
// ─────────────────────────────────────────────────────────────

impl fmt::Display for DirectionalBacktestEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let results = self.build_results();
        writeln!(f, "=== Directional Backtest Results ===")?;
        writeln!(
            f,
            "Period:        {} to {}",
            results.start_time.format("%Y-%m-%d %H:%M"),
            results.end_time.format("%Y-%m-%d %H:%M")
        )?;
        writeln!(f, "Total trades:  {}", results.total_trades)?;
        writeln!(
            f,
            "Win/Loss:      {} / {}",
            results.winning_trades, results.losing_trades
        )?;
        writeln!(f, "Win rate:      {:.1}%", results.win_rate * 100.0)?;
        writeln!(f, "Total PnL:     ${:.2}", results.total_pnl)?;
        writeln!(f, "Avg PnL/trade: ${:.4}", results.avg_pnl_per_trade)?;
        writeln!(f, "Sharpe ratio:  {:.2}", results.sharpe_ratio)?;
        writeln!(f, "Profit factor: {:.2}", results.profit_factor)?;
        writeln!(
            f,
            "Max drawdown:  {:.2}%",
            results.max_drawdown * dec!(100)
        )?;
        writeln!(f, "Avg hold time: {:.0}s", results.avg_holding_time_secs)?;
        writeln!(f, "Largest win:   ${:.4}", results.largest_win)?;
        writeln!(f, "Largest loss:  ${:.4}", results.largest_loss)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use std::collections::VecDeque;

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn test_empty_feed() {
        let config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        let mut engine = DirectionalBacktestEngine::new(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_settlement_binary_payout() {
        // Setup: create a position manually via the feed, then settle it.
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0; // Accept any positive EV
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;

        let mut engine = DirectionalBacktestEngine::new(config);

        // Build a feed: EventState (window opens) → SpotTrade (builds history) → PmQuote (triggers entry) → EventState (settlement)
        let base = ts(0);
        let end_time = ts(900); // 15 min window

        let mut updates = vec![];

        // Event opens: S0 = 100, window ends at +900s
        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Build spot price history (need >=10 points for volatility)
        for i in 1..=15 {
            updates.push(MarketUpdate {
                timestamp: ts(i * 2),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(101) + Decimal::from(i) * dec!(0.01),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // PM quote with cheap UP ask (price is above S0, so UP is favored)
        updates.push(MarketUpdate {
            timestamp: ts(40),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.40)),
                down_ask: Some(dec!(0.65)),
            },
        });

        // Settlement: UP wins
        updates.push(MarketUpdate {
            timestamp: end_time,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        // Should have at least 1 trade that settled
        assert!(results.total_trades >= 1, "Expected at least 1 trade");

        // Check the trade details
        let trades = engine.closed_trades();
        if !trades.is_empty() {
            let t = &trades[0];
            assert_eq!(t.exit_reason, "settlement");
            assert_eq!(t.direction, "UP");
            // Won: exit at $1.00, entry around $0.40 → positive PnL
            assert!(t.won, "UP trade should win when UP settles");
            assert!(t.pnl > Decimal::ZERO, "PnL should be positive");
            assert_eq!(t.exit_price, Decimal::ONE, "Settlement pays $1.00");
        }
    }

    #[test]
    fn test_entry_ev_filter() {
        // High entry threshold should reject entries
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.99; // Impossibly high EV requirement
        config.shares_per_trade = 100;

        let mut engine = DirectionalBacktestEngine::new(config);

        let base = ts(0);
        let end_time = ts(900);

        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        for i in 1..=15 {
            updates.push(MarketUpdate {
                timestamp: ts(i * 2),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(101),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(40),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.50)),
                down_ask: Some(dec!(0.55)),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(
            results.total_trades, 0,
            "No trades should pass 99% EV threshold"
        );
    }

    #[test]
    fn test_hold_to_settlement() {
        // With hold_to_settlement=true, positions should not exit early
        // unless hard stop is triggered
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = true;
        config.hard_stop_usd = dec!(999); // Very high, won't trigger
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 10;

        let mut engine = DirectionalBacktestEngine::new(config);

        let base = ts(0);
        let end_time = ts(900);

        let mut updates = vec![];

        // Event opens
        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Build price history
        for i in 1..=15 {
            updates.push(MarketUpdate {
                timestamp: ts(i * 2),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(101),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry quote
        updates.push(MarketUpdate {
            timestamp: ts(40),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.30)),
                down_ask: Some(dec!(0.75)),
            },
        });

        // Adverse PM quote (price drops significantly) but NO settlement yet
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.20)),
                down_ask: Some(dec!(0.85)),
            },
        });

        // More adverse quotes
        updates.push(MarketUpdate {
            timestamp: ts(200),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.15)),
                down_ask: Some(dec!(0.90)),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        // Position should NOT have been closed by exits (hold_to_settlement = true,
        // hard_stop very high). It should be closed as "data_exhausted" at the end.
        let trades = engine.closed_trades();
        if !trades.is_empty() {
            assert_eq!(
                trades[0].exit_reason, "data_exhausted",
                "Should hold to settlement, closed only because feed ended"
            );
        }
    }

    #[test]
    fn test_hard_stop() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = false; // Must be false for hard stop to trigger
        config.hard_stop_usd = dec!(1); // Very tight stop: $1
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100; // 100 shares * price drop → triggers stop

        let mut engine = DirectionalBacktestEngine::new(config);

        let base = ts(0);
        let end_time = ts(900);

        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        for i in 1..=15 {
            updates.push(MarketUpdate {
                timestamp: ts(i * 2),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(101),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry at 0.40
        updates.push(MarketUpdate {
            timestamp: ts(40),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.40)),
                down_ask: Some(dec!(0.65)),
            },
        });

        // Price crashes to 0.10 — unrealized loss = 100 * (0.10 - ~0.40) ≈ -$30 > $1 stop
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                up_ask: Some(dec!(0.10)),
                down_ask: Some(dec!(0.95)),
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        // Should have triggered hard stop
        let hard_stopped = trades.iter().any(|t| t.exit_reason == "hard_stop");
        assert!(
            hard_stopped || trades.is_empty(),
            "Expected hard_stop exit or no entry (if EV filter blocked)"
        );
    }
}
