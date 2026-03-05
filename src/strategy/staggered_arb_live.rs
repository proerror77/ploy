//! Staggered Arbitrage Live Adapter — 時間差套利
//!
//! Wraps the staggered arb backtest signal logic into the event-driven `Strategy`
//! trait interface. Supports both paper trading (dry-run) and live order submission.
//!
//! The core idea: buy the side predicted to get expensive first (Leg1), then buy
//! the opposite side after price movement (Leg2). When both legs are filled,
//! the cycle is considered complete and new cycles can start. If Leg2 doesn't
//! fill profitably, force-complete to bound losses.
//!
//! Usage:
//!   ploy strategy start staggered_arb --config config/strategies/staggered_arb.toml
//!   ploy strategy start staggered_arb --dry-run --config config/strategies/staggered_arb.toml

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use super::momentum::Direction;
use super::probability::estimate_probability;
use super::staggered_arb_backtest::StaggeredArbBacktestConfig;
use super::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};
use crate::adapters::SpotPrice;
use crate::domain::order::OrderRequest;
use crate::domain::{OrderStatus, Side};
use crate::error::Result;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Live adapter configuration — wraps the backtest config + paper fee rate.
#[derive(Debug, Clone)]
pub struct StaggeredArbLiveConfig {
    pub backtest_config: StaggeredArbBacktestConfig,
    /// Fixed fee rate for paper fills (default 1.5%)
    pub fee_rate: Decimal,
}

fn default_staggered_series_ids() -> Vec<String> {
    vec![
        "10684".into(),
        "10683".into(),
        "10686".into(),
        "10685".into(), // 5m
        "10192".into(),
        "10191".into(),
        "10423".into(),
        "10422".into(), // 15m
    ]
}

// ─────────────────────────────────────────────────────────────
// Internal types
// ─────────────────────────────────────────────────────────────

/// An active event window being monitored for entry signals.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct LiveWindow {
    event_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    condition_id: Option<String>,
    end_time: DateTime<Utc>,
    open_price: Option<Decimal>,
    window_secs: u64,
}

/// Paper position state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaperPositionState {
    Leg1Filled,
    Merged,
    ForcedComplete,
}

/// A paper position tracking the two-leg arb lifecycle.
#[derive(Debug, Clone)]
struct PaperPosition {
    symbol: String,
    event_id: String,
    condition_id: Option<String>,
    up_token: String,
    down_token: String,
    leg1_direction: Direction,
    leg1_price: Decimal,
    leg1_shares: u64,
    leg1_fee: Decimal,
    leg1_time: DateTime<Utc>,
    wait_deadline: DateTime<Utc>,
    leg2_price: Option<Decimal>,
    leg2_shares: Option<u64>,
    leg2_fee: Option<Decimal>,
    leg2_time: Option<DateTime<Utc>>,
    state: PaperPositionState,
}

/// A closed paper trade for summary reporting.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct PaperTrade {
    symbol: String,
    event_id: String,
    direction: Direction,
    leg1_price: Decimal,
    leg2_price: Decimal,
    total_cost: Decimal,
    payout: Decimal,
    pnl: Decimal,
    exit_reason: String,
    duration_secs: i64,
    opened_at: DateTime<Utc>,
    closed_at: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
// Live order tracking
// ─────────────────────────────────────────────────────────────

/// Tracks an in-flight order for the live (non-paper) path.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct LiveOrderTrack {
    event_id: String,
    condition_id: Option<String>,
    symbol: String,
    up_token: String,
    down_token: String,
    direction: Direction,
    token_id: String,
    leg: u8, // 1 or 2
    price: Decimal,
    shares: u64,
    /// For Leg2: index into self.positions
    position_idx: Option<usize>,
    /// How Leg2 was triggered: merge vs forced_*
    close_reason: Option<String>,
    /// When this order was submitted (for stale order detection)
    submitted_at: DateTime<Utc>,
    /// When we sent a cancel request (None = not yet requested)
    cancel_requested_at: Option<DateTime<Utc>>,
}

// ─────────────────────────────────────────────────────────────
// Adapter
// ─────────────────────────────────────────────────────────────

pub struct StaggeredArbAdapter {
    id: String,
    config: StaggeredArbLiveConfig,
    dry_run: bool,

    // ── Market state ──
    spot_prices: HashMap<String, SpotPrice>,
    /// symbol -> latest Binance L2 OBI(top-5)
    binance_l2_obi_5: HashMap<String, Decimal>,
    /// symbol -> timestamp for latest Binance L2 OBI update
    binance_l2_obi_ts: HashMap<String, DateTime<Utc>>,
    /// symbol → (up_ask, down_ask)
    pm_asks: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    /// token_id → (symbol, Direction)
    token_to_symbol: HashMap<String, (String, Direction)>,

    // ── Event windows ──
    active_windows: HashMap<String, Vec<LiveWindow>>,
    /// Polymarket series IDs subscribed by this strategy instance.
    series_ids: Vec<String>,

    // ── Paper positions ──
    positions: Vec<PaperPosition>,
    closed_trades: Vec<PaperTrade>,
    equity: Decimal,
    initial_capital: Decimal,

    // ── Cooldowns ──
    cooldowns: HashMap<String, DateTime<Utc>>,
    event_trade_counts: HashMap<String, usize>,

    // ── Periodic summary ──
    last_summary: Option<DateTime<Utc>>,

    // ── Live order tracking (used when dry_run = false) ──
    /// client_order_id → order tracking info
    live_orders: HashMap<String, LiveOrderTrack>,
    /// Events with in-flight Leg1 orders (prevents duplicate entries)
    pending_leg1_events: HashSet<String>,
    /// Position indices with in-flight Leg2 orders (prevents duplicate Leg2)
    pending_leg2_positions: HashSet<usize>,
    /// Fixed USD amount per trade (overrides shares_per_trade when set)
    fixed_amount_usd: Option<f64>,
    /// Keep at least this amount available before opening Leg1.
    min_balance_usd: Decimal,
    /// Warn once when fixed notional is inflated by Polymarket's minimum share rule.
    fixed_amount_overage_warned: bool,

    // ── Balance management ──
    /// Consecutive balance-related failures
    consecutive_balance_failures: u32,
    /// Pause new entries until this time (waiting for claimer to free funds)
    balance_pause_until: Option<DateTime<Utc>>,
    /// Entry gate reject counters (why Leg1 was skipped)
    entry_reject_counts: HashMap<String, u64>,
    /// Leg2 skip counters (why close was skipped/deferred)
    leg2_skip_counts: HashMap<String, u64>,
}

impl StaggeredArbAdapter {
    pub fn new(id: String, config: StaggeredArbLiveConfig, dry_run: bool) -> Self {
        let initial_capital = config.backtest_config.initial_capital;
        Self {
            id,
            config,
            dry_run,
            spot_prices: HashMap::new(),
            binance_l2_obi_5: HashMap::new(),
            binance_l2_obi_ts: HashMap::new(),
            pm_asks: HashMap::new(),
            token_to_symbol: HashMap::new(),
            active_windows: HashMap::new(),
            series_ids: default_staggered_series_ids(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity: initial_capital,
            initial_capital,
            cooldowns: HashMap::new(),
            event_trade_counts: HashMap::new(),
            last_summary: None,
            live_orders: HashMap::new(),
            pending_leg1_events: HashSet::new(),
            pending_leg2_positions: HashSet::new(),
            fixed_amount_usd: None,
            min_balance_usd: Decimal::ZERO,
            fixed_amount_overage_warned: false,
            consecutive_balance_failures: 0,
            balance_pause_until: None,
            entry_reject_counts: HashMap::new(),
            leg2_skip_counts: HashMap::new(),
        }
    }

    fn bump_entry_reject(&mut self, reason: &str) {
        *self.entry_reject_counts.entry(reason.to_string()).or_default() += 1;
    }

    fn bump_leg2_skip(&mut self, reason: &str) {
        *self.leg2_skip_counts.entry(reason.to_string()).or_default() += 1;
    }

    /// Create from TOML configuration string.
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty = Value::Table(Default::default());
        let entry = config.get("entry").unwrap_or(&empty);
        let timing = config.get("timing").unwrap_or(&empty);
        let risk = config.get("risk").unwrap_or(&empty);
        let model = config.get("model").unwrap_or(&empty);
        let filter = config.get("filter").unwrap_or(&empty);
        let markets = config.get("markets").unwrap_or(&empty);

        let symbols: Vec<String> = entry
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec!["BTCUSDT".into(), "ETHUSDT".into()]);

