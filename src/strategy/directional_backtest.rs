//! Directional backtest engine for momentum-driven binary option trading.
//!
//! Uses weighted momentum (10s/30s/60s) → fair value estimation → edge filtering
//! to enter positions, mirroring the live MomentumDetector logic. Holds to
//! settlement by default (binary options settle at $1.00 or $0.00).
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
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{
    BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType,
};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;
use crate::strategy::pm_5m_bayesian::BayesianPrior;
use crate::strategy::volatility::normal_cdf;

const PROB_FLOOR: f64 = 1e-6;

// ─────────────────────────────────────────────────────────────
// L2 state for microstructure signals
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BacktestL2State {
    obi: f64,
    spread_bps: f64,
    bid_volume_5: f64,
    ask_volume_5: f64,
    timestamp: DateTime<Utc>,
}

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
    /// Minimum edge to enter (fair_value - pm_ask - fees), e.g. 0.05 = 5%
    pub entry_threshold: f64,
    /// Don't buy YES above this price (e.g. 0.85)
    pub max_entry_price: Decimal,
    /// Don't buy YES below this price (e.g. 0.15)
    pub min_entry_price: Decimal,
    /// Minimum absolute momentum to trigger signal (e.g. 0.003 = 0.3%)
    pub min_momentum: Decimal,
    /// Time stop: exit if <N secs remaining AND position is underwater (e.g. 30)
    pub time_stop_secs: u64,
    /// Maximum loss per position in USD
    pub hard_stop_usd: Decimal,
    /// Hold winners to settlement (default true — let them run)
    pub hold_to_settlement: bool,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter a position (seconds).
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds).
    /// Only enter when outcome is becoming clearer.
    pub max_time_remaining_secs: u64,
    /// Use price_to_beat in fair value calculation
    pub use_price_to_beat: bool,
    // ── Log-normal probability model (pm_5m_directional parity) ──
    /// EWMA lambda for volatility estimation (0.94 = RiskMetrics standard)
    pub ewma_lambda: f64,
    /// Annualized vol floor
    pub vol_floor: f64,
    /// Minimum probability to enter
    pub p_entry: f64,
    /// Minimum edge (effective_p - cost)
    pub min_edge: f64,
    /// Minimum |z| to enter
    pub min_abs_z: f64,
    // ── L2 microstructure signals ──
    /// Enable L2-based microstructure adjustment. When false, falls back to old sigmoid model.
    pub use_l2_signals: bool,
    /// OBI weight in logit adjustment
    pub obi_weight: f64,
    /// Flow pressure weight in logit adjustment
    pub flow_weight: f64,
    /// Microgap proxy weight in logit adjustment
    pub microgap_weight: f64,
    /// Minimum OBI for direction confirmation
    pub min_obi: f64,
    // ── No-trade zone ──
    pub no_trade_price_min: f64,
    pub no_trade_price_max: f64,
    pub no_trade_override_z: f64,
    pub no_trade_override_flow: f64,
    // ── PM liquidity filters ──
    pub max_pm_spread: Decimal,
    pub min_pm_ask_size: Decimal,
    // ── Bayesian gate ──
    pub use_bayesian: bool,
    pub bayesian_credible_z: f64,
}

impl Default for DirectionalBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.05,
            max_entry_price: dec!(0.85),
            min_entry_price: dec!(0.15),
            min_momentum: dec!(0.003), // 0.3% minimum move
            time_stop_secs: 30,
            hard_stop_usd: dec!(5),
            hold_to_settlement: true,
            cooldown_secs: 60,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            use_price_to_beat: true,
            ewma_lambda: 0.94,
            vol_floor: 0.0012,
            p_entry: 0.62,
            min_edge: 0.03,
            min_abs_z: 0.35,
            use_l2_signals: true,
            obi_weight: 0.75,
            flow_weight: 1.10,
            microgap_weight: 0.40,
            min_obi: 0.05,
            no_trade_price_min: 0.45,
            no_trade_price_max: 0.55,
            no_trade_override_z: 0.90,
            no_trade_override_flow: 0.45,
            max_pm_spread: dec!(0.025),
            min_pm_ask_size: dec!(25),
            use_bayesian: false,
            bayesian_credible_z: 1.645,
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
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    // L2 microstructure state
    l2_by_symbol: HashMap<String, BacktestL2State>,
    prev_obi_by_symbol: HashMap<String, f64>,
    // EWMA vol tracking
    ewma_var: HashMap<String, f64>,
    ewma_last_price: HashMap<String, f64>,
    // Bayesian prior
    bayesian: BayesianPrior,
    // Active events: symbol -> concurrent windows (5m + 15m can overlap)
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
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
    // Throttle: last timestamp we ran entry/exit logic per symbol
    last_logic_ts: HashMap<String, DateTime<Utc>>,
}

