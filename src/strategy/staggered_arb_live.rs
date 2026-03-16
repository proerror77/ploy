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
use super::staggered_arb_backtest::{
    polymarket_order_meets_minimum, PmEventQuoteState, StaggeredArbBacktestConfig,
};
use super::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyOrderIntent, StrategyStateInfo,
};
use crate::adapters::SpotPrice;
use crate::domain::{OrderRequest, OrderStatus, OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::platform::Domain;
use crate::strategy::crypto::{all_updown_series_ids, symbol_and_window_for_series};

mod entry;
mod leg2;
mod lifecycle;
mod order_updates;
mod runtime_flow;

use entry::{
    has_opening_window_candidate as has_opening_window_candidate_impl, try_entry as try_entry_impl,
    try_entry_for_window as try_entry_for_window_impl,
};
use lifecycle::{LiveOrderTrack, PaperPosition, PaperPositionState, PaperTrade};

fn crypto_submit_intent(
    client_order_id: String,
    market_slug: String,
    token_id: String,
    side: Side,
    shares: u64,
    limit_price: Decimal,
    priority: u8,
) -> StrategyAction {
    StrategyAction::SubmitIntent {
        intent: StrategyOrderIntent {
            client_order_id,
            domain: Domain::Crypto,
            market_slug,
            token_id,
            side,
            is_buy: true,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority,
            metadata: HashMap::new(),
        },
    }
}

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
    all_updown_series_ids()
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

#[derive(Debug, Clone)]
struct QuoteRoute {
    event_id: String,
    symbol: String,
    direction: Direction,
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
    /// symbol -> previous Binance L2 OBI(top-5) for persistence / flip checks
    binance_l2_obi_prev_5: HashMap<String, Decimal>,
    /// symbol -> timestamp for latest Binance L2 OBI update
    binance_l2_obi_ts: HashMap<String, DateTime<Utc>>,
    /// event_id → (up_ask, down_ask)
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    /// event_id → quote freshness/persistence tracking for both sides
    pm_quote_state_by_event: HashMap<String, PmEventQuoteState>,
    /// token_id → quote routing metadata
    token_to_quote_route: HashMap<String, QuoteRoute>,

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
    /// Stale orders moved out of active cancellation loops but still eligible for late reconciliation.
    archived_live_orders: HashMap<String, LiveOrderTrack>,
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
    /// Entry gate reject counters partitioned by symbol for diagnostics.
    entry_reject_counts_by_symbol: HashMap<String, HashMap<String, u64>>,
    /// Leg2 skip counters (why close was skipped/deferred)
    leg2_skip_counts: HashMap<String, u64>,
    /// Leg2 skip counters partitioned by symbol for diagnostics.
    leg2_skip_counts_by_symbol: HashMap<String, HashMap<String, u64>>,
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
            binance_l2_obi_prev_5: HashMap::new(),
            binance_l2_obi_ts: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            pm_quote_state_by_event: HashMap::new(),
            token_to_quote_route: HashMap::new(),
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
            archived_live_orders: HashMap::new(),
            pending_leg1_events: HashSet::new(),
            pending_leg2_positions: HashSet::new(),
            fixed_amount_usd: None,
            min_balance_usd: Decimal::ZERO,
            fixed_amount_overage_warned: false,
            consecutive_balance_failures: 0,
            balance_pause_until: None,
            entry_reject_counts: HashMap::new(),
            entry_reject_counts_by_symbol: HashMap::new(),
            leg2_skip_counts: HashMap::new(),
            leg2_skip_counts_by_symbol: HashMap::new(),
        }
    }

    fn bump_entry_reject(&mut self, reason: &str) {
        *self
            .entry_reject_counts
            .entry(reason.to_string())
            .or_default() += 1;
    }

    fn bump_entry_reject_for_symbol(&mut self, symbol: &str, reason: &str) {
        self.bump_entry_reject(reason);
        *self
            .entry_reject_counts_by_symbol
            .entry(symbol.to_string())
            .or_default()
            .entry(reason.to_string())
            .or_default() += 1;
    }

    fn bump_leg2_skip(&mut self, reason: &str) {
        *self.leg2_skip_counts.entry(reason.to_string()).or_default() += 1;
    }

    fn bump_leg2_skip_for_symbol(&mut self, symbol: &str, reason: &str) {
        self.bump_leg2_skip(reason);
        *self
            .leg2_skip_counts_by_symbol
            .entry(symbol.to_string())
            .or_default()
            .entry(reason.to_string())
            .or_default() += 1;
    }

    /// Create from TOML configuration string.
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty = Value::Table(Default::default());
        let risk = config.get("risk").unwrap_or(&empty);
        let entry = config.get("entry").unwrap_or(&empty);
        let markets = config.get("markets").unwrap_or(&empty);
        let bc = StaggeredArbBacktestConfig::from_toml_str_with_default_symbols(
            config_str,
            vec!["BTCUSDT".into(), "ETHUSDT".into()],
        )?;

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
        symbol_and_window_for_series(series_id)
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

    fn effective_filled_shares(update_filled: u64, fallback_shares: u64) -> u64 {
        if update_filled > 0 {
            update_filled.min(fallback_shares)
        } else {
            fallback_shares
        }
    }

    fn settle_single_leg_position(
        &mut self,
        idx: usize,
        up_won: bool,
        settle_spot: Decimal,
        ts: DateTime<Utc>,
        actions: &mut Vec<StrategyAction>,
    ) {
        if idx >= self.positions.len() {
            return;
        }

        let (symbol, event_id, direction, leg1_price, leg1_shares, leg1_fee, leg1_time, won) = {
            let pos = &self.positions[idx];
            let won = matches!(pos.leg1_direction, Direction::Up) == up_won;
            (
                pos.symbol.clone(),
                pos.event_id.clone(),
                pos.leg1_direction.clone(),
                pos.leg1_price,
                pos.leg1_shares,
                pos.leg1_fee,
                pos.leg1_time,
                won,
            )
        };

        let payout = if won {
            Decimal::from(leg1_shares)
        } else {
            Decimal::ZERO
        };
        let total_cost = Decimal::from(leg1_shares) * leg1_price + leg1_fee;
        let pnl = payout - total_cost;
        let duration_secs = (ts - leg1_time).num_seconds();

        let pos = &mut self.positions[idx];
        pos.state = PaperPositionState::Settled;
        pos.leg2_time = Some(ts);

        self.closed_trades.push(PaperTrade {
            symbol: symbol.clone(),
            event_id,
            direction,
            leg1_price,
            leg2_price: Decimal::ZERO,
            total_cost,
            payout,
            pnl,
            exit_reason: "live_settlement".to_string(),
            duration_secs,
            opened_at: leg1_time,
            closed_at: ts,
        });

        info!(
            "[STAG-ARB] SETTLED {} spot={} payout=${:.4} pnl={}{:.4} wait={}s",
            symbol,
            settle_spot,
            payout,
            if pnl >= Decimal::ZERO { "+" } else { "" },
            pnl,
            duration_secs,
        );
        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::CycleCompleted,
                format!(
                    "[STAG-ARB] SETTLED {} payout=${:.4} pnl={}{:.4} wait={}s",
                    symbol,
                    payout,
                    if pnl >= Decimal::ZERO { "+" } else { "" },
                    pnl,
                    duration_secs
                ),
            ),
        });
    }

    fn record_pm_quote(
        &mut self,
        event_id: &str,
        direction: Direction,
        ask: Option<Decimal>,
        ask_size: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        let state = self
            .pm_quote_state_by_event
            .entry(event_id.to_string())
            .or_default();
        let side = match direction {
            Direction::Up => Side::Up,
            Direction::Down => Side::Down,
        };
        let side_state = state.side_mut(side);
        if self.config.backtest_config.pm_quote_max_stale_secs > 0 {
            if let Some(last_seen_at) = side_state.last_seen_at {
                if (ts - last_seen_at).num_seconds()
                    > self.config.backtest_config.pm_quote_max_stale_secs as i64
                {
                    side_state.clear();
                }
            }
        }
        state.update(side, ask, ask_size, ts);
        self.pm_asks_by_event
            .insert(event_id.to_string(), state.asks());
    }

    fn event_quote_state(
        &self,
        event_id: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) -> PmEventQuoteState {
        self.pm_quote_state_by_event
            .get(event_id)
            .copied()
            .unwrap_or_else(|| PmEventQuoteState::synthetic(up_ask, down_ask, ts))
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

    fn has_opening_window_candidate(&self, symbol: &str, ts: DateTime<Utc>) -> bool {
        has_opening_window_candidate_impl(self, symbol, ts)
    }

    // ─── Entry logic (ported from backtest engine) ──────────

    fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) -> Vec<StrategyAction> {
        try_entry_impl(self, symbol, ts)
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
        let predicted_up = if bc.reverse_signal {
            p_hat < 0.5
        } else {
            p_hat > 0.5
        };

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
                    exchange_order_id: None,
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
                purpose: crate::strategy::OrderPurpose::Entry,
                order,
                priority: 10,
            })
        }
    }

    // ─── Leg2 monitoring ──────────────────────────────────────

    fn summarize_gate_counts(
        counts: &HashMap<String, u64>,
        include_reasons: Option<&[&str]>,
        exclude_reasons: &[&str],
        limit: usize,
    ) -> String {
        let mut ranked: Vec<_> = counts
            .iter()
            .filter(|(reason, count)| {
                let reason = reason.as_str();
                let include_match =
                    include_reasons.map_or(true, |included| included.contains(&reason));
                let exclude_match = exclude_reasons.contains(&reason);
                **count > 0 && include_match && !exclude_match
            })
            .map(|(reason, count)| (reason.as_str(), *count))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

        if ranked.is_empty() {
            return "none".to_string();
        }

        ranked
            .into_iter()
            .take(limit)
            .map(|(reason, count)| format!("{}:{}", reason, count))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn summarize_symbol_gate_counts(
        counts_by_symbol: &HashMap<String, HashMap<String, u64>>,
        symbols: &[String],
        include_reasons: Option<&[&str]>,
        exclude_reasons: &[&str],
        per_symbol_limit: usize,
    ) -> String {
        let mut parts = Vec::new();
        for symbol in symbols {
            let Some(counts) = counts_by_symbol.get(symbol) else {
                continue;
            };
            let summary = Self::summarize_gate_counts(
                counts,
                include_reasons,
                exclude_reasons,
                per_symbol_limit,
            );
            if !summary.is_empty() {
                parts.push(format!("{}:[{}]", symbol, summary));
            }
        }

        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(";")
        }
    }

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
        let entry_timing_reasons = [
            "before_event_start",
            "entry_window_expired",
            "time_remaining_too_low",
        ];
        let entry_timing_gates = Self::summarize_gate_counts(
            &self.entry_reject_counts,
            Some(&entry_timing_reasons),
            &["entry_accepted"],
            3,
        );
        let entry_signal_gates = Self::summarize_gate_counts(
            &self.entry_reject_counts,
            None,
            &[
                "entry_accepted",
                "before_event_start",
                "entry_window_expired",
                "time_remaining_too_low",
            ],
            3,
        );
        let entry_signal_by_symbol = Self::summarize_symbol_gate_counts(
            &self.entry_reject_counts_by_symbol,
            &self.config.backtest_config.symbols,
            None,
            &[
                "entry_accepted",
                "before_event_start",
                "entry_window_expired",
                "time_remaining_too_low",
            ],
            1,
        );
        let leg2_gates = Self::summarize_gate_counts(&self.leg2_skip_counts, None, &[], 3);
        let leg2_by_symbol = Self::summarize_symbol_gate_counts(
            &self.leg2_skip_counts_by_symbol,
            &self.config.backtest_config.symbols,
            None,
            &[],
            1,
        );

        format!(
            "[STAG-ARB] equity=${:.2} trades={} win_rate={:.0}% avg_pnl=${:.4} open={} entry_timing_gates={} entry_signal_gates={} entry_signal_by_symbol={} leg2_gates={} leg2_by_symbol={}",
            self.equity, total, win_rate, avg_pnl, open, entry_timing_gates, entry_signal_gates, entry_signal_by_symbol, leg2_gates, leg2_by_symbol,
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
        Ok(self.handle_market_update(update))
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        if self.dry_run {
            return Ok(Vec::new());
        }
        let mut actions = Vec::new();

        let client_id = match &update.client_order_id {
            Some(id) => id.clone(),
            None => {
                // Cancel callbacks arrive without client_order_id — reverse-lookup by exchange hash
                match self
                    .live_orders
                    .iter()
                    .find(|(_, t)| t.exchange_order_id.as_deref() == Some(&update.order_id))
                    .map(|(k, _)| k.clone())
                {
                    Some(id) => id,
                    None => return Ok(Vec::new()),
                }
            }
        };

        // Store exchange order ID on first callback so we can cancel by exchange hash
        if let Some(track) = self.live_orders.get_mut(&client_id) {
            if track.exchange_order_id.is_none() && !update.order_id.is_empty() {
                track.exchange_order_id = Some(update.order_id.clone());
            }
        }

        let track = match self.live_orders.get(&client_id) {
            Some(t) => t.clone(),
            None => return Ok(Vec::new()),
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(track.price);
                let ts = update.timestamp;
                let filled_shares = Self::effective_filled_shares(update.filled_qty, track.shares);

                if track.leg == 1 {
                    self.record_leg1_fill(&track, filled_shares, fill_price, ts, &mut actions);
                } else {
                    // ── Leg2 filled → close position ──
                    if let Some(idx) = track.position_idx {
                        self.pending_leg2_positions.remove(&idx);

                        if idx < self.positions.len() {
                            let close_reason =
                                track.close_reason.as_deref().unwrap_or("merge").to_string();
                            let total_filled =
                                self.record_leg2_fill(idx, filled_shares, fill_price, ts);
                            let target = self.positions[idx].leg1_shares;

                            if total_filled >= target {
                                self.finalize_leg2_position(
                                    idx,
                                    close_reason.as_str(),
                                    ts,
                                    &mut actions,
                                );
                            } else {
                                let symbol = self.positions[idx].symbol.clone();
                                let avg = self.positions[idx].leg2_price.unwrap_or(fill_price);
                                info!(
                                    "[STAG-ARB] LEG2 PARTIAL FILL {} {}/{} shares avg={:.2}¢",
                                    symbol,
                                    total_filled,
                                    target,
                                    avg * dec!(100)
                                );
                            }
                        }
                    }
                }

                self.live_orders.remove(&client_id);
            }

            OrderStatus::Cancelled | OrderStatus::Failed => {
                // If this is an immediate synthetic cancel ack without client_order_id, wait for
                // the polling update carrying exchange-side fill details before cleanup.
                if update.status == OrderStatus::Cancelled
                    && update.client_order_id.is_none()
                    && update.filled_qty == 0
                    && track.cancel_requested_at.is_some()
                {
                    debug!(
                        "[STAG-ARB] LEG{} cancel ack without fill details {} {} — waiting for poll reconciliation",
                        track.leg, track.symbol, track.event_id
                    );
                    return Ok(actions);
                }

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

                let filled_shares = update.filled_qty.min(track.shares);
                if track.leg == 1 {
                    if filled_shares > 0 {
                        let fill_price = update.avg_fill_price.unwrap_or(track.price);
                        warn!(
                            "[STAG-ARB] LEG1 {:?} but partially filled: {} {} shares={} avg={:.2}¢",
                            update.status,
                            track.symbol,
                            track.event_id,
                            filled_shares,
                            fill_price * dec!(100)
                        );
                        self.record_leg1_fill(
                            &track,
                            filled_shares,
                            fill_price,
                            update.timestamp,
                            &mut actions,
                        );
                    } else {
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
                    }
                } else if let Some(idx) = track.position_idx {
                    self.pending_leg2_positions.remove(&idx);
                    if filled_shares > 0 {
                        let fill_price = update.avg_fill_price.unwrap_or(track.price);
                        let total_filled =
                            self.record_leg2_fill(idx, filled_shares, fill_price, update.timestamp);
                        let target = self
                            .positions
                            .get(idx)
                            .map(|p| p.leg1_shares)
                            .unwrap_or(filled_shares);
                        warn!(
                            "[STAG-ARB] LEG2 {:?} {} had partial fill shares={} total={}/{} before closure",
                            update.status, track.symbol, filled_shares
                            , total_filled, target
                        );
                        if total_filled >= target {
                            let close_reason =
                                track.close_reason.as_deref().unwrap_or("merge").to_string();
                            self.finalize_leg2_position(
                                idx,
                                close_reason.as_str(),
                                update.timestamp,
                                &mut actions,
                            );
                        } else {
                            info!(
                                "[STAG-ARB] LEG2 {:?} {} — will retry on next tick (filled {}/{})",
                                update.status, track.symbol, total_filled, target
                            );
                        }
                    } else {
                        let (filled, target) = self
                            .positions
                            .get(idx)
                            .map(|p| (Self::leg2_filled_shares(p), p.leg1_shares))
                            .unwrap_or((0, 0));
                        info!(
                            "[STAG-ARB] LEG2 {:?} {} — will retry on next tick (filled {}/{})",
                            update.status, track.symbol, filled, target
                        );
                    }
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
        Ok(self.handle_tick(now))
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
        for (symbol, counts) in self.entry_reject_counts_by_symbol.iter() {
            for (reason, count) in counts {
                metrics.insert(
                    format!("entry_gate_{}_{}", symbol, reason),
                    count.to_string(),
                );
            }
        }
        for (k, v) in self.leg2_skip_counts.iter() {
            metrics.insert(format!("leg2_gate_{}", k), v.to_string());
        }
        for (symbol, counts) in self.leg2_skip_counts_by_symbol.iter() {
            for (reason, count) in counts {
                metrics.insert(
                    format!("leg2_gate_{}_{}", symbol, reason),
                    count.to_string(),
                );
            }
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
        self.pm_asks_by_event.clear();
        self.pm_quote_state_by_event.clear();
        self.binance_l2_obi_5.clear();
        self.binance_l2_obi_prev_5.clear();
        self.binance_l2_obi_ts.clear();
        self.token_to_quote_route.clear();
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
    use crate::domain::OrderStatus;
    use crate::strategy::OrderUpdate;

    fn default_config() -> StaggeredArbLiveConfig {
        StaggeredArbLiveConfig {
            backtest_config: StaggeredArbBacktestConfig::default(),
            fee_rate: dec!(0.015),
        }
    }

    fn sample_leg1_track(now: DateTime<Utc>) -> LiveOrderTrack {
        LiveOrderTrack {
            event_id: "evt-1".to_string(),
            condition_id: Some("cond-1".to_string()),
            symbol: "ETHUSDT".to_string(),
            up_token: "up-token".to_string(),
            down_token: "down-token".to_string(),
            direction: Direction::Up,
            token_id: "up-token".to_string(),
            leg: 1,
            price: dec!(0.51),
            shares: 20,
            position_idx: None,
            close_reason: None,
            submitted_at: now - chrono::Duration::seconds(35),
            cancel_requested_at: Some(now - chrono::Duration::seconds(5)),
            exchange_order_id: Some("0xabc".to_string()),
        }
    }

    fn sample_leg2_track(now: DateTime<Utc>, shares: u64, idx: usize) -> LiveOrderTrack {
        LiveOrderTrack {
            event_id: "evt-1".to_string(),
            condition_id: Some("cond-1".to_string()),
            symbol: "ETHUSDT".to_string(),
            up_token: "up-token".to_string(),
            down_token: "down-token".to_string(),
            direction: Direction::Down,
            token_id: "down-token".to_string(),
            leg: 2,
            price: dec!(0.38),
            shares,
            position_idx: Some(idx),
            close_reason: Some("merge".to_string()),
            submitted_at: now - chrono::Duration::seconds(10),
            cancel_requested_at: None,
            exchange_order_id: Some("0xleg2".to_string()),
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
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.49))));
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
    fn test_try_entry_uses_event_scoped_quotes() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![
                LiveWindow {
                    event_id: "evt-a".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up-a".into(),
                    down_token: "down-a".into(),
                    condition_id: None,
                    end_time: now + chrono::Duration::seconds(240),
                    open_price: Some(dec!(100)),
                    window_secs: 300,
                },
                LiveWindow {
                    event_id: "evt-b".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up-b".into(),
                    down_token: "down-b".into(),
                    condition_id: None,
                    end_time: now + chrono::Duration::seconds(240),
                    open_price: Some(dec!(100)),
                    window_secs: 300,
                },
            ],
        );
        adapter
            .pm_asks_by_event
            .insert("evt-a".into(), (Some(dec!(0.55)), Some(dec!(0.30))));

        let actions = adapter.try_entry("BTCUSDT", now);

        assert_eq!(actions.len(), 1, "only the quoted event should be tradable");
        assert_eq!(adapter.positions.len(), 1);
        assert_eq!(adapter.positions[0].event_id, "evt-a");
    }

    #[test]
    fn test_try_entry_only_allows_opening_window_entries() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

        let within_open_window = LiveWindow {
            event_id: "evt-open".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-open".into(),
            down_token: "down-open".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(280),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };
        let late_window = LiveWindow {
            event_id: "evt-late".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-late".into(),
            down_token: "down-late".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(260),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let early_action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &within_open_window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );
        assert!(
            early_action.is_some(),
            "entry should be allowed during the configured opening window even when sum is above the old 0.92 cap"
        );

        let late_action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &late_window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );
        assert!(
            late_action.is_none(),
            "entry should be blocked once the opening window has expired"
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
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.55))));
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
    fn test_force_threshold_blocks_forced_timeout_above_cap() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.75)), Some(dec!(0.27))));
        adapter.positions.push(PaperPosition {
            symbol: "BTCUSDT".into(),
            event_id: "evt".into(),
            condition_id: None,
            up_token: "up".into(),
            down_token: "down".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.75),
            leg1_shares: 10,
            leg1_fee: dec!(0.1125),
            leg1_time: now - chrono::Duration::seconds(30),
            wait_deadline: now - chrono::Duration::seconds(1),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(
            actions.is_empty(),
            "forced timeout should be blocked when sum exceeds force_complete_threshold"
        );
    }

    #[tokio::test]
    async fn test_event_expired_settles_single_leg_position() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now,
                open_price: Some(dec!(100)),
                window_secs: 300,
            }],
        );
        adapter.positions.push(PaperPosition {
            symbol: "BTCUSDT".into(),
            event_id: "evt".into(),
            condition_id: None,
            up_token: "up".into(),
            down_token: "down".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 10,
            leg1_fee: dec!(0.0825),
            leg1_time: now - chrono::Duration::seconds(60),
            wait_deadline: now - chrono::Duration::seconds(1),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter
            .on_market_update(&MarketUpdate::EventExpired {
                event_id: "evt".into(),
            })
            .await
            .unwrap();

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, StrategyAction::LogEvent { .. })),
            "settlement should emit a cycle completion log"
        );
        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.positions[0].state, PaperPositionState::Settled);
        assert_eq!(adapter.closed_trades[0].payout, dec!(10));
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

    #[tokio::test]
    async fn test_leg1_cancelled_with_partial_fill_creates_position() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        let client_id = "cid-leg1".to_string();
        adapter
            .live_orders
            .insert(client_id.clone(), sample_leg1_track(now));
        adapter.pending_leg1_events.insert("evt-1".to_string());

        let update = OrderUpdate {
            order_id: "0xabc".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::Cancelled,
            filled_qty: 7,
            avg_fill_price: Some(dec!(0.52)),
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();

        assert!(
            !adapter.pending_leg1_events.contains("evt-1"),
            "partial-cancelled leg1 should clear event pending lock"
        );
        assert!(
            !adapter.live_orders.contains_key(&client_id),
            "partial-cancelled leg1 should be removed from live order tracking"
        );
        assert_eq!(
            adapter.positions.len(),
            1,
            "leg1 partial fill should open position"
        );
        let pos = &adapter.positions[0];
        assert_eq!(pos.leg1_shares, 7);
        assert_eq!(pos.leg1_price, dec!(0.52));
        assert_eq!(pos.state, PaperPositionState::Leg1Filled);
    }

    #[tokio::test]
    async fn test_leg1_cancel_ack_without_fill_details_waits_for_poll_update() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        let client_id = "cid-leg1".to_string();
        adapter
            .live_orders
            .insert(client_id.clone(), sample_leg1_track(now));
        adapter.pending_leg1_events.insert("evt-1".to_string());

        let update = OrderUpdate {
            order_id: "0xabc".to_string(),
            client_order_id: None, // cancel ack path
            status: OrderStatus::Cancelled,
            filled_qty: 0,
            avg_fill_price: None,
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();

        assert!(
            adapter.pending_leg1_events.contains("evt-1"),
            "synthetic cancel ack should not clear pending lock before reconciliation"
        );
        assert!(
            adapter.live_orders.contains_key(&client_id),
            "synthetic cancel ack should keep live order for poll reconciliation"
        );
        assert!(
            adapter.positions.is_empty(),
            "no fill details => no position should be created yet"
        );
    }

    #[tokio::test]
    async fn test_leg2_partial_cancel_tracks_progress_and_only_resubmits_remaining() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        adapter.positions.push(PaperPosition {
            symbol: "ETHUSDT".into(),
            event_id: "evt-1".into(),
            condition_id: Some("cond-1".into()),
            up_token: "up-token".into(),
            down_token: "down-token".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.62),
            leg1_shares: 20,
            leg1_fee: dec!(0.186),
            leg1_time: now - chrono::Duration::seconds(20),
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let client_id = "cid-leg2".to_string();
        adapter
            .live_orders
            .insert(client_id.clone(), sample_leg2_track(now, 20, 0));
        adapter.pending_leg2_positions.insert(0);

        let update = OrderUpdate {
            order_id: "0xleg2".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::Cancelled,
            filled_qty: 7,
            avg_fill_price: Some(dec!(0.38)),
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();
        assert!(
            !adapter.pending_leg2_positions.contains(&0),
            "leg2 partial cancel should clear in-flight marker so remaining shares can retry"
        );
        assert!(
            !adapter.live_orders.contains_key(&client_id),
            "leg2 partial cancel should remove completed attempt from tracking"
        );
        let pos = &adapter.positions[0];
        assert_eq!(pos.leg2_shares, Some(7));
        assert_eq!(pos.state, PaperPositionState::Leg1Filled);

        let action = adapter.fill_leg2(0, dec!(0.40), "merge", now);
        match action {
            Some(StrategyAction::SubmitOrder { order, .. }) => {
                assert_eq!(order.shares, 13, "should only submit remaining shares")
            }
            _ => panic!("expected leg2 submit action"),
        }
    }

    #[tokio::test]
    async fn test_leg2_partial_then_full_fill_closes_once_with_weighted_price() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        adapter.positions.push(PaperPosition {
            symbol: "ETHUSDT".into(),
            event_id: "evt-1".into(),
            condition_id: Some("cond-1".into()),
            up_token: "up-token".into(),
            down_token: "down-token".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.62),
            leg1_shares: 20,
            leg1_fee: dec!(0.186),
            leg1_time: now - chrono::Duration::seconds(20),
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: Some(dec!(0.38)),
            leg2_shares: Some(7),
            leg2_fee: Some(dec!(0.0399)),
            leg2_time: Some(now - chrono::Duration::seconds(10)),
            state: PaperPositionState::Leg1Filled,
        });

        let client_id = "cid-leg2-fill".to_string();
        adapter
            .live_orders
            .insert(client_id.clone(), sample_leg2_track(now, 13, 0));
        adapter.pending_leg2_positions.insert(0);

        let update = OrderUpdate {
            order_id: "0xleg2fill".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::Filled,
            filled_qty: 13,
            avg_fill_price: Some(dec!(0.39)),
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();

        let pos = &adapter.positions[0];
        assert_eq!(
            pos.state,
            PaperPositionState::Merged,
            "position should close when cumulative leg2 shares reach leg1 size"
        );
        assert_eq!(pos.leg2_shares, Some(20));
        assert_eq!(adapter.closed_trades.len(), 1, "should only close once");
        let trade = &adapter.closed_trades[0];
        assert_eq!(trade.payout, dec!(20));
        assert_eq!(trade.leg2_price, dec!(0.3865));
    }
}