        let bc = StaggeredArbBacktestConfig {
            symbols,
            initial_capital: Decimal::try_from(
                entry
                    .get("initial_capital")
                    .and_then(|v| v.as_float())
                    .unwrap_or(10000.0),
            )
            .unwrap_or(dec!(10000)),
            shares_per_trade: entry
                .get("shares_per_trade")
                .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
                .unwrap_or(20) as u64,
            max_concurrent_positions: entry
                .get("max_concurrent")
                .and_then(|v| v.as_integer())
                .unwrap_or(5) as usize,
            direction_threshold: entry
                .get("direction_threshold")
                .and_then(|v| v.as_float())
                .unwrap_or(0.03),
            reverse_signal: entry
                .get("reverse_signal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            max_initial_sum: Decimal::try_from(
                entry
                    .get("max_initial_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.20),
            )
            .unwrap_or(dec!(1.20)),
            max_leg1_price: Decimal::try_from(
                entry
                    .get("max_leg1_price")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.80),
            )
            .unwrap_or(dec!(0.80)),
            merge_target_sum: Decimal::try_from(
                entry
                    .get("merge_target_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.95),
            )
            .unwrap_or(dec!(0.95)),
            min_profit_target: Decimal::try_from(
                entry
                    .get("min_profit_target")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.02),
            )
            .unwrap_or(dec!(0.02)),
            max_wait_secs: timing
                .get("max_wait_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(180) as u64,
            entry_after_start_max_secs: timing
                .get("entry_after_start_max_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(120) as u64,
            no_trade_last_secs: timing
                .get("no_trade_last_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(60) as u64,
            max_wait_pct: timing
                .get("max_wait_pct")
                .and_then(|v| v.as_float())
                .unwrap_or(0.40),
            min_time_remaining_secs: timing
                .get("min_time_remaining")
                .and_then(|v| v.as_integer())
                .unwrap_or(60) as u64,
            max_leg1_loss: Decimal::try_from(
                risk.get("max_leg1_loss")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.0),
            )
            .unwrap_or(Decimal::ZERO),
            force_complete_threshold: Decimal::try_from(
                risk.get("force_complete_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.00),
            )
            .unwrap_or(Decimal::ONE),
            min_ask_price: Decimal::try_from(
                entry
                    .get("min_ask_price")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.05),
            )
            .unwrap_or(dec!(0.05)),
            min_entry_sum: Decimal::try_from(
                entry
                    .get("min_entry_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.70),
            )
            .unwrap_or(dec!(0.70)),
            allowed_window_durations: filter
                .get("allowed_windows")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_integer().map(|i| i as u64))
                        .collect()
                })
                .unwrap_or_else(|| vec![300, 900]),
            window_duration_tolerance: filter
                .get("window_tolerance")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
            min_leg2_delay_secs: timing
                .get("min_leg2_delay_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(3) as u64,
            max_trades_per_event: timing
                .get("max_trades_per_event")
                .and_then(|v| v.as_integer())
                .unwrap_or(2) as usize,
            mu: model.get("mu").and_then(|v| v.as_float()).unwrap_or(0.0),
            vol_lookback_secs: model
                .get("vol_lookback_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(300) as u64,
            vol_floor: model
                .get("vol_floor")
                .and_then(|v| v.as_float())
                .unwrap_or(0.005),
            cooldown_secs: timing
                .get("cooldown_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(5) as u64,
            // Greeks integration — read from TOML [model] section
            use_greeks: model
                .get("use_greeks")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            min_gamma: model
                .get("min_gamma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
            max_theta_cost: model
                .get("max_theta_cost")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
            delta_weighted_sizing: model
                .get("delta_weighted_sizing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };

        let fee_rate = Decimal::try_from(
            entry
                .get("fee_rate")
                .and_then(|v| v.as_float())
                .unwrap_or(0.015),
        )
        .unwrap_or(dec!(0.015));

        let fixed_amount_usd = risk.get("fixed_amount_usd").and_then(|v| v.as_float());
        let min_balance_usd = Decimal::try_from(
            risk.get("min_balance_usd")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
        )
        .unwrap_or(Decimal::ZERO);
        let mut series_ids: Vec<String> = markets
            .get("series_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if series_ids.is_empty() {
            series_ids = default_staggered_series_ids();
        } else {
            series_ids.sort();
            series_ids.dedup();
        }

        let mut adapter = Self::new(
            id,
            StaggeredArbLiveConfig {
                backtest_config: bc,
                fee_rate,
            },
            dry_run,
        );
        adapter.fixed_amount_usd = fixed_amount_usd;
        adapter.min_balance_usd = min_balance_usd.max(Decimal::ZERO);
        adapter.series_ids = series_ids;
        Ok(adapter)
    }

    // ─── Series ID mapping (same as MomentumStrategyAdapter) ──

    fn series_to_symbol(series_id: &str) -> Option<(&'static str, u64)> {
        match series_id {
            "10684" => Some(("BTCUSDT", 300)),
            "10683" => Some(("ETHUSDT", 300)),
            "10686" => Some(("SOLUSDT", 300)),
            "10685" => Some(("XRPUSDT", 300)),
            "10192" => Some(("BTCUSDT", 900)),
            "10191" => Some(("ETHUSDT", 900)),
            "10423" => Some(("SOLUSDT", 900)),
            "10422" => Some(("XRPUSDT", 900)),
            _ => None,
        }
    }

    fn estimated_live_locked_capital(&self) -> Decimal {
        let open_leg1: Decimal = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| p.leg1_price * Decimal::from(p.leg1_shares) + p.leg1_fee)
            .sum();

        let pending_orders: Decimal = self
            .live_orders
            .values()
            .map(|track| {
                let notional = track.price * Decimal::from(track.shares);
                let fee = notional * self.config.fee_rate;
                notional + fee
            })
            .sum();

        open_leg1 + pending_orders
    }

    fn available_balance_for_leg1(&self) -> Decimal {
        if self.dry_run {
            self.equity
        } else {
            (self.equity - self.estimated_live_locked_capital()).max(Decimal::ZERO)
        }
    }

    /// Count currently active cycles (open Leg1 positions + pending Leg1 orders).
    fn active_cycle_count(&self) -> usize {
        let open_positions = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();
        let pending_leg1 = self.pending_leg1_events.len();
        open_positions + pending_leg1
    }

    /// Check if a specific event already has an active cycle (open or pending).
    fn has_active_cycle_for_event(&self, event_id: &str) -> bool {
        self.positions
            .iter()
            .any(|p| p.event_id == event_id && p.state == PaperPositionState::Leg1Filled)
            || self.pending_leg1_events.contains(event_id)
    }

    // ─── Entry logic (ported from backtest engine) ──────────

    fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) -> Vec<StrategyAction> {
        let mut actions = Vec::new();
        let bc = &self.config.backtest_config;

        let windows: Vec<LiveWindow> = match self.active_windows.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => return actions,
        };

        let (st, vol_info) = match self.spot_prices.get(symbol) {
            Some(s) => {
                let vol = s.volatility(bc.vol_lookback_secs).and_then(|v| v.to_f64());
                let n_ticks = s.history_len().min(5000) as f64;
                (s.price, (vol, n_ticks))
            }
            None => return actions,
        };

        let (up_ask, down_ask) = match self.pm_asks.get(symbol) {
            Some(a) => *a,
            None => return actions,
        };

        for window in &windows {
            if let Some(action) =
                self.try_entry_for_window(symbol, ts, window, st, vol_info, up_ask, down_ask)
            {
                actions.push(action);
            }
        }
        actions
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &LiveWindow,
        st: Decimal,
        vol_info: (Option<f64>, f64),
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Option<StrategyAction> {
        let bc = self.config.backtest_config.clone();

        // 0. Balance pause — skip entries while waiting for claimer to free funds
        if !self.dry_run {
            if let Some(pause_until) = self.balance_pause_until {
                if ts < pause_until {
                    self.bump_entry_reject("balance_pause_active");
                    return None;
                }
                // Pause expired, reset
                self.balance_pause_until = None;
                self.consecutive_balance_failures = 0;
                info!("[STAG-ARB] Balance pause expired, resuming entries");
            }
        }

        // Per-event cycle lock: each event can only have one active cycle.
        if self.has_active_cycle_for_event(&window.event_id) {
            self.bump_entry_reject("event_cycle_active");
            return None;
        }

        // Global concurrency cap: max N cycles across all events.
        if self.active_cycle_count() >= bc.max_concurrent_positions {
            self.bump_entry_reject("max_concurrent_reached");
            return None;
        }

        // 1. Time remaining
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < bc.min_time_remaining_secs as f64 {
            self.bump_entry_reject("time_remaining_too_low");
            return None;
        }
        // Entry timing gate: prefer entering soon after event starts.
        let window_start = window.end_time - chrono::Duration::seconds(window.window_secs as i64);
        let elapsed_since_start = (ts - window_start).num_seconds();
        if elapsed_since_start < 0 {
            self.bump_entry_reject("before_event_start");
            return None;
        }
        if bc.entry_after_start_max_secs > 0
            && elapsed_since_start > bc.entry_after_start_max_secs as i64
        {
            self.bump_entry_reject("entry_window_expired");
            return None;
        }

        // 2. Need both asks
        let (ua, da) = match (up_ask, down_ask) {
            (Some(u), Some(d)) => (u, d),
            _ => {
                self.bump_entry_reject("missing_pm_quotes");
                return None;
            }
        };

        // 3. Min ask price filter
        if ua < bc.min_ask_price || da < bc.min_ask_price {
            self.bump_entry_reject("ask_below_min");
            return None;
        }

        // 4. Min entry sum filter
        let current_sum = ua + da;
        if current_sum < bc.min_entry_sum {
            self.bump_entry_reject("sum_below_min_entry_sum");
            return None;
        }

        // 5. Max entry sum filter (strict): require current_sum < max_initial_sum
        if current_sum >= bc.max_initial_sum {
            self.bump_entry_reject("sum_above_max_initial_sum");
            return None;
        }

        // 6. Compute volatility
        let sigma = {
            let floor = bc.vol_floor;
            match vol_info.0 {
                Some(tick_vol) if tick_vol > 0.0 => {
                    let n_ticks = vol_info.1;
                    (tick_vol * n_ticks.sqrt()).max(floor)
                }
                _ => floor,
            }
        };

        // 7. Estimate probability
        // Require a concrete event anchor (window open) to avoid false threshold events.
        let s0 = match window.open_price {
            Some(v) if v > Decimal::ZERO => v,
            _ => {
                self.bump_entry_reject("missing_window_open_anchor");
                return None;
            }
        };
        let p_hat = estimate_probability(s0, st, sigma, time_remaining, bc.mu);

        // 7b. Compute Greeks (optional)
        let greeks = if bc.use_greeks {
            super::gamma_scalping::greeks::binary_greeks(
                st.to_f64().unwrap_or(0.0),
                s0.to_f64().unwrap_or(0.0),
                sigma,
                time_remaining,
                window.window_secs as f64,
            )
        } else {
            None
        };

        // 7c. Greeks-based filters
        if let Some(ref g) = greeks {
            if bc.min_gamma > 0.0 && g.gamma.abs() < bc.min_gamma {
                self.bump_entry_reject("greeks_gamma_below_min");
                return None;
            }
            if bc.max_theta_cost > 0.0 && g.theta.abs() > bc.max_theta_cost {
                self.bump_entry_reject("greeks_theta_above_max");
                return None;
            }
        }

        // 8. Direction threshold
        if (p_hat - 0.5).abs() < bc.direction_threshold {
            self.bump_entry_reject("direction_strength_below_threshold");
            return None;
        }

        // 8b. Price displacement force from event open anchor.
        // Require meaningful move and direction agreement to avoid noisy fake thresholds.
        const MIN_PRICE_DISPLACEMENT: f64 = 0.0003; // 3 bps
        let displacement = ((st - s0) / s0).to_f64().unwrap_or(0.0);
        if displacement.abs() < MIN_PRICE_DISPLACEMENT {
            self.bump_entry_reject("price_displacement_too_small");
            return None;
        }

        // 9. Direction: p_hat > 0.5 → buy UP first
        let predicted_up = if bc.reverse_signal { p_hat < 0.5 } else { p_hat > 0.5 };

        if predicted_up && displacement <= 0.0 {
            self.bump_entry_reject("direction_displacement_mismatch");
            return None;
        }
        if !predicted_up && displacement >= 0.0 {
            self.bump_entry_reject("direction_displacement_mismatch");
            return None;
        }

        // 9a. Greeks directional confirmation.
        if let Some(ref g) = greeks {
            const MIN_DELTA_ABS: f64 = 0.02;
            const MIN_VEGA_ABS: f64 = 0.0001;
            const MIN_D2_STRENGTH: f64 = 0.05;
            if g.delta.abs() < MIN_DELTA_ABS || g.vega.abs() < MIN_VEGA_ABS {
                self.bump_entry_reject("greeks_strength_too_low");
                return None;
            }
            if predicted_up {
                if g.d2 < MIN_D2_STRENGTH || g.fair_value <= 0.5 {
                    self.bump_entry_reject("greeks_direction_mismatch");
                    return None;
                }
            } else if g.d2 > -MIN_D2_STRENGTH || g.fair_value >= 0.5 {
                self.bump_entry_reject("greeks_direction_mismatch");
                return None;
            }
        }

        // 9b. Binance L2 OI confirmation gate (feed -> market update -> entry filter).
        const OI_CONFIRM_THRESHOLD: f64 = 0.005;
        const OI_MAX_STALE_SECS: i64 = 60;
        let obi_ts = match self.binance_l2_obi_ts.get(symbol) {
            Some(v) => *v,
            None => {
                self.bump_entry_reject("obi_missing");
                return None;
            }
        };
        if (ts - obi_ts).num_seconds().abs() > OI_MAX_STALE_SECS {
            self.bump_entry_reject("obi_stale");
            return None;
        }
        let obi = match self.binance_l2_obi_5.get(symbol) {
            Some(v) => v.to_f64().unwrap_or(0.0),
            None => {
                self.bump_entry_reject("obi_missing");
                return None;
            }
        };
        if predicted_up && obi < OI_CONFIRM_THRESHOLD {
            self.bump_entry_reject("obi_not_confirmed");
            return None;
        }
        if !predicted_up && obi > -OI_CONFIRM_THRESHOLD {
            self.bump_entry_reject("obi_not_confirmed");
            return None;
        }

        let (leg1_dir, leg1_ask) = if predicted_up {
            (Direction::Up, ua)
        } else {
            (Direction::Down, da)
        };

        // 9b. Leg1 price cap
        if leg1_ask > bc.max_leg1_price {
            self.bump_entry_reject("leg1_price_above_cap");
            return None;
        }

        // 10. Target Leg2 feasibility: need leg1 + leg2 < merge_target_sum
        let target_leg2 = bc.merge_target_sum - leg1_ask;
        if target_leg2 <= Decimal::ZERO {
            self.bump_entry_reject("target_leg2_non_positive");
            return None;
        }

        // 11. Cooldown
        if let Some(last) = self.cooldowns.get(symbol) {
            if (ts - *last).num_seconds() < bc.cooldown_secs as i64 {
                self.bump_entry_reject("cooldown_active");
                return None;
            }
        }

        // 12–13. (Moved to top of function: per-event cycle lock + global concurrency cap)

        // 14. Max trades per event
        if bc.max_trades_per_event > 0 {
            let count = self
                .event_trade_counts
                .get(&window.event_id)
                .copied()
                .unwrap_or(0);
            if count >= bc.max_trades_per_event {
                self.bump_entry_reject("max_trades_per_event_reached");
                return None;
            }
        }

        // 15. Calculate shares with venue minimums:
        // - At least 5 shares
        // - At least $1 notional for marketable BUY orders
        let mut fixed_amount_target: Option<Decimal> = None;
        let mut min_share_bump = false;
        let shares = if let Some(amount_usd) = self.fixed_amount_usd {
            fixed_amount_target = Decimal::try_from(amount_usd)
                .ok()
                .map(|d| d.max(Decimal::ZERO));
            let price_f64 = leg1_ask.to_f64().unwrap_or(0.5);
            if price_f64 > 0.0 {
                let calc_from_target = (amount_usd / price_f64).ceil() as u64;
                let min_shares_for_notional = (1.0_f64 / price_f64).ceil() as u64;
                let adjusted = calc_from_target.max(min_shares_for_notional).max(5);
                min_share_bump = adjusted > calc_from_target;
                adjusted
            } else {
                bc.shares_per_trade.max(5)
            }
        } else {
            let base_shares = bc.shares_per_trade.max(5);
            if bc.delta_weighted_sizing {
                if let Some(ref g) = greeks {
                    let scale = (g.delta.abs() * 2.0).clamp(0.5, 2.0);
                    ((base_shares as f64 * scale).round() as u64).max(5)
                } else {
                    base_shares
                }
            } else {
                base_shares
            }
        };
        if shares == 0 {
            self.bump_entry_reject("zero_share_sizing");
            return None;
        }

        let leg1_notional = leg1_ask * Decimal::from(shares);
        if let Some(target) = fixed_amount_target.filter(|t| *t > Decimal::ZERO) {
            if min_share_bump {
                info!(
                    "[STAG-ARB] FIXED AMOUNT ADJUST {} target=${:.4} actual_leg_notional=${:.4} shares={} price={:.4}",
                    symbol, target, leg1_notional, shares, leg1_ask
                );
                if leg1_notional > target * dec!(1.20) && !self.fixed_amount_overage_warned {
                    let over_pct = ((leg1_notional - target) / target) * dec!(100);
                    warn!(
                        "[STAG-ARB] fixed_amount_usd=${:.4} inflated to actual_leg_notional=${:.4} (+{:.1}%) because venue minimums apply ($1 notional / 5 shares)",
                        target, leg1_notional, over_pct
                    );
                    self.fixed_amount_overage_warned = true;
                }
            }
        }

        let leg1_fee = leg1_notional * self.config.fee_rate;
        let total_cost = leg1_notional + leg1_fee;
        let available_before = self.available_balance_for_leg1();
        let remaining_after = available_before - total_cost;
        if total_cost > available_before || remaining_after < self.min_balance_usd {
            self.bump_entry_reject("reserve_guard");
            info!(
                "[STAG-ARB] SKIP ENTRY {} reserve_guard available=${:.4} cost=${:.4} min_balance=${:.4}",
                symbol, available_before, total_cost, self.min_balance_usd
            );
            return None;
        }

        // 16. Determine token_id for the leg1 side
        let token_id = match leg1_dir {
            Direction::Up => window.up_token.clone(),
            Direction::Down => window.down_token.clone(),
        };
        let side = match leg1_dir {
            Direction::Up => Side::Up,
            Direction::Down => Side::Down,
        };

        if self.dry_run {
            // ── Paper fill path (original behavior) ──
            self.equity -= total_cost;

            let window_duration = (window.end_time - ts).num_seconds() as f64;
            let max_wait_by_pct = (window_duration * bc.max_wait_pct) as i64;
            let max_wait = (bc.max_wait_secs as i64).min(max_wait_by_pct);
            let wait_deadline = ts + chrono::Duration::seconds(max_wait);

            self.positions.push(PaperPosition {
                symbol: symbol.to_string(),
                event_id: window.event_id.clone(),
                condition_id: window.condition_id.clone(),
                up_token: window.up_token.clone(),
                down_token: window.down_token.clone(),
                leg1_direction: leg1_dir.clone(),
                leg1_price: leg1_ask,
                leg1_shares: shares,
                leg1_fee,
                leg1_time: ts,
                wait_deadline,
                leg2_price: None,
                leg2_shares: None,
                leg2_fee: None,
                leg2_time: None,
                state: PaperPositionState::Leg1Filled,
            });

            self.cooldowns.insert(symbol.to_string(), ts);
            *self
                .event_trade_counts
                .entry(window.event_id.clone())
                .or_default() += 1;

            let msg = format!(
                "[STAG-ARB] ENTRY {} {} leg1=${:.4} sum=${:.4} p_hat={:.3} σ={:.5} (paper)",
                symbol, leg1_dir, leg1_ask, current_sum, p_hat, sigma,
            );
            info!("{}", msg);
            self.bump_entry_reject("entry_accepted");

            Some(StrategyAction::LogEvent {
                event: StrategyEvent::new(StrategyEventType::EntryTriggered, msg),
            })
        } else {
            // ── Live order path ──
            let client_order_id = format!(
                "stag_leg1_{}_{}",
                window.event_id,
                Utc::now().timestamp_millis()
            );

            let order = OrderRequest::buy_limit(token_id.clone(), side, shares, leg1_ask);

            // Track pending order
            self.live_orders.insert(
                client_order_id.clone(),
                LiveOrderTrack {
                    event_id: window.event_id.clone(),
                    condition_id: window.condition_id.clone(),
                    symbol: symbol.to_string(),
                    up_token: window.up_token.clone(),
                    down_token: window.down_token.clone(),
                    direction: leg1_dir.clone(),
                    token_id,
                    leg: 1,
                    price: leg1_ask,
                    shares,
                    position_idx: None,
                    close_reason: None,
                    submitted_at: ts,
                    cancel_requested_at: None,
                },
            );
            self.pending_leg1_events.insert(window.event_id.clone());
            self.cooldowns.insert(symbol.to_string(), ts);

            let msg = format!(
                "[STAG-ARB] LEG1 SUBMIT {} {} @ {:.2}¢ ({} shares, ${:.2}) p_hat={:.3} σ={:.5}",
                symbol,
                leg1_dir,
                leg1_ask * dec!(100),
                shares,
                leg1_ask.to_f64().unwrap_or(0.0) * shares as f64,
                p_hat,
                sigma,
            );
            info!("{}", msg);
            self.bump_entry_reject("entry_accepted");

            Some(StrategyAction::SubmitOrder {
                client_order_id,
                order,
                priority: 10,
            })
        }
    }

    // ─── Leg2 monitoring ──────────────────────────────────────

    fn check_leg2_opportunities(&mut self, symbol: &str, ts: DateTime<Utc>) -> Vec<StrategyAction> {
        let mut actions = Vec::new();
        let bc = self.config.backtest_config.clone();
        let mut leg2_skip_batch: HashMap<&'static str, u64> = HashMap::new();

        let pm_asks = match self.pm_asks.get(symbol) {
            Some(a) => *a,
            None => {
                self.bump_leg2_skip("missing_pm_quotes");
                return actions;
            }
        };

        // Collect indices + actions (can't mutate while iterating)
        let mut leg2_fills: Vec<(usize, Decimal, String)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.state != PaperPositionState::Leg1Filled {
                continue;
            }

            // Skip positions with in-flight Leg2 orders
            if self.pending_leg2_positions.contains(&i) {
                *leg2_skip_batch.entry("leg2_order_pending").or_default() += 1;
                continue;
            }

            let time_remaining = match self.active_windows.get(symbol) {
                Some(windows) => windows
                    .iter()
                    .find(|w| w.event_id == pos.event_id)
                    .map(|w| (w.end_time - ts).num_seconds() as f64)
                    .unwrap_or(f64::MAX),
                None => f64::MAX,
            };
            let in_final_window = bc.no_trade_last_secs > 0
                && time_remaining <= bc.no_trade_last_secs as f64
                && time_remaining > 0.0;

            let other_ask = match pos.leg1_direction {
                Direction::Up => pm_asks.1,
                Direction::Down => pm_asks.0,
            };
            let other_ask = match other_ask {
                Some(a) if a >= bc.min_ask_price => a,
                Some(_) => {
                    *leg2_skip_batch.entry("other_ask_below_min").or_default() += 1;
                    continue;
                }
                None => {
                    *leg2_skip_batch.entry("missing_other_ask").or_default() += 1;
                    continue;
                }
            };

            let current_sum = pos.leg1_price + other_ask;
            let all_in_sum = (pos.leg1_price + other_ask) * (Decimal::ONE + self.config.fee_rate);
            let net_profit_per_share = Decimal::ONE - all_in_sum;
            let secs_since_leg1 = (ts - pos.leg1_time).num_seconds();
            let leg2_ready = secs_since_leg1 >= bc.min_leg2_delay_secs as i64;
            if !leg2_ready {
                *leg2_skip_batch.entry("min_leg2_delay").or_default() += 1;
                continue;
            }
            // "Forced" paths (timeout / time-safety / stop-loss / final-window) should not be
            // blocked by the final_minute_block, otherwise positions can
            // remain open until settlement in low-liquidity windows.

            // A. Merge target reached (primary close condition) — blocked in final window
            if !in_final_window && current_sum <= bc.merge_target_sum && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            // B. Profitable merge after fees — blocked in final window
            if !in_final_window && net_profit_per_share >= bc.min_profit_target && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            // C. Lock any net profit after fees — blocked in final window
            if !in_final_window && net_profit_per_share > Decimal::ZERO && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            // D. Leg1 loss guard (if configured) — always allowed
            if bc.max_leg1_loss > Decimal::ZERO && leg2_ready {
                let leg1_mark = match pos.leg1_direction {
                    Direction::Up => pm_asks.0,
                    Direction::Down => pm_asks.1,
                };
                if let Some(mark) = leg1_mark {
                    let leg1_loss = (pos.leg1_price - mark).max(Decimal::ZERO);
                    if leg1_loss >= bc.max_leg1_loss {
                        leg2_fills.push((i, other_ask, "forced_stop_loss".to_string()));
                        continue;
                    }
                }
            }

            // E. Timeout — force-complete — always allowed
            if ts >= pos.wait_deadline && leg2_ready {
                leg2_fills.push((i, other_ask, "forced_timeout".to_string()));
                continue;
            }

            // F. Time safety — not enough time left — always allowed
            if time_remaining < bc.min_time_remaining_secs as f64 && leg2_ready {
                leg2_fills.push((i, other_ask, "forced_time_safety".to_string()));
                continue;
            }

            // G. Final window smart close — use probability to decide.
            //
            // In the last no_trade_last_secs (30s), compute p_hat to assess
            // whether our Leg1 direction is likely to win at settlement:
            //
            //   p_win HIGH (>0.80): our side is strongly favored → let it settle
            //     single-leg. EV of settlement > cost of buying expensive Leg2.
            //   p_win LOW (<0.80) or uncertain: directional risk too high →
            //     force buy Leg2 to lock in a known (possibly negative) outcome.
            //
            // This avoids the worst case: holding a losing single-leg to $0.
            if in_final_window && leg2_ready {
                // Compute p_hat for this position's window
                let window_info = self.active_windows.get(symbol).and_then(|ws| {
                    ws.iter().find(|w| w.event_id == pos.event_id)
                });
                let s0 = window_info.and_then(|w| w.open_price);
                let window_secs = window_info.map(|w| w.window_secs).unwrap_or(300);
                let st = self.spot_prices.get(symbol).map(|s| s.price);
                let sigma = self.spot_prices.get(symbol)
                    .and_then(|s| s.volatility(bc.vol_lookback_secs))
                    .and_then(|v| v.to_f64())
                    .map(|tick_vol| {
                        let n = self.spot_prices.get(symbol)
                            .map(|s| s.history_len().min(5000) as f64)
                            .unwrap_or(100.0);
                        (tick_vol * n.sqrt()).max(bc.vol_floor)
                    })
                    .unwrap_or(bc.vol_floor);

                let should_force_close = match (s0, st) {
                    (Some(s0_val), Some(st_val)) if s0_val > Decimal::ZERO => {
                        let p_hat = estimate_probability(
                            s0_val, st_val, sigma, time_remaining, bc.mu,
                        );
                        // p_win = probability that OUR Leg1 direction wins
                        let p_win = match pos.leg1_direction {
                            Direction::Up => p_hat,
                            Direction::Down => 1.0 - p_hat,
                        };

                        // Also check: is price near the strike? (high uncertainty zone)
                        let displacement = ((st_val - s0_val) / s0_val)
                            .to_f64().unwrap_or(0.0).abs();
                        let near_strike = displacement < 0.001; // within 10 bps

                        // Also check: is vol high relative to time left?
                        // High vol + little time = anything can happen
                        let vol_time_ratio = sigma / (time_remaining / window_secs as f64).max(0.01);
                        let high_vol_regime = vol_time_ratio > 0.05;

                        if p_win >= 0.80 && !near_strike {
                            // Strongly in our favor AND price has moved away from strike
                            // → let it settle single-leg for higher EV
                            info!(
                                "[STAG-ARB] FINAL WINDOW HOLD {} {} p_win={:.3} disp={:.4} vol_ratio={:.4} — letting settle",
                                symbol, pos.leg1_direction, p_win, displacement, vol_time_ratio,
                            );
                            false
                        } else {
                            // Uncertain or against us → force close
                            info!(
                                "[STAG-ARB] FINAL WINDOW CLOSE {} {} p_win={:.3} disp={:.4} near_strike={} high_vol={} — buying Leg2",
                                symbol, pos.leg1_direction, p_win, displacement, near_strike, high_vol_regime,
                            );
                            true
                        }
                    }
                    _ => {
                        // No price data → can't assess risk → force close to be safe
                        true
                    }
                };

                if should_force_close {
                    leg2_fills.push((i, other_ask, "forced_final_window".to_string()));
                    continue;
                }
            }
        }

        // Execute in reverse order to preserve indices
        leg2_fills.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, other_ask, reason) in leg2_fills {
            if let Some(action) = self.fill_leg2(idx, other_ask, &reason, ts) {
                actions.push(action);
            }
        }
        for (reason, count) in leg2_skip_batch {
            *self.leg2_skip_counts.entry(reason.to_string()).or_default() += count;
        }
        actions
    }

    fn fill_leg2(
        &mut self,
        idx: usize,
        other_ask: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
    ) -> Option<StrategyAction> {
        let pos = &self.positions[idx];
        let shares = pos.leg1_shares;

        if self.dry_run {
            // ── Paper fill path ──
            let leg2_fee = other_ask * Decimal::from(shares) * self.config.fee_rate;
            let leg2_cost = other_ask * Decimal::from(shares) + leg2_fee;

            if leg2_cost > self.equity {
                return None;
            }
            self.equity -= leg2_cost;

            let payout = Decimal::from(shares) * Decimal::ONE;
            let total_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price
                + pos.leg1_fee
                + other_ask * Decimal::from(shares)
                + leg2_fee;
            let pnl = payout - total_cost;
            self.equity += payout;

            let duration_secs = (ts - pos.leg1_time).num_seconds();
            let symbol = pos.symbol.clone();
            let event_id = pos.event_id.clone();
            let direction = pos.leg1_direction.clone();
            let leg1_price = pos.leg1_price;
            let opened_at = pos.leg1_time;

            let pos = &mut self.positions[idx];
            pos.leg2_price = Some(other_ask);
            pos.leg2_shares = Some(shares);
            pos.leg2_fee = Some(leg2_fee);
            pos.leg2_time = Some(ts);
            pos.state = if reason == "merge" {
                PaperPositionState::Merged
            } else {
                PaperPositionState::ForcedComplete
            };

            self.closed_trades.push(PaperTrade {
                symbol: symbol.clone(),
                event_id,
                direction: direction.clone(),
                leg1_price,
                leg2_price: other_ask,
                total_cost,
                payout,
                pnl,
                exit_reason: reason.to_string(),
                duration_secs,
                opened_at,
                closed_at: ts,
            });

            let tag = if reason == "merge" { "COMPLETE" } else { "FORCED" };
            let msg =
                format!(
                "[STAG-ARB] {} {} cost=${:.4} payout=${:.4} pnl={}{:.4} wait={}s reason={} (paper)",
                tag, symbol, total_cost, payout,
                if pnl >= Decimal::ZERO { "+" } else { "" },
                pnl, duration_secs, reason,
            );
            info!("{}", msg);

            Some(StrategyAction::LogEvent {
                event: StrategyEvent::new(StrategyEventType::CycleCompleted, msg),
            })
        } else {
            // ── Live order path ──
            let symbol = pos.symbol.clone();
            let event_id = pos.event_id.clone();
            let up_token = pos.up_token.clone();
            let down_token = pos.down_token.clone();
            let leg2_direction = match pos.leg1_direction {
                Direction::Up => Direction::Down,
                Direction::Down => Direction::Up,
            };

            let token_id = match leg2_direction {
                Direction::Up => up_token.clone(),
                Direction::Down => down_token.clone(),
            };

            let side = match leg2_direction {
                Direction::Up => Side::Up,
                Direction::Down => Side::Down,
            };

            let close_mode = if reason == "merge" { "merge" } else { "forced" };
            let client_order_id = format!(
                "stag_leg2_{}_{}_{}",
                close_mode,
                event_id,
                Utc::now().timestamp_millis()
            );

            let order = OrderRequest::buy_limit(token_id.clone(), side, shares, other_ask);

            // Track pending Leg2 order
            self.live_orders.insert(
                client_order_id.clone(),
                LiveOrderTrack {
                    event_id: event_id.clone(),
                    condition_id: pos.condition_id.clone(),
                    symbol: symbol.clone(),
                    up_token,
                    down_token,
                    direction: leg2_direction,
                    token_id,
                    leg: 2,
                    price: other_ask,
                    shares,
                    position_idx: Some(idx),
                    close_reason: Some(reason.to_string()),
                    submitted_at: ts,
                    cancel_requested_at: None,
                },
            );
            self.pending_leg2_positions.insert(idx);

            let tag = if reason == "merge" { "COMPLETE" } else { "FORCED" };
            let msg = format!(
                "[STAG-ARB] LEG2 {} SUBMIT {} @ {:.2}¢ ({} shares, ${:.2}) reason={}",
                tag,
                symbol,
                other_ask * dec!(100),
                shares,
                other_ask.to_f64().unwrap_or(0.0) * shares as f64,
                reason,
            );
            info!("{}", msg);

            Some(StrategyAction::SubmitOrder {
                client_order_id,
                order,
                priority: 10,
            })
        }
    }

    // ─── Periodic summary ────────────────────────────────────

    fn build_summary(&self) -> String {
        let total = self.closed_trades.len();
        let wins = self
            .closed_trades
            .iter()
            .filter(|t| t.pnl > Decimal::ZERO)
            .count();
        let win_rate = if total > 0 {
            wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let avg_pnl = if total > 0 {
            self.closed_trades.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(total as u64)
        } else {
            Decimal::ZERO
        };
        let open = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();

        format!(
            "[STAG-ARB] equity=${:.2} trades={} win_rate={:.0}% avg_pnl=${:.4} open={}",
            self.equity, total, win_rate, avg_pnl, open,
        )
    }
}

#[async_trait]
impl Strategy for StaggeredArbAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Staggered Arbitrage"
    }