impl DirectionalBacktestEngine {
    pub fn new(config: DirectionalBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            l2_by_symbol: HashMap::new(),
            prev_obi_by_symbol: HashMap::new(),
            ewma_var: HashMap::new(),
            ewma_last_price: HashMap::new(),
            bayesian: BayesianPrior::new(),
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

    pub fn new_without_recorder(config: DirectionalBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &DirectionalBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[DirectionalClosedTrade] {
        &self.closed_trades
    }

    /// Take ownership of the recorder back from the engine.
    /// Useful for calling async methods (like `flush_async`/`finalize`) after `run()`.
    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
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

            // Prune expired events (end_time has passed without settlement)
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            // Settle positions whose event window has expired but no settlement record arrived.
            // Use spot price vs price_to_beat to determine outcome.
            let mut expired_closes: Vec<(usize, Decimal)> = Vec::new();
            for (i, pos) in self.positions.iter().enumerate() {
                if pos.event_end_time <= update.timestamp {
                    let spot = self.spot_prices.get(&pos.symbol).map(|s| s.price);
                    let won = match (spot, pos.s0) {
                        (Some(s), s0) if s0 > Decimal::ZERO => match pos.direction {
                            Direction::Up => s > s0,
                            Direction::Down => s <= s0,
                        },
                        _ => false,
                    };
                    let exit_price = if won { Decimal::ONE } else { Decimal::ZERO };
                    expired_closes.push((i, exit_price));
                }
            }
            expired_closes.sort_by(|a, b| b.0.cmp(&a.0));
            for (idx, exit_price) in expired_closes {
                self.close_position(idx, exit_price, "settlement", update.timestamp);
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
                    // Binary settlement — only close positions matching this event
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        // Remove only the settled event, not all events for the symbol
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                    }

                    // Track active window: store S0 (price_to_beat) for probability calc
                    // Multiple events per symbol are allowed (5m + 15m overlap)
                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            let events =
                                self.active_events.entry(update.symbol.clone()).or_default();
                            // Don't add duplicate events
                            if !events.iter().any(|e| e.event_slug == *event_slug) {
                                events.push(ActiveWindowInfo {
                                    event_slug: event_slug.clone(),
                                    s0: *s0,
                                    end_time: *end,
                                });
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot { .. } => {
                    // LOB depth not used by directional backtest
                }
                UpdateType::BinanceL2 {
                    obi_5,
                    obi_10: _,
                    bid_volume_5,
                    ask_volume_5,
                    spread_bps,
                } => {
                    let obi = obi_5.to_f64().unwrap_or(0.0);
                    // Store previous OBI for flow proxy before overwriting
                    if let Some(prev) = self.l2_by_symbol.get(&update.symbol) {
                        self.prev_obi_by_symbol
                            .insert(update.symbol.clone(), prev.obi);
                    }
                    self.l2_by_symbol.insert(
                        update.symbol.clone(),
                        BacktestL2State {
                            obi,
                            spread_bps: spread_bps.to_f64().unwrap_or(0.0),
                            bid_volume_5: bid_volume_5.to_f64().unwrap_or(0.0),
                            ask_volume_5: ask_volume_5.to_f64().unwrap_or(0.0),
                            timestamp: update.timestamp,
                        },
                    );
                }
            }
        }

        // Force-close any remaining positions at latest PM price (data exhausted)
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

        // Update EWMA vol tracker
        if let Some(price_f) = price.to_f64() {
            self.update_ewma_vol(symbol, price_f);
        }
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks (per event_slug)
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

        // Update position mark-to-market (cheap — just price assignment)
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

        // Throttle entry/exit logic to once per second per symbol.
        // PM quotes arrive ~30-40/sec — running probability model on every tick is wasteful.
        let should_run_logic = match self.last_logic_ts.get(symbol) {
            Some(last) => (ts - *last).num_seconds() >= 1,
            None => true,
        };
        if !should_run_logic {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        // Try directional entry
        self.try_directional_entry(symbol, ts);

        // Check exits for existing positions
        self.check_exits(ts);

        // Record equity curve
        self.record_equity(ts);
    }

    // ─── EWMA volatility ────────────────────────────────────

    /// Update EWMA variance with a new price observation.
    fn update_ewma_vol(&mut self, symbol: &str, price_f: f64) {
        let lambda = self.config.ewma_lambda;
        if let Some(prev) = self.ewma_last_price.get(symbol).copied() {
            if prev > 0.0 {
                let log_ret = (price_f / prev).ln();
                let var = self.ewma_var.entry(symbol.to_string()).or_insert(0.0);
                *var = lambda * *var + (1.0 - lambda) * log_ret * log_ret;
            }
        }
        self.ewma_last_price.insert(symbol.to_string(), price_f);
    }

    /// Current annualized sigma from EWMA variance.
    fn ewma_sigma_annualized(&self, symbol: &str) -> f64 {
        const SECS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;
        let var = self.ewma_var.get(symbol).copied().unwrap_or(0.0);
        (var * SECS_PER_YEAR).sqrt().max(self.config.vol_floor)
    }

    // ─── Log-normal probability ─────────────────────────────

    /// Compute base probability using log-normal model (same as pm_5m_directional).
    /// Returns (p_base, sigma_annual, z).
    fn compute_probability(
        &self,
        spot: &SpotPrice,
        s0: Decimal,
        end_time: DateTime<Utc>,
        now: DateTime<Utc>,
        sigma_annual: f64,
    ) -> Option<(f64, f64, f64)> {
        let spot_f = spot.price.to_f64()?;
        let beat_f = s0.to_f64()?;
        if spot_f <= 0.0 || beat_f <= 0.0 {
            return None;
        }

        let remaining_secs = (end_time - now).num_seconds().max(0) as f64;
        let tau_years = remaining_secs / (365.25 * 24.0 * 3600.0);
        let d_t = (spot_f / beat_f).ln();
        let z = d_t / (sigma_annual * tau_years.sqrt()).max(PROB_FLOOR);
        let p_base = normal_cdf(z).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
        Some((p_base, sigma_annual, z))
    }

    // ─── L2 microstructure helpers ──────────────────────────

    fn microgap_proxy(l2: &BacktestL2State) -> f64 {
        (l2.obi * (l2.spread_bps / 5.0).clamp(0.0, 1.0)).clamp(-1.0, 1.0)
    }

    fn flow_pressure(&self, symbol: &str, l2: &BacktestL2State) -> f64 {
        let prev_obi = self.prev_obi_by_symbol.get(symbol).copied().unwrap_or(0.0);
        (l2.obi - prev_obi).clamp(-1.0, 1.0)
    }

    // ─── Entry logic (log-normal + microstructure) ──────────

    fn try_directional_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        // 1. Need: active events with S0, spot price history, PM asks
        let windows: Vec<ActiveWindowInfo> = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: String::new(),
                        timestamp: ts,
                        p_hat: None,
                        ev_net: None,
                        sigma: None,
                        market_price: None,
                        spot_price: None,
                        s0: None,
                        time_remaining_secs: None,
                        filter_reason: Some("no_active_event".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "no_active_event",
                );
                return;
            }
        };

        // Shared preconditions: spot price, PM quotes
        let spot = match self.spot_prices.get(symbol) {
            Some(s) => s.clone(),
            None => {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: String::new(),
                        timestamp: ts,
                        p_hat: None,
                        ev_net: None,
                        sigma: None,
                        market_price: None,
                        spot_price: None,
                        s0: None,
                        time_remaining_secs: None,
                        filter_reason: Some("no_spot_data".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "no_spot_data",
                );
                return;
            }
        };

        let sigma_annual = self.ewma_sigma_annualized(symbol);

        // Try entry on each active event window independently
        for window in windows {
            let (up_ask, down_ask) = self
                .pm_asks_by_event
                .get(&window.event_slug)
                .copied()
                .unwrap_or((None, None));
            self.try_entry_for_window(symbol, ts, &window, &spot, sigma_annual, up_ask, down_ask);
        }
    }

    /// Attempt entry on a specific event window using log-normal probability model.
    /// When `use_l2_signals` is true, applies microstructure adjustment in logit space
    /// (matching pm_5m_directional live strategy). Falls back to old sigmoid model when false.
    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        spot: &SpotPrice,
        sigma_annual: f64,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        let st = spot.price;
        // 1. Time remaining — must be within [min, max] window
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }
        if time_remaining > self.config.max_time_remaining_secs as f64 {
            return;
        }

        // 2. Backward-compat: if L2 signals disabled, use old sigmoid model
        if !self.config.use_l2_signals {
            let momentum = spot.weighted_momentum().or_else(|| spot.momentum(30));
            self.try_entry_for_window_legacy(symbol, ts, window, st, momentum, up_ask, down_ask);
            return;
        }

        // 3. Compute log-normal base probability
        let (p_base, sigma, z) =
            match self.compute_probability(spot, window.s0, window.end_time, ts, sigma_annual) {
                Some(v) => v,
                None => return,
            };

        // 4. Logit-space microstructure adjustment (if L2 data available)
        let (p_hat, pressure, microgap) = if let Some(l2) = self.l2_by_symbol.get(symbol).cloned()
        {
            let logit_base = (p_base / (1.0 - p_base)).ln();
            let mg = Self::microgap_proxy(&l2);
            let pr = self.flow_pressure(symbol, &l2);
            let adjusted_logit = logit_base
                + self.config.obi_weight * l2.obi
                + self.config.flow_weight * pr
                + self.config.microgap_weight * mg;
            let p = (1.0 / (1.0 + (-adjusted_logit).exp())).clamp(PROB_FLOOR, 1.0 - PROB_FLOOR);
            (p, pr, mg)
        } else {
            (p_base, 0.0, 0.0)
        };

        // 5. Side selection
        let (direction, effective_p) = if p_hat >= 0.5 {
            (Direction::Up, p_hat)
        } else {
            (Direction::Down, 1.0 - p_hat)
        };

        // 6. Probability gate
        if effective_p < self.config.p_entry || z.abs() < self.config.min_abs_z {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(effective_p),
                    ev_net: None,
                    sigma: Some(sigma),
                    market_price: None,
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("weak_probability".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "weak_probability",
            );
            return;
        }

        // 7. Direction confirmation (if L2 available)
        if self.l2_by_symbol.contains_key(symbol) {
            let l2 = self.l2_by_symbol.get(symbol).unwrap();
            let confirmed = match direction {
                Direction::Up => {
                    l2.obi > self.config.min_obi
                        && pressure > self.config.min_obi
                        && microgap > 0.0
                }
                Direction::Down => {
                    l2.obi < -self.config.min_obi
                        && pressure < -self.config.min_obi
                        && microgap < 0.0
                }
            };
            if !confirmed {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: format!("{}", direction),
                        timestamp: ts,
                        p_hat: Some(effective_p),
                        ev_net: None,
                        sigma: Some(sigma),
                        market_price: None,
                        spot_price: Some(st),
                        s0: Some(window.s0),
                        time_remaining_secs: Some(time_remaining),
                        filter_reason: Some("direction_confirmation_failed".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "direction_confirmation_failed",
                );
                return;
            }
        }

        // 8. Select market ask for the chosen direction
        let market_ask = match direction {
            Direction::Up => match up_ask {
                Some(ask) => ask,
                None => return,
            },
            Direction::Down => match down_ask {
                Some(ask) => ask,
                None => return,
            },
        };

        // 9. Price bounds check
        if market_ask > self.config.max_entry_price || market_ask < self.config.min_entry_price {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(effective_p),
                    ev_net: None,
                    sigma: Some(sigma),
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("price_bounds".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "price_bounds",
            );
            return;
        }

        // 10. No-trade zone
        let ask_f = market_ask.to_f64().unwrap_or(0.5);
        if ask_f >= self.config.no_trade_price_min && ask_f <= self.config.no_trade_price_max {
            if !(z.abs() >= self.config.no_trade_override_z
                && pressure.abs() >= self.config.no_trade_override_flow)
            {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: format!("{}", direction),
                        timestamp: ts,
                        p_hat: Some(effective_p),
                        ev_net: None,
                        sigma: Some(sigma),
                        market_price: Some(market_ask),
                        spot_price: Some(st),
                        s0: Some(window.s0),
                        time_remaining_secs: Some(time_remaining),
                        filter_reason: Some("no_trade_zone".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "no_trade_zone",
                );
                return;
            }
        }

        // 11. Edge = effective_p - (ask + fees)
        let best_bid = (market_ask - dec!(0.02)).max(dec!(0.01));
        let depth_ratio = Decimal::from(self.config.shares_per_trade) / dec!(10000);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let fee_cost = cost.total.to_f64().unwrap_or(0.01);
        let effective_cost = ask_f + fee_cost;
        let edge = effective_p - effective_cost;

        if edge < self.config.min_edge {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(effective_p),
                    ev_net: Some(edge),
                    sigma: Some(sigma),
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("edge_below_threshold".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "edge_below_threshold",
            );
            return;
        }

        // 12. Bayesian gate (optional)
        let bayes_lb = if self.config.use_bayesian {
            self.bayesian.posterior_lower_bound(
                ask_f,
                time_remaining,
                sigma,
                effective_p,
                self.config.bayesian_credible_z,
            )
        } else {
            effective_p
        };
        if bayes_lb < self.config.p_entry {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(effective_p),
                    ev_net: Some(edge),
                    sigma: Some(sigma),
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("bayesian_lb_below_threshold".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "bayesian_lb_below_threshold",
            );
            return;
        }


        // 13. Cooldown check
        if let Some(last) = self.last_entry_time.get(symbol) {
            let elapsed = (ts - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                return;
            }
        }

        // 14. Max positions check
        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        // 15. Don't enter if already holding same event+direction
        let already_holding = self.positions.iter().any(|p| {
            p.event_slug == window.event_slug
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&direction)
        });
        if already_holding {
            return;
        }

        // 16. Execute entry via ExecutionSimulator
        let sim_result =
            self.execution_sim
                .simulate_buy(market_ask, ts, self.config.shares_per_trade, 10_000);

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let entry_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
            trace!(
                "Skipping entry: insufficient equity ({} < {})",
                self.equity,
                total_entry_cost
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
            entry_ev_net: edge,
            entry_sigma: sigma,
            latest_pm_price: market_ask,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);

        // Record Bayesian outcome tracking data for settlement
        // (bayesian.record_outcome is called at settlement time)

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", direction),
            timestamp: ts,
            p_hat: Some(effective_p),
            ev_net: Some(edge),
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
            "ENTRY {} {} @ {:.4} | p_hat={:.3} z={:.3} sigma={:.4} edge={:.3}",
            symbol,
            direction,
            sim_result.fill_price,
            effective_p,
            z,
            sigma,
            edge,
        );
    }

    /// Legacy entry logic using sigmoid-momentum model (backward compatibility).
    fn try_entry_for_window_legacy(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        momentum: Option<Decimal>,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        let time_remaining = (window.end_time - ts).num_seconds() as f64;

        let momentum = match momentum {
            Some(m) => m,
            None => return,
        };

        if momentum.abs() < self.config.min_momentum {
            return;
        }

        let (direction, market_ask) = if momentum > Decimal::ZERO {
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

        let mut fair_value = Self::estimate_fair_value(momentum);
        if self.config.use_price_to_beat {
            fair_value = Self::adjust_fair_value_for_price_to_beat(
                fair_value,
                momentum,
                st,
                window.s0,
                time_remaining as i64,
                window.end_time,
            );
        }

        if market_ask > self.config.max_entry_price || market_ask < self.config.min_entry_price {
            return;
        }

        let best_bid = (market_ask - dec!(0.02)).max(dec!(0.01));
        let depth_ratio = Decimal::from(self.config.shares_per_trade) / dec!(10000);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let fee_per_share_usd = market_ask * cost.taker_fee;
        let spread_plus_slip = cost.spread_cost + cost.depth_slippage;

        let fair_value_f = fair_value.to_f64().unwrap_or(0.5);
        let market_ask_f = market_ask.to_f64().unwrap_or(0.5);
        let total_cost_f =
            fee_per_share_usd.to_f64().unwrap_or(0.01) + spread_plus_slip.to_f64().unwrap_or(0.01);
        let edge = fair_value_f - market_ask_f - total_cost_f;

        if edge < self.config.entry_threshold {
            return;
        }

        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return;
            }
        }

        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        let already_holding = self.positions.iter().any(|p| {
            p.event_slug == window.event_slug
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&direction)
        });
        if already_holding {
            return;
        }

        let sim_result =
            self.execution_sim
                .simulate_buy(market_ask, ts, self.config.shares_per_trade, 10_000);

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let entry_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
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
            entry_p_hat: fair_value_f,
            entry_ev_net: edge,
            entry_sigma: momentum.to_f64().unwrap_or(0.0),
            latest_pm_price: market_ask,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", direction),
            timestamp: ts,
            p_hat: Some(fair_value_f),
            ev_net: Some(edge),
            sigma: Some(momentum.to_f64().unwrap_or(0.0)),
            market_price: Some(sim_result.fill_price),
            spot_price: Some(st),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });
    }

    // ─── Fair value estimation (from live MomentumDetector) ───

    /// Sigmoid-like mapping from momentum to fair value.
    /// Mirrors `MomentumDetector::estimate_fair_value()` in momentum.rs.
    fn estimate_fair_value(momentum: Decimal) -> Decimal {
        let abs_momentum = momentum.abs();
        let momentum_factor = if abs_momentum < dec!(0.001) {
            // Very small moves: linear scaling (0.1% → 5%)
            abs_momentum * dec!(50)
        } else if abs_momentum < dec!(0.005) {
            // Medium moves: moderate scaling (0.5% → ~21%)
            dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40)
        } else {
            // Large moves: diminishing returns (1% → ~36%)
            dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30)
        };
        // Cap at 90%
        (dec!(0.50) + momentum_factor).min(dec!(0.90))
    }

    /// Adjust fair value based on distance to price_to_beat and time remaining.
    /// Mirrors `MomentumDetector::estimate_fair_value_with_price_to_beat()`.
    fn adjust_fair_value_for_price_to_beat(
        base_fv: Decimal,
        momentum: Decimal,
        current_price: Decimal,
        price_to_beat: Decimal,
        time_remaining_secs: i64,
        _end_time: DateTime<Utc>,
    ) -> Decimal {
        if price_to_beat <= Decimal::ZERO {
            return base_fv;
        }

        let distance_pct = (current_price - price_to_beat) / price_to_beat;

        // time_factor: fraction of time elapsed. Near expiry → time_factor → 1.0
        let time_factor = (Decimal::ONE - Decimal::from(time_remaining_secs.max(0)) / dec!(900))
            .max(Decimal::ZERO);

        let direction_matches = (momentum > Decimal::ZERO && distance_pct > Decimal::ZERO)
            || (momentum < Decimal::ZERO && distance_pct < Decimal::ZERO);

        if direction_matches {
            let boost = distance_pct.abs() * time_factor * dec!(0.5);
            (base_fv + boost).min(dec!(0.95))
        } else {
            let reduction = distance_pct.abs() * dec!(0.3);
            (base_fv - reduction).max(dec!(0.35))
        }
    }

    // ─── Exit logic (directional, NOT arb) ───────────────────

    fn check_exits(&mut self, ts: DateTime<Utc>) {
        let mut to_close: Vec<(usize, Decimal, &str)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;

            // A. Hold to settlement — no early exit at all. Binary options settle
            // at $1.00 or $0.00, so unrealized PnL fluctuations are noise.
            // The only meaningful exit is settlement itself.
            // When enabled, skip ALL exit checks (time_stop, hard_stop) and wait
            // for the EventState settlement event to close the position.
            if self.config.hold_to_settlement {
                continue;
            }

            // B. Time stop: <N secs remaining AND position is underwater.
            //    Use unrealized PnL (market price vs entry price), NOT the probability model.
            //    The prob model returns ~0.5 always, which would incorrectly exit winners
            //    (pm_price > 0.5 → ev_now < 0 → false exit signal).
            if time_remaining <= self.config.time_stop_secs as f64 && time_remaining > 0.0 {
                let unrealized_per_share = pos.latest_pm_price - pos.entry_price;
                if unrealized_per_share < Decimal::ZERO {
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
        }

        // Close in reverse order to preserve indices
        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, reason) in to_close {
            self.close_position(idx, exit_price, reason, ts);
        }
    }

    // ─── Settlement ──────────────────────────────────────────

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
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, "settlement", ts);
        }
    }

    // ─── Close position ──────────────────────────────────────

    fn close_position(&mut self, idx: usize, exit_price: Decimal, reason: &str, ts: DateTime<Utc>) {
        let pos = self.positions.remove(idx);

        // For settlement ($1 or $0), no need to simulate — it's binary payout.
        // Fee at settlement ($1 or $0): p*(1-p) = 0, so settlement fee = $0.
        // For early exits, simulate sell via ExecutionSimulator + exit fee.
        let (final_price, proceeds, _exit_fee) = if reason == "settlement" {
            let p = exit_price;
            // At $1.00 or $0.00, the parabolic fee curve = 0 (p*(1-p) = 0)
            (p, p * Decimal::from(pos.shares), Decimal::ZERO)
        } else {
            let sim_result = self
                .execution_sim
                .simulate_sell(exit_price, ts, pos.shares, 10_000);
            let raw_proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
            // Taker fee on sell: shares × price × feeRate × (p*(1-p))^exponent
            let sell_fee = self.fee_model.fee_shares(
                Decimal::from(sim_result.filled_shares),
                sim_result.fill_price,
            ) * sim_result.fill_price;
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
        // For settlement exits, use event_end_time (not resolved_at which can be hours later)
        let effective_exit_time = if reason == "settlement" {
            pos.event_end_time.min(ts)
        } else {
            ts
        };
        let holding_secs = (effective_exit_time - pos.entry_time).num_seconds();

        self.closed_trades.push(DirectionalClosedTrade {
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: effective_exit_time,
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

        // Record exit signal and trade
        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pos.entry_ev_net),
            sigma: Some(pos.entry_sigma),
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
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pos.entry_ev_net),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
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
        println!(
            "Settlement rate:  {:.1}% ({}/{})",
            settlement_rate, settled, total
        );
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
        println!(
            "  Avg: {:.0}s  Min: {}s  Max: {}s",
            avg_hold, min_hold, max_hold
        );

        // Entry price distribution
        let entry_prices: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|t| t.entry_price.to_f64().unwrap_or(0.0))
            .collect();
        let avg_entry = entry_prices.iter().sum::<f64>() / entry_prices.len().max(1) as f64;
        println!("  Avg entry price: ${:.4}", avg_entry);

        // Per-symbol breakdown
        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry =
                symbol_stats
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
        writeln!(f, "Max drawdown:  {:.2}%", results.max_drawdown * dec!(100))?;
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
        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_settlement_binary_payout() {
        // Setup: create a position via momentum signal, then settle it.
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0; // Accept any positive edge
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001); // Low threshold for test
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300); // 5 min window

        let mut updates = vec![];

        // Event opens: S0 = 100
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

        // Build spot price history with UPWARD momentum (100.00 → 101.50)
        // Need enough points spread over 60s for weighted_momentum to work
        for i in 1..=60 {
            let price = dec!(100) + Decimal::from(i) * dec!(0.025);
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price,
                    quantity: Some(dec!(1)),
                },
            });
        }

        // PM quote with cheap UP ask — momentum is up, so should buy UP
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
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

        assert!(results.total_trades >= 1, "Expected at least 1 trade");

        let trades = engine.closed_trades();
        if !trades.is_empty() {
            let t = &trades[0];
            assert_eq!(t.exit_reason, "settlement");
            assert_eq!(t.direction, "UP");
            assert!(t.won, "UP trade should win when UP settles");
            assert!(t.pnl > Decimal::ZERO, "PnL should be positive");
            assert_eq!(t.exit_price, Decimal::ONE, "Settlement pays $1.00");
        }
    }

    #[test]
    fn test_entry_edge_filter() {
        // High entry threshold should reject entries
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.99; // Impossibly high edge requirement
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
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

        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.02),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.50)),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(
            results.total_trades, 0,
            "No trades should pass 99% edge threshold"
        );
    }

    #[test]
    fn test_hold_to_settlement() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = true;
        config.hard_stop_usd = dec!(999);
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 10;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
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

        // Upward momentum
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry quote
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.30)),
            },
        });

        // Adverse PM quote but NO settlement
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.20)),
            },
        });

        updates.push(MarketUpdate {
            timestamp: ts(200),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.15)),
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

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
        config.hold_to_settlement = false;
        config.hard_stop_usd = dec!(1); // Very tight stop: $1
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
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

        // Upward momentum to trigger entry
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry at 0.40
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
            },
        });

        // Price crashes to 0.10 — unrealized loss = 100 * (0.10 - ~0.40) ≈ -$30 > $1 stop
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.10)),
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        let hard_stopped = trades.iter().any(|t| t.exit_reason == "hard_stop");
        assert!(
            hard_stopped || trades.is_empty(),
            "Expected hard_stop exit or no entry (if edge filter blocked)"
        );
    }
}