    fn description(&self) -> &str {
        "Time-staggered two-leg arb on crypto UP/DOWN binary options"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::BinanceSpot {
                symbols: self.config.backtest_config.symbols.clone(),
            },
            DataFeed::PolymarketEvents {
                series_ids: self.series_ids.clone(),
            },
            DataFeed::Tick { interval_ms: 1000 },
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
                self.spot_prices
                    .entry(symbol.clone())
                    .and_modify(|sp| sp.update(*price, None, *timestamp))
                    .or_insert_with(|| SpotPrice::new(*price, None, *timestamp));

                // Set open_price on windows that don't have one yet
                if let Some(windows) = self.active_windows.get_mut(symbol) {
                    for w in windows.iter_mut() {
                        if w.open_price.is_none() {
                            w.open_price = Some(*price);
                        }
                    }
                }
            }

            MarketUpdate::BinanceL2 {
                symbol,
                obi_5,
                timestamp,
                ..
            } => {
                self.binance_l2_obi_5.insert(symbol.clone(), *obi_5);
                self.binance_l2_obi_ts.insert(symbol.clone(), *timestamp);
            }

            MarketUpdate::PolymarketQuote {
                token_id, quote, ..
            } => {
                // Map token → (symbol, direction)
                if let Some((symbol, direction)) = self.token_to_symbol.get(token_id) {
                    let symbol = symbol.clone();
                    let direction = direction.clone();
                    let ask = quote.best_ask;

                    let entry = self.pm_asks.entry(symbol.clone()).or_insert((None, None));
                    match direction {
                        Direction::Up => entry.0 = ask,
                        Direction::Down => entry.1 = ask,
                    }

                    // Check Leg2 opportunities first (existing positions)
                    let leg2_actions = self.check_leg2_opportunities(&symbol, Utc::now());
                    actions.extend(leg2_actions);

                    // Then try new entries
                    let entry_actions = self.try_entry(&symbol, Utc::now());
                    actions.extend(entry_actions);
                }
            }
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                condition_id,
                ..
            } => {
                let Some((symbol, window_secs)) = Self::series_to_symbol(series_id) else {
                    return Ok(actions);
                };

                // Only track symbols we're configured for
                if !self
                    .config
                    .backtest_config
                    .symbols
                    .iter()
                    .any(|s| s == symbol)
                {
                    return Ok(actions);
                }

                // Window duration filter
                let bc = &self.config.backtest_config;
                if !bc.allowed_window_durations.is_empty() {
                    let tol = bc.window_duration_tolerance as i64;
                    let matches = bc
                        .allowed_window_durations
                        .iter()
                        .any(|&d| (window_secs as i64 - d as i64).abs() <= tol);
                    if !matches {
                        return Ok(actions);
                    }
                }

                // Register token → symbol mapping
                self.token_to_symbol
                    .insert(up_token.clone(), (symbol.to_string(), Direction::Up));
                self.token_to_symbol
                    .insert(down_token.clone(), (symbol.to_string(), Direction::Down));

                // Add window
                let windows = self.active_windows.entry(symbol.to_string()).or_default();
                if !windows.iter().any(|w| w.event_id == *event_id) {
                    let open_price = self.spot_prices.get(symbol).map(|s| s.price);
                    windows.push(LiveWindow {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                        condition_id: condition_id.clone(),
                        end_time: *end_time,
                        open_price,
                        window_secs,
                    });
                    debug!(
                        "[STAG-ARB] Window added: {} {} {}s end={}",
                        symbol,
                        event_id,
                        window_secs,
                        end_time.format("%H:%M:%S"),
                    );
                }
            }

            MarketUpdate::EventExpired { event_id } => {
                for windows in self.active_windows.values_mut() {
                    windows.retain(|w| w.event_id != *event_id);
                }
            }
            _ => {}
        }

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        if self.dry_run {
            return Ok(Vec::new());
        }
        let mut actions = Vec::new();

        let client_id = match &update.client_order_id {
            Some(id) => id.clone(),
            None => return Ok(Vec::new()),
        };

        let track = match self.live_orders.get(&client_id) {
            Some(t) => t.clone(),
            None => return Ok(Vec::new()),
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(track.price);
                let ts = update.timestamp;

                if track.leg == 1 {
                    // ── Leg1 filled → create position ──
                    self.pending_leg1_events.remove(&track.event_id);
                    *self
                        .event_trade_counts
                        .entry(track.event_id.clone())
                        .or_default() += 1;

                    let bc = &self.config.backtest_config;
                    let window_end = self
                        .active_windows
                        .get(&track.symbol)
                        .and_then(|ws| ws.iter().find(|w| w.event_id == track.event_id))
                        .map(|w| w.end_time)
                        .unwrap_or(ts + chrono::Duration::seconds(300));

                    let window_duration = (window_end - ts).num_seconds() as f64;
                    let max_wait_by_pct = (window_duration * bc.max_wait_pct) as i64;
                    let max_wait = (bc.max_wait_secs as i64).min(max_wait_by_pct);
                    let wait_deadline = ts + chrono::Duration::seconds(max_wait);

                    let leg1_fee = fill_price * Decimal::from(track.shares) * self.config.fee_rate;

                    self.positions.push(PaperPosition {
                        symbol: track.symbol.clone(),
                        event_id: track.event_id.clone(),
                        condition_id: track.condition_id.clone(),
                        up_token: track.up_token.clone(),
                        down_token: track.down_token.clone(),
                        leg1_direction: track.direction.clone(),
                        leg1_price: fill_price,
                        leg1_shares: track.shares,
                        leg1_fee,
                        leg1_time: ts,
                        wait_deadline,
                        leg2_price: None,
                        leg2_shares: None,
                        leg2_fee: None,
                        leg2_time: None,
                        state: PaperPositionState::Leg1Filled,
                    });

                    info!(
                        "[STAG-ARB] LEG1 FILLED {} {} @ {:.2}¢ ({} shares)",
                        track.symbol,
                        track.direction,
                        fill_price * dec!(100),
                        track.shares,
                    );
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::OrderFilled,
                            format!(
                                "[STAG-ARB] LEG1 FILLED {} {} @ {:.2}¢ shares={}",
                                track.symbol,
                                track.direction,
                                fill_price * dec!(100),
                                track.shares
                            ),
                        ),
                    });
                } else {
                    // ── Leg2 filled → close position ──
                    if let Some(idx) = track.position_idx {
                        self.pending_leg2_positions.remove(&idx);

                        if idx < self.positions.len() {
                            let pos = &self.positions[idx];
                            let total_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price
                                + pos.leg1_fee
                                + fill_price * Decimal::from(track.shares)
                                + fill_price * Decimal::from(track.shares) * self.config.fee_rate;
                            let payout = Decimal::from(track.shares);
                            let pnl = payout - total_cost;
                            let duration_secs = (ts - pos.leg1_time).num_seconds();

                            let symbol = pos.symbol.clone();
                            let event_id = pos.event_id.clone();
                            let direction = pos.leg1_direction.clone();
                            let leg1_price = pos.leg1_price;
                            let opened_at = pos.leg1_time;
                            let close_reason =
                                track.close_reason.as_deref().unwrap_or("merge").to_string();
                            let exit_reason = if close_reason == "merge" {
                                "live_leg2_complete".to_string()
                            } else {
                                "live_forced".to_string()
                            };

                            let pos = &mut self.positions[idx];
                            pos.leg2_price = Some(fill_price);
                            pos.leg2_shares = Some(track.shares);
                            pos.leg2_fee = Some(
                                fill_price * Decimal::from(track.shares) * self.config.fee_rate,
                            );
                            pos.leg2_time = Some(ts);
                            pos.state = if close_reason == "merge" {
                                PaperPositionState::Merged
                            } else {
                                PaperPositionState::ForcedComplete
                            };

                            self.closed_trades.push(PaperTrade {
                                symbol: symbol.clone(),
                                event_id,
                                direction,
                                leg1_price,
                                leg2_price: fill_price,
                                total_cost,
                                payout,
                                pnl,
                                exit_reason,
                                duration_secs,
                                opened_at,
                                closed_at: ts,
                            });

                            let tag = if close_reason == "merge" {
                                "COMPLETE"
                            } else {
                                "FORCED"
                            };
                            info!(
                                "[STAG-ARB] LEG2 {} FILLED {} cost=${:.4} pnl={}{:.4} wait={}s",
                                tag,
                                symbol,
                                total_cost,
                                if pnl >= Decimal::ZERO { "+" } else { "" },
                                pnl,
                                duration_secs,
                            );
                            actions.push(StrategyAction::LogEvent {
                                event: StrategyEvent::new(
                                    StrategyEventType::CycleCompleted,
                                    format!(
                                        "[STAG-ARB] LEG2 {} FILLED {} pnl={}{:.4} wait={}s",
                                        tag,
                                        symbol,
                                        if pnl >= Decimal::ZERO { "+" } else { "" },
                                        pnl,
                                        duration_secs
                                    ),
                                ),
                            });

                        }
                    }
                }

                self.live_orders.remove(&client_id);
            }

            OrderStatus::Cancelled | OrderStatus::Failed => {
                // Detect balance failures from error message
                if !self.dry_run {
                    let is_balance_error = update
                        .error
                        .as_ref()
                        .map(|e| e.contains("not enough balance") || e.contains("allowance"))
                        .unwrap_or(false);
                    if is_balance_error {
                        self.consecutive_balance_failures += 1;
                        if self.consecutive_balance_failures >= 3
                            && self.balance_pause_until.is_none()
                        {
                            // Pause entries for 90s — let claimer free up funds
                            let pause_secs = 90;
                            self.balance_pause_until =
                                Some(update.timestamp + chrono::Duration::seconds(pause_secs));
                            info!(
                                "[STAG-ARB] Balance insufficient ({} consecutive failures), pausing entries for {}s to let claimer recycle funds",
                                self.consecutive_balance_failures, pause_secs
                            );
                        }
                    } else {
                        // Non-balance failure, reset counter
                        self.consecutive_balance_failures = 0;
                    }
                }

                if track.leg == 1 {
                    self.pending_leg1_events.remove(&track.event_id);
                    info!(
                        "[STAG-ARB] LEG1 {:?} {} {} — cleared for re-entry",
                        update.status, track.symbol, track.event_id,
                    );
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::Error,
                            format!(
                                "[STAG-ARB] LEG1 {:?} {} {}",
                                update.status, track.symbol, track.event_id
                            ),
                        ),
                    });
                } else if let Some(idx) = track.position_idx {
                    self.pending_leg2_positions.remove(&idx);
                    info!(
                        "[STAG-ARB] LEG2 {:?} {} — will retry on next tick",
                        update.status, track.symbol,
                    );
                    actions.push(StrategyAction::LogEvent {
                        event: StrategyEvent::new(
                            StrategyEventType::Error,
                            format!("[STAG-ARB] LEG2 {:?} {}", update.status, track.symbol),
                        ),
                    });
                }
                self.live_orders.remove(&client_id);
            }

            OrderStatus::Submitted | OrderStatus::PartiallyFilled => {
                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::StateChanged,
                        format!(
                            "[STAG-ARB] ORDER {:?} leg={} event={} symbol={} filled={} avg={:?}",
                            update.status,
                            track.leg,
                            track.event_id,
                            track.symbol,
                            update.filled_qty,
                            update.avg_fill_price
                        ),
                    ),
                });
            }

            _ => {}
        }

        Ok(actions)
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // 0. Cancel stale orders — two-phase approach to avoid race conditions.
        //
        // Phase 1: Order unfilled for >30s → send CancelOrder, mark cancel_requested_at.
        //          Do NOT remove from live_orders yet — let on_order_update handle cleanup
        //          when the exchange confirms Cancelled or Filled.
        //
        // Phase 2: Cancel was requested >60s ago but no callback arrived (lost message) →
        //          hard cleanup as last resort.
        const STALE_ORDER_SECS: i64 = 30;
        const HARD_CLEANUP_SECS: i64 = 90;

        // Phase 1: request cancel for stale orders
        let cancel_ids: Vec<String> = self
            .live_orders
            .iter()
            .filter(|(_, track)| {
                track.cancel_requested_at.is_none()
                    && (now - track.submitted_at).num_seconds() > STALE_ORDER_SECS
            })
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in &cancel_ids {
            if let Some(track) = self.live_orders.get_mut(client_id) {
                info!(
                    "[STAG-ARB] STALE ORDER CANCEL leg={} {} {} age={}s price={:.2}¢",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                    track.price * dec!(100),
                );
                track.cancel_requested_at = Some(now);
                actions.push(StrategyAction::CancelOrder {
                    order_id: client_id.clone(),
                });
            }
        }

        // Phase 2: hard cleanup — cancel was sent but no callback after 90s total
        let orphan_ids: Vec<String> = self
            .live_orders
            .iter()
            .filter(|(_, track)| {
                track.cancel_requested_at.is_some()
                    && (now - track.submitted_at).num_seconds() > HARD_CLEANUP_SECS
            })
            .map(|(id, _)| id.clone())
            .collect();
        for client_id in orphan_ids {
            if let Some(track) = self.live_orders.remove(&client_id) {
                warn!(
                    "[STAG-ARB] ORPHAN ORDER HARD CLEANUP leg={} {} {} age={}s — no callback received",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                );
                if track.leg == 1 {
                    self.pending_leg1_events.remove(&track.event_id);
                } else if let Some(idx) = track.position_idx {
                    self.pending_leg2_positions.remove(&idx);
                }
            }
        }

        // 1. Clean expired windows
        for windows in self.active_windows.values_mut() {
            windows.retain(|w| w.end_time > now);
        }

        // 2. Force-complete positions on expired windows
        let symbols: Vec<String> = self.pm_asks.keys().cloned().collect();
        for symbol in &symbols {
            let leg2_actions = self.check_leg2_opportunities(symbol, now);
            actions.extend(leg2_actions);
        }

        // 3. Periodic summary (every 60s)
        let should_print = self
            .last_summary
            .map(|t| (now - t).num_seconds() >= 60)
            .unwrap_or(true);
        if should_print && !self.closed_trades.is_empty() {
            let summary = self.build_summary();
            info!("{}", summary);
            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("summary".to_string()),
                    summary,
                ),
            });
            self.last_summary = Some(now);
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let open_count = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();
        let realized_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();
        let total_exposure: Decimal = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| p.leg1_price * Decimal::from(p.leg1_shares))
            .sum();

        let mut metrics = HashMap::new();
        metrics.insert("equity".to_string(), format!("{:.2}", self.equity));
        metrics.insert(
            "total_trades".to_string(),
            self.closed_trades.len().to_string(),
        );
        let merges = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason.contains("merge"))
            .count();
        let forced = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason.contains("forced"))
            .count();
        metrics.insert("merge_count".to_string(), merges.to_string());
        metrics.insert("forced_count".to_string(), forced.to_string());
        metrics.insert("dry_run".to_string(), self.dry_run.to_string());
        for (k, v) in self.entry_reject_counts.iter() {
            metrics.insert(format!("entry_gate_{}", k), v.to_string());
        }
        for (k, v) in self.leg2_skip_counts.iter() {
            metrics.insert(format!("leg2_gate_{}", k), v.to_string());
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if open_count > 0 {
                "trading".to_string()
            } else {
                "monitoring".to_string()
            },
            enabled: true,
            active: open_count > 0,
            position_count: open_count,
            pending_order_count: self.live_orders.len(),
            total_exposure,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| {
                let token_id = match p.leg1_direction {
                    Direction::Up => format!("{}_up", p.symbol),
                    Direction::Down => format!("{}_down", p.symbol),
                };
                let side = match p.leg1_direction {
                    Direction::Up => Side::Up,
                    Direction::Down => Side::Down,
                };
                PositionInfo::new(token_id, side, p.leg1_shares, p.leg1_price, self.id.clone())
            })
            .collect()
    }

    fn is_active(&self) -> bool {
        self.positions
            .iter()
            .any(|p| p.state == PaperPositionState::Leg1Filled)
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        let summary = self.build_summary();
        info!("[STAG-ARB] Shutdown: {}", summary);
        Ok(vec![StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                format!("Shutdown: {}", summary),
            ),
        }])
    }

    fn reset(&mut self) {
        self.positions.clear();
        self.closed_trades.clear();
        self.equity = self.initial_capital;
        self.cooldowns.clear();
        self.event_trade_counts.clear();
        self.active_windows.clear();
        self.spot_prices.clear();
        self.pm_asks.clear();
        self.token_to_symbol.clear();
        self.last_summary = None;
        self.fixed_amount_overage_warned = false;
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> StaggeredArbLiveConfig {
        StaggeredArbLiveConfig {
            backtest_config: StaggeredArbBacktestConfig::default(),
            fee_rate: dec!(0.015),
        }
    }

    #[test]
    fn test_adapter_creation() {
        let adapter = StaggeredArbAdapter::new("test_stag".to_string(), default_config(), true);
        assert_eq!(adapter.id(), "test_stag");
        assert_eq!(adapter.name(), "Staggered Arbitrage");
        assert!(!adapter.is_active());
        assert_eq!(adapter.equity, dec!(10000));
    }

    #[test]
    fn test_series_mapping() {
        assert_eq!(
            StaggeredArbAdapter::series_to_symbol("10684"),
            Some(("BTCUSDT", 300)),
        );
        assert_eq!(
            StaggeredArbAdapter::series_to_symbol("10192"),
            Some(("BTCUSDT", 900)),
        );
        assert_eq!(StaggeredArbAdapter::series_to_symbol("99999"), None);
    }

    #[test]
    fn test_from_toml() {
        let toml = r#"
[strategy]
name = "staggered_arb"

[entry]
symbols = ["BTCUSDT"]
shares_per_trade = 20
max_concurrent = 3
direction_threshold = 0.03
max_initial_sum = 1.20
max_leg1_price = 0.80
merge_target_sum = 0.95
min_profit_target = 0.02
min_ask_price = 0.05
min_entry_sum = 0.70

[timing]
max_wait_secs = 180
max_wait_pct = 0.40
min_time_remaining = 60
cooldown_secs = 5
min_leg2_delay_secs = 3
max_trades_per_event = 2

[risk]
max_leg1_loss = 0.0
force_complete_threshold = 1.00

[model]
mu = 0.0
vol_lookback_secs = 300
vol_floor = 0.005

[filter]
allowed_windows = [300, 900]
window_tolerance = 30

[markets]
series_ids = ["10684", "10192", "10684"]
"#;
        let adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
        assert_eq!(adapter.config.backtest_config.shares_per_trade, 20);
        assert_eq!(adapter.config.backtest_config.max_concurrent_positions, 3);
        assert_eq!(adapter.config.backtest_config.symbols, vec!["BTCUSDT"]);
        assert_eq!(
            adapter.series_ids,
            vec!["10192".to_string(), "10684".to_string()]
        );
    }

    #[test]
    fn test_summary_empty() {
        let adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let summary = adapter.build_summary();
        assert!(summary.contains("trades=0"));
        assert!(summary.contains("open=0"));
    }

    #[tokio::test]
    async fn test_required_feeds() {
        let adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let feeds = adapter.required_feeds();
        assert_eq!(feeds.len(), 3);
        // Should have BinanceSpot, PolymarketEvents, Tick
        assert!(matches!(&feeds[0], DataFeed::BinanceSpot { .. }));
        match &feeds[1] {
            DataFeed::PolymarketEvents { series_ids } => {
                assert_eq!(series_ids, &default_staggered_series_ids());
            }
            _ => panic!("expected polymarket events feed"),
        }
        assert!(matches!(&feeds[2], DataFeed::Tick { .. }));
    }

    #[test]
    fn test_leg2_does_not_merge_when_fee_adjusted_pnl_is_negative() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();

        // Raw sum is 0.99 (< 1.00) but fee-adjusted total > 1.00.
        adapter
            .pm_asks
            .insert("BTCUSDT".into(), (Some(dec!(0.50)), Some(dec!(0.49))));
        adapter.positions.push(PaperPosition {
            symbol: "BTCUSDT".into(),
            event_id: "evt".into(),
            condition_id: None,
            up_token: "up".into(),
            down_token: "down".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.50),
            leg1_shares: 10,
            leg1_fee: dec!(0.075),
            leg1_time: now - chrono::Duration::seconds(10),
            wait_deadline: now + chrono::Duration::seconds(60),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);
        assert!(
            actions.is_empty(),
            "fee-adjusted negative trade should not auto-merge"
        );
    }

    #[test]
    fn test_min_balance_blocks_entry() {
        let toml = r#"
[entry]
symbols = ["BTCUSDT"]
initial_capital = 10.0
shares_per_trade = 5
direction_threshold = 0.0

[risk]
min_balance_usd = 9.0
"#;

        let mut adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
        let now = Utc::now();
        let window = LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-token".into(),
            down_token: "down-token".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(300),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(200),
            (Some(0.01), 100.0),
            Some(dec!(0.60)),
            Some(dec!(0.30)),
        );

        assert!(
            action.is_none(),
            "entry should be blocked when reserve balance would be violated"
        );
        assert!(
            adapter.positions.is_empty(),
            "no leg1 position should be opened when min_balance_usd blocks entry"
        );
    }

    #[test]
    fn test_force_threshold_not_triggered_without_timeout_or_risk() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();

        // Sum crosses threshold, but no timeout/time-safety/stop-loss condition is true.
        adapter
            .pm_asks
            .insert("BTCUSDT".into(), (Some(dec!(0.50)), Some(dec!(0.55))));
        adapter.positions.push(PaperPosition {
            symbol: "BTCUSDT".into(),
            event_id: "evt".into(),
            condition_id: None,
            up_token: "up".into(),
            down_token: "down".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.50),
            leg1_shares: 10,
            leg1_fee: dec!(0.075),
            leg1_time: now - chrono::Duration::seconds(10),
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);
        assert!(
            actions.is_empty(),
            "force_complete_threshold should not trigger without timeout/time-safety/risk"
        );
    }

    #[test]
    fn test_live_leg2_uses_position_tokens_even_without_active_window() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();

        adapter.positions.push(PaperPosition {
            symbol: "BTCUSDT".into(),
            event_id: "evt".into(),
            condition_id: None,
            up_token: "up-token".into(),
            down_token: "down-token".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.40),
            leg1_shares: 5,
            leg1_fee: dec!(0.03),
            leg1_time: now - chrono::Duration::seconds(10),
            wait_deadline: now + chrono::Duration::seconds(30),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let action = adapter.fill_leg2(0, dec!(0.62), "forced_timeout", now);
        assert!(
            matches!(action, Some(StrategyAction::SubmitOrder { .. })),
            "live leg2 should still submit even if active window already expired"
        );
    }
}
