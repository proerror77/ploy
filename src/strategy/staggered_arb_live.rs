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
use crate::domain::{OrderStatus, OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::platform::Domain;
use crate::strategy::crypto::{all_updown_series_ids, symbol_and_window_for_series};

mod entry;
mod lifecycle;

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

    fn forced_close_allowed(
        &self,
        current_sum: Decimal,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> bool {
        let threshold = self.config.backtest_config.force_close_threshold_now(
            time_remaining_secs,
            window_duration_secs,
            in_final_window,
        );
        threshold <= Decimal::ZERO || current_sum <= threshold
    }

    fn protective_close_allowed(
        &self,
        current_sum: Decimal,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> bool {
        let threshold = self.config.backtest_config.protective_close_threshold_now(
            time_remaining_secs,
            window_duration_secs,
            in_final_window,
        );
        threshold <= Decimal::ZERO || current_sum <= threshold
    }

    fn premium_sum_excess(&self, current_sum: Decimal) -> f64 {
        let threshold = self.config.backtest_config.premium_sum_threshold;
        if current_sum <= threshold {
            0.0
        } else {
            (current_sum - threshold).to_f64().unwrap_or(0.0).max(0.0)
        }
    }

    fn current_sigma_for_symbol(&self, symbol: &str, bc: &StaggeredArbBacktestConfig) -> f64 {
        self.spot_prices
            .get(symbol)
            .and_then(|s| s.volatility(bc.vol_lookback_secs))
            .and_then(|v| v.to_f64())
            .map(|tick_vol| {
                let n = self
                    .spot_prices
                    .get(symbol)
                    .map(|s| s.history_len().min(5000) as f64)
                    .unwrap_or(100.0);
                (tick_vol * n.sqrt()).max(bc.vol_floor)
            })
            .unwrap_or(bc.vol_floor)
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

    fn current_window_greeks(
        &self,
        symbol: &str,
        event_id: &str,
        time_remaining: f64,
    ) -> Option<super::gamma_scalping::greeks::BinaryGreeks> {
        let bc = &self.config.backtest_config;
        if !bc.use_greeks || time_remaining <= 0.0 {
            return None;
        }
        let window = self
            .active_windows
            .get(symbol)
            .and_then(|ws| ws.iter().find(|w| w.event_id == event_id))?;
        let s0 = window.open_price?;
        if s0 <= Decimal::ZERO {
            return None;
        }
        let st = self.spot_prices.get(symbol)?.price;
        let sigma = self.current_sigma_for_symbol(symbol, bc);
        super::gamma_scalping::greeks::binary_greeks(
            st.to_f64().unwrap_or(0.0),
            s0.to_f64().unwrap_or(0.0),
            sigma,
            time_remaining,
            window.window_secs as f64,
        )
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
        try_entry_for_window_impl(self, symbol, ts, window, st, vol_info, up_ask, down_ask)
    }

    // ─── Leg2 monitoring ──────────────────────────────────────

    fn check_leg2_opportunities(&mut self, symbol: &str, ts: DateTime<Utc>) -> Vec<StrategyAction> {
        let mut actions = Vec::new();
        let bc = self.config.backtest_config.clone();
        let mut leg2_skip_batch: HashMap<&'static str, u64> = HashMap::new();

        // Collect indices + actions (can't mutate while iterating)
        let mut leg2_fills: Vec<(usize, Decimal, String)> = Vec::new();
        let mut protective_arm_updates: Vec<(usize, Option<DateTime<Utc>>)> = Vec::new();
        let mut saw_event_quotes = false;

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.state != PaperPositionState::Leg1Filled {
                continue;
            }

            let pm_asks = match self.pm_asks_by_event.get(&pos.event_id) {
                Some(a) => {
                    saw_event_quotes = true;
                    *a
                }
                None => {
                    *leg2_skip_batch.entry("missing_event_quotes").or_default() += 1;
                    continue;
                }
            };
            let quote_state = self.event_quote_state(&pos.event_id, pm_asks.0, pm_asks.1, ts);

            // Skip positions with in-flight Leg2 orders
            if self.pending_leg2_positions.contains(&i) {
                *leg2_skip_batch.entry("leg2_order_pending").or_default() += 1;
                continue;
            }

            let (time_remaining, window_secs, window_open) = match self.active_windows.get(symbol) {
                Some(windows) => windows
                    .iter()
                    .find(|w| w.event_id == pos.event_id)
                    .map(|w| {
                        (
                            (w.end_time - ts).num_seconds() as f64,
                            w.window_secs,
                            w.open_price,
                        )
                    })
                    .unwrap_or((f64::MAX, 0, None)),
                None => (f64::MAX, 0, None),
            };
            let current_greeks = self.current_window_greeks(symbol, &pos.event_id, time_remaining);
            let current_obi = self
                .binance_l2_obi_5
                .get(symbol)
                .map(|value| value.to_f64().unwrap_or(0.0));
            let in_final_window = bc.no_trade_last_secs > 0
                && time_remaining <= bc.no_trade_last_secs as f64
                && time_remaining > 0.0;
            let displacement_supportive = window_open
                .filter(|open| *open > Decimal::ZERO)
                .and_then(|open| {
                    self.spot_prices
                        .get(symbol)
                        .map(|sp| ((sp.price - open) / open).to_f64().unwrap_or(0.0))
                })
                .map(|displacement| match pos.leg1_direction {
                    Direction::Up => displacement > 0.0,
                    Direction::Down => displacement < 0.0,
                })
                .unwrap_or(false);
            let greeks_supportive = current_greeks
                .as_ref()
                .map(|g| match pos.leg1_direction {
                    Direction::Up => g.d2 > 0.05 && g.fair_value > 0.5,
                    Direction::Down => g.d2 < -0.05 && g.fair_value < 0.5,
                })
                .unwrap_or(!bc.use_greeks);

            let (other_ask, other_state, leg1_mark, leg1_mark_state) = match pos.leg1_direction {
                Direction::Up => (pm_asks.1, quote_state.down, pm_asks.0, quote_state.up),
                Direction::Down => (pm_asks.0, quote_state.up, pm_asks.1, quote_state.down),
            };
            if !bc.pm_quote_is_fresh(other_state.last_seen_at, ts) {
                *leg2_skip_batch.entry("stale_other_ask").or_default() += 1;
                continue;
            }
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

            if let Some(ref g) = current_greeks {
                if !in_final_window && current_sum < Decimal::ONE {
                    let gamma_urgency = g.gamma.abs().min(1.0);
                    let adjusted_target = bc.min_profit_target
                        * Decimal::from_f64(1.0 - gamma_urgency * 0.8).unwrap_or(Decimal::ONE);
                    if current_sum < bc.merge_target_sum + adjusted_target {
                        leg2_fills.push((i, other_ask, "merge".to_string()));
                        continue;
                    }
                }

                if bc.max_theta_cost > 0.0 {
                    let theta_cost_remaining = g.theta.abs() * time_remaining.max(0.0);
                    if theta_cost_remaining > bc.max_theta_cost {
                        if !in_final_window && current_sum <= Decimal::ONE {
                            leg2_fills.push((i, other_ask, "merge".to_string()));
                            continue;
                        }
                        if self.protective_close_allowed(
                            current_sum,
                            time_remaining,
                            window_secs,
                            in_final_window,
                        ) {
                            leg2_fills.push((i, other_ask, "protective_theta".to_string()));
                            continue;
                        }
                        *leg2_skip_batch
                            .entry("protective_threshold_blocked")
                            .or_default() += 1;
                        continue;
                    }
                }
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
                let leg1_mark = if bc.pm_quote_is_fresh(leg1_mark_state.last_seen_at, ts) {
                    leg1_mark
                } else {
                    None
                };
                if let Some(mark) = leg1_mark {
                    let leg1_loss = (pos.leg1_price - mark).max(Decimal::ZERO);
                    if leg1_loss >= bc.max_leg1_loss {
                        let obi_supportive = bc.obi_signal_still_supportive(
                            pos.leg1_direction,
                            pos.entry_obi,
                            current_obi,
                        );
                        if obi_supportive && displacement_supportive && greeks_supportive {
                            protective_arm_updates.push((i, None));
                            *leg2_skip_batch
                                .entry("protective_signal_still_supportive")
                                .or_default() += 1;
                            continue;
                        }
                        let hard_signal_broken = bc
                            .obi_signal_hard_flipped(pos.leg1_direction, current_obi)
                            || (!displacement_supportive && !greeks_supportive);
                        let armed_at = pos.protective_stop_armed_at.unwrap_or(ts);
                        let recovery_elapsed = (ts - armed_at).num_seconds();
                        let recovery_expired = bc.protective_recovery_window_secs == 0
                            || recovery_elapsed >= bc.protective_recovery_window_secs as i64;
                        if !hard_signal_broken && !recovery_expired {
                            protective_arm_updates.push((i, Some(armed_at)));
                            *leg2_skip_batch
                                .entry("protective_recovery_window")
                                .or_default() += 1;
                            continue;
                        }
                        protective_arm_updates.push((i, None));
                        if self.protective_close_allowed(
                            current_sum,
                            time_remaining,
                            window_secs,
                            in_final_window,
                        ) {
                            leg2_fills.push((i, other_ask, "protective_stop_loss".to_string()));
                        } else {
                            *leg2_skip_batch
                                .entry("protective_threshold_blocked")
                                .or_default() += 1;
                        }
                        continue;
                    } else if pos.protective_stop_armed_at.is_some() {
                        protective_arm_updates.push((i, None));
                    }
                }
            }

            // E. Timeout — force-complete — always allowed
            if ts >= pos.wait_deadline && leg2_ready {
                if self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    leg2_fills.push((i, other_ask, "forced_timeout".to_string()));
                } else {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                }
                continue;
            }

            // F. Time safety — not enough time left — always allowed
            if time_remaining < bc.min_time_remaining_secs as f64 && leg2_ready {
                if self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    leg2_fills.push((i, other_ask, "forced_time_safety".to_string()));
                } else {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                }
                continue;
            }

            // G. Final window close — this profile should keep hedge discipline.
            //
            // In the last no_trade_last_secs, always try to buy Leg2 if the
            // forced-close threshold still allows it. We still log p_win /
            // displacement for diagnostics, but we no longer intentionally
            // hold a single-leg into settlement.
            if in_final_window && leg2_ready {
                let window_info = self
                    .active_windows
                    .get(symbol)
                    .and_then(|ws| ws.iter().find(|w| w.event_id == pos.event_id));
                let s0 = window_info.and_then(|w| w.open_price);
                let window_secs = window_info.map(|w| w.window_secs).unwrap_or(300);
                let st = self.spot_prices.get(symbol).map(|s| s.price);
                let sigma = self
                    .spot_prices
                    .get(symbol)
                    .and_then(|s| s.volatility(bc.vol_lookback_secs))
                    .and_then(|v| v.to_f64())
                    .map(|tick_vol| {
                        let n = self
                            .spot_prices
                            .get(symbol)
                            .map(|s| s.history_len().min(5000) as f64)
                            .unwrap_or(100.0);
                        (tick_vol * n.sqrt()).max(bc.vol_floor)
                    })
                    .unwrap_or(bc.vol_floor);

                match (s0, st) {
                    (Some(s0_val), Some(st_val)) if s0_val > Decimal::ZERO => {
                        let p_hat =
                            estimate_probability(s0_val, st_val, sigma, time_remaining, bc.mu);
                        let p_win = match pos.leg1_direction {
                            Direction::Up => p_hat,
                            Direction::Down => 1.0 - p_hat,
                        };
                        let displacement =
                            ((st_val - s0_val) / s0_val).to_f64().unwrap_or(0.0).abs();
                        let near_strike = displacement < 0.001; // within 10 bps
                        let vol_time_ratio =
                            sigma / (time_remaining / window_secs as f64).max(0.01);
                        let high_vol_regime = vol_time_ratio > 0.05;
                        info!(
                            "[STAG-ARB] FINAL WINDOW CLOSE {} {} p_win={:.3} disp={:.4} near_strike={} high_vol={} — buying Leg2",
                            symbol,
                            pos.leg1_direction,
                            p_win,
                            displacement,
                            near_strike,
                            high_vol_regime,
                        );
                    }
                    _ => {
                        info!(
                            "[STAG-ARB] FINAL WINDOW CLOSE {} {} without price context — buying Leg2",
                            symbol, pos.leg1_direction,
                        );
                    }
                }
                if !self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                    continue;
                }
                leg2_fills.push((i, other_ask, "forced_final_window".to_string()));
                continue;
            }
        }

        if !saw_event_quotes {
            self.bump_leg2_skip_for_symbol(symbol, "missing_pm_quotes");
        }

        for (idx, armed_at) in protective_arm_updates {
            if let Some(pos) = self.positions.get_mut(idx) {
                pos.protective_stop_armed_at = armed_at;
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
            *self
                .leg2_skip_counts_by_symbol
                .entry(symbol.to_string())
                .or_default()
                .entry(reason.to_string())
                .or_default() += count;
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
        let symbol = pos.symbol.clone();
        let already_filled = Self::leg2_filled_shares(pos);
        let shares = Self::leg2_remaining_shares(pos);
        if shares == 0 {
            return None;
        }
        if !polymarket_order_meets_minimum(other_ask, shares) {
            self.bump_leg2_skip_for_symbol(&symbol, "leg2_residual_below_venue_minimum");
            return None;
        }

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

            let tag = if reason == "merge" {
                "COMPLETE"
            } else {
                "FORCED"
            };
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
                    token_id: token_id.clone(),
                    leg: 2,
                    price: other_ask,
                    shares,
                    position_idx: Some(idx),
                    close_reason: Some(reason.to_string()),
                    submitted_at: ts,
                    cancel_requested_at: None,
                    exchange_order_id: None,
                    acknowledged_filled_qty: already_filled,
                    entry_obi: pos.entry_obi,
                },
            );
            self.pending_leg2_positions.insert(idx);

            let tag = if reason == "merge" {
                "COMPLETE"
            } else {
                "FORCED"
            };
            let msg = format!(
                "[STAG-ARB] LEG2 {} SUBMIT {} @ {:.2}¢ ({} shares, ${:.2}) reason={} filled={}/{}",
                tag,
                symbol,
                other_ask * dec!(100),
                shares,
                other_ask.to_f64().unwrap_or(0.0) * shares as f64,
                reason,
                already_filled,
                pos.leg1_shares,
            );
            info!("{}", msg);

            Some(crypto_submit_intent(
                client_order_id,
                event_id,
                token_id,
                side,
                shares,
                other_ask,
                10,
            ))
        }
    }

    // ─── Periodic summary ────────────────────────────────────

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
                if let Some(prev) = self.binance_l2_obi_5.insert(symbol.clone(), *obi_5) {
                    self.binance_l2_obi_prev_5.insert(symbol.clone(), prev);
                }
                self.binance_l2_obi_ts.insert(symbol.clone(), *timestamp);
            }

            MarketUpdate::PolymarketQuote {
                token_id,
                quote,
                timestamp,
                ..
            } => {
                if let Some(route) = self.token_to_quote_route.get(token_id) {
                    let symbol = route.symbol.clone();
                    let event_id = route.event_id.clone();
                    let direction = route.direction.clone();
                    let ask = quote.best_ask;
                    let ts = *timestamp;

                    self.record_pm_quote(&event_id, direction, ask, quote.ask_size, ts);

                    // Check Leg2 opportunities first (existing positions)
                    let leg2_actions = self.check_leg2_opportunities(&symbol, ts);
                    actions.extend(leg2_actions);

                    // Then try new entries
                    let entry_actions = self.try_entry(&symbol, ts);
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

                self.token_to_quote_route.insert(
                    up_token.clone(),
                    QuoteRoute {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        direction: Direction::Up,
                    },
                );
                self.token_to_quote_route.insert(
                    down_token.clone(),
                    QuoteRoute {
                        event_id: event_id.clone(),
                        symbol: symbol.to_string(),
                        direction: Direction::Down,
                    },
                );

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
                let expired_windows: Vec<LiveWindow> = self
                    .active_windows
                    .values()
                    .flat_map(|windows| windows.iter())
                    .filter(|w| w.event_id == *event_id)
                    .cloned()
                    .collect();
                for window in &expired_windows {
                    self.settle_expired_event(window, Utc::now(), &mut actions);
                }
                for windows in self.active_windows.values_mut() {
                    windows.retain(|w| w.event_id != *event_id);
                }
                self.pm_asks_by_event.remove(event_id);
                self.pm_quote_state_by_event.remove(event_id);
                self.token_to_quote_route
                    .retain(|_, route| route.event_id != *event_id);
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
            None => {
                // Cancel callbacks arrive without client_order_id — reverse-lookup by exchange hash
                match self
                    .live_orders
                    .iter()
                    .chain(self.archived_live_orders.iter())
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
        } else if let Some(track) = self.archived_live_orders.get_mut(&client_id) {
            if track.exchange_order_id.is_none() && !update.order_id.is_empty() {
                track.exchange_order_id = Some(update.order_id.clone());
            }
        }

        let track = match self
            .live_orders
            .get(&client_id)
            .or_else(|| self.archived_live_orders.get(&client_id))
        {
            Some(t) => t.clone(),
            None => return Ok(Vec::new()),
        };
        let ts = update.timestamp;
        let fill_price = update.avg_fill_price.unwrap_or(track.price);
        let cumulative_filled = Self::effective_cumulative_filled_qty(&track, update);
        let filled_delta = Self::incremental_filled_shares(&track, update);

        match update.status {
            OrderStatus::Filled => {
                if track.leg == 1 {
                    let position_idx = if filled_delta > 0 {
                        Some(self.record_leg1_fill(
                            &track,
                            filled_delta,
                            fill_price,
                            ts,
                            &mut actions,
                        ))
                    } else {
                        track.position_idx
                    };
                    self.update_order_fill_progress(&client_id, cumulative_filled, position_idx);
                } else {
                    // ── Leg2 filled → close position ──
                    if let Some(idx) = track.position_idx {
                        self.pending_leg2_positions.remove(&idx);

                        if idx < self.positions.len() {
                            if self.positions[idx].state != PaperPositionState::Leg1Filled {
                                self.remove_order_tracking(&client_id);
                                return Ok(actions);
                            }
                            let close_reason =
                                track.close_reason.as_deref().unwrap_or("merge").to_string();
                            let total_filled = if filled_delta > 0 {
                                self.record_leg2_fill(idx, filled_delta, fill_price, ts)
                            } else {
                                Self::leg2_filled_shares(&self.positions[idx])
                            };
                            self.update_order_fill_progress(
                                &client_id,
                                cumulative_filled,
                                Some(idx),
                            );
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

                self.remove_order_tracking(&client_id);
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

                let position_idx = track.position_idx;
                if track.leg == 1 {
                    if filled_delta > 0 {
                        warn!(
                            "[STAG-ARB] LEG1 {:?} but partially filled: {} {} shares={} avg={:.2}¢",
                            update.status,
                            track.symbol,
                            track.event_id,
                            filled_delta,
                            fill_price * dec!(100)
                        );
                        let idx = self.record_leg1_fill(
                            &track,
                            filled_delta,
                            fill_price,
                            ts,
                            &mut actions,
                        );
                        self.update_order_fill_progress(&client_id, cumulative_filled, Some(idx));
                    } else {
                        if position_idx.is_none() {
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
                    }
                } else if let Some(idx) = position_idx {
                    self.pending_leg2_positions.remove(&idx);
                    if idx < self.positions.len()
                        && self.positions[idx].state != PaperPositionState::Leg1Filled
                    {
                        self.remove_order_tracking(&client_id);
                        return Ok(actions);
                    }
                    if filled_delta > 0 {
                        let total_filled = self.record_leg2_fill(idx, filled_delta, fill_price, ts);
                        self.update_order_fill_progress(&client_id, cumulative_filled, Some(idx));
                        let target = self
                            .positions
                            .get(idx)
                            .map(|p| p.leg1_shares)
                            .unwrap_or(filled_delta);
                        warn!(
                            "[STAG-ARB] LEG2 {:?} {} had partial fill shares={} total={}/{} before closure",
                            update.status, track.symbol, filled_delta
                            , total_filled, target
                        );
                        if total_filled >= target {
                            let close_reason =
                                track.close_reason.as_deref().unwrap_or("merge").to_string();
                            self.finalize_leg2_position(
                                idx,
                                close_reason.as_str(),
                                ts,
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
                self.remove_order_tracking(&client_id);
            }

            OrderStatus::PartiallyFilled => {
                if track.leg == 1 {
                    let position_idx = if filled_delta > 0 {
                        Some(self.record_leg1_fill(
                            &track,
                            filled_delta,
                            fill_price,
                            ts,
                            &mut actions,
                        ))
                    } else {
                        track.position_idx
                    };
                    self.update_order_fill_progress(&client_id, cumulative_filled, position_idx);

                    if filled_delta > 0 {
                        let cancel_id =
                            if let Some(track_mut) = self.live_orders.get_mut(&client_id) {
                                if track_mut.cancel_requested_at.is_none() {
                                    track_mut.cancel_requested_at = Some(ts);
                                    Some(
                                        track_mut
                                            .exchange_order_id
                                            .clone()
                                            .unwrap_or_else(|| client_id.clone()),
                                    )
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        if let Some(order_id) = cancel_id {
                            info!(
                                "[STAG-ARB] LEG1 PARTIAL ACCEPT {} {} cumulative={}/{} — cancelling remainder",
                                track.symbol,
                                track.event_id,
                                cumulative_filled,
                                track.shares,
                            );
                            actions.push(StrategyAction::CancelOrder { order_id });
                        }
                    }
                } else if let Some(idx) = track.position_idx {
                    if idx < self.positions.len()
                        && self.positions[idx].state != PaperPositionState::Leg1Filled
                    {
                        self.remove_order_tracking(&client_id);
                        return Ok(actions);
                    }
                    if filled_delta > 0 {
                        let total_filled = self.record_leg2_fill(idx, filled_delta, fill_price, ts);
                        self.update_order_fill_progress(&client_id, cumulative_filled, Some(idx));
                        let target = self.positions.get(idx).map(|p| p.leg1_shares).unwrap_or(0);
                        info!(
                            "[STAG-ARB] LEG2 PARTIAL FILL {} {}/{} shares avg={:.2}¢",
                            track.symbol,
                            total_filled,
                            target,
                            self.positions[idx].leg2_price.unwrap_or(fill_price) * dec!(100)
                        );
                    }
                }
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

            OrderStatus::Submitted => {
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
                // Use exchange order hash for cancel (CLOB requires it), fall back to client_id
                let cancel_id = track
                    .exchange_order_id
                    .clone()
                    .unwrap_or_else(|| client_id.clone());
                info!(
                    "[STAG-ARB] STALE ORDER CANCEL leg={} {} {} age={}s price={:.2}¢ exchange_id={}",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                    track.price * dec!(100),
                    cancel_id,
                );
                track.cancel_requested_at = Some(now);
                actions.push(StrategyAction::CancelOrder {
                    order_id: cancel_id,
                });
            }
        }

        // Phase 2: move orphaned orders out of the active cancellation loop, but keep
        // reconciliation metadata and event/position locks intact. Clearing the locks here
        // can reopen the same event or submit duplicate hedges while the venue still has a
        // live or recently-filled order we have not heard back about yet.
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
                    "[STAG-ARB] ORPHAN ORDER ARCHIVE leg={} {} {} age={}s — no callback received, keeping lock for reconciliation",
                    track.leg,
                    track.symbol,
                    track.event_id,
                    (now - track.submitted_at).num_seconds(),
                );
                self.archived_live_orders.insert(client_id, track);
            }
        }

        // 1. Clean expired windows
        for windows in self.active_windows.values_mut() {
            windows.retain(|w| w.end_time > now);
        }

        // 2. Re-run leg2 checks for all active symbols and any still-open positions.
        let mut symbols: HashSet<String> = self.active_windows.keys().cloned().collect();
        symbols.extend(
            self.positions
                .iter()
                .filter(|p| p.state == PaperPositionState::Leg1Filled)
                .map(|p| p.symbol.clone()),
        );
        for symbol in &symbols {
            let leg2_actions = self.check_leg2_opportunities(symbol, now);
            actions.extend(leg2_actions);
        }

        // 3. Re-run entry checks on tick so opening-window Leg1s do not depend on
        // a fresh Polymarket quote callback arriving inside the first 30 seconds.
        for symbol in &symbols {
            if self.has_opening_window_candidate(symbol, now) {
                let entry_actions = self.try_entry(symbol, now);
                actions.extend(entry_actions);
            }
        }

        // 4. Periodic summary (every 60s)
        let should_print = self
            .last_summary
            .map(|t| (now - t).num_seconds() >= 60)
            .unwrap_or(true);
        if should_print {
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
            acknowledged_filled_qty: 0,
            entry_obi: Some(0.02),
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
            acknowledged_filled_qty: 0,
            entry_obi: Some(-0.02),
        }
    }

    fn seed_persistent_pm_quotes(
        adapter: &mut StaggeredArbAdapter,
        event_id: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        first_seen_at: DateTime<Utc>,
        last_seen_at: DateTime<Utc>,
    ) {
        adapter.record_pm_quote(event_id, Direction::Up, up_ask, None, first_seen_at);
        adapter.record_pm_quote(event_id, Direction::Down, down_ask, None, first_seen_at);
        if last_seen_at != first_seen_at {
            adapter.record_pm_quote(event_id, Direction::Up, up_ask, None, last_seen_at);
            adapter.record_pm_quote(event_id, Direction::Down, down_ask, None, last_seen_at);
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
premium_sum_threshold = 1.00
premium_sum_direction_slope = 1.25
premium_sum_obi_slope = 0.25
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
        assert_eq!(
            adapter.config.backtest_config.premium_sum_threshold,
            Decimal::ONE
        );
        assert_eq!(
            adapter.config.backtest_config.premium_sum_direction_slope,
            1.25
        );
        assert_eq!(adapter.config.backtest_config.premium_sum_obi_slope, 0.25);
        assert_eq!(adapter.config.backtest_config.obi_confirm_threshold, 0.005);
        assert_eq!(adapter.config.backtest_config.strong_obi_threshold, 0.015);
        assert_eq!(adapter.config.backtest_config.symbols, vec!["BTCUSDT"]);
        assert_eq!(
            adapter.series_ids,
            vec!["10192".to_string(), "10684".to_string()]
        );
    }

    #[test]
    fn test_from_toml_defaults_match_delayed_entry_profile() {
        let toml = r#"
[strategy]
name = "staggered_arb"
"#;

        let adapter = StaggeredArbAdapter::from_toml("test".into(), toml, true).unwrap();
        let config = &adapter.config.backtest_config;

        assert_eq!(config.max_concurrent_positions, 0);
        assert_eq!(config.max_initial_sum, Decimal::ZERO);
        assert_eq!(config.entry_after_start_min_secs, 30);
        assert_eq!(config.entry_after_start_max_secs, 240);
        assert_eq!(config.pm_quote_max_stale_secs, 10);
        assert_eq!(config.entry_quote_persistence_secs, 8);
        assert_eq!(config.strong_obi_window_bonus_secs, 60);
        assert_eq!(config.allowed_window_durations, vec![300]);
        assert_eq!(config.protective_recovery_window_secs, 0);
        assert_eq!(config.max_trades_per_event, 0);
        assert_eq!(config.force_complete_threshold, dec!(1.06));
        assert_eq!(config.protective_close_threshold, dec!(1.06));
        assert_eq!(config.obi_decay_exit_ratio, 0.35);
        assert_eq!(config.obi_flip_exit_threshold, 0.008);
        assert_eq!(config.min_entry_sum, dec!(0.30));
        assert_eq!(config.max_entry_sigma, 0.0);
    }

    #[test]
    fn test_strong_obi_bonus_adjusts_entry_thresholds() {
        let config = StaggeredArbBacktestConfig::default();
        assert!(config.strong_obi_entry_bonus_active(
            true,
            0.02,
            Some(0.01),
            dec!(1.02),
            Some(0.03)
        ));
        assert!((config.direction_threshold_now(dec!(1.02), true) - 0.06).abs() < 1e-9);
        assert_eq!(config.max_leg1_price_now(true), dec!(0.58));
        assert_eq!(config.entry_after_start_max_secs_now(900, true), 300);
    }

    #[test]
    fn test_summary_empty() {
        let adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let summary = adapter.build_summary();
        assert!(summary.contains("trades=0"));
        assert!(summary.contains("open=0"));
    }

    #[test]
    fn test_summary_includes_per_symbol_gate_breakdown() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        adapter.config.backtest_config.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
        adapter.bump_entry_reject_for_symbol("BTCUSDT", "obi_stale");
        adapter.bump_leg2_skip_for_symbol("ETHUSDT", "missing_pm_quotes");

        let summary = adapter.build_summary();

        assert!(summary.contains("entry_signal_by_symbol=BTCUSDT:[obi_stale:1]"));
        assert!(summary.contains("leg2_by_symbol=ETHUSDT:[missing_pm_quotes:1]"));
    }

    #[test]
    fn test_live_leg1_submit_sets_client_order_and_idempotency_key() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_initial_sum = Decimal::ZERO;
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
        adapter.config.backtest_config.entry_after_start_max_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.03));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-live-order",
            Some(dec!(0.55)),
            Some(dec!(0.48)),
            now - chrono::Duration::seconds(10),
            now,
        );

        let window = LiveWindow {
            event_id: "evt-live-order".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-live".into(),
            down_token: "down-live".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(260),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter
            .try_entry_for_window(
                "BTCUSDT",
                now,
                &window,
                dec!(101),
                (Some(0.01), 100.0),
                Some(dec!(0.55)),
                Some(dec!(0.48)),
            )
            .expect("entry should be accepted");

        match action {
            StrategyAction::SubmitIntent { intent } => {
                let order = crate::strategy::order_request_from_intent(&intent);
                assert_eq!(order.client_order_id, intent.client_order_id);
                assert_eq!(
                    order.idempotency_key.as_deref(),
                    Some(intent.client_order_id.as_str())
                );
                assert_eq!(intent.market_slug, "evt-live-order");
            }
            other => panic!("expected submit intent action, got {:?}", other),
        }
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
            entry_obi: None,
            protective_stop_armed_at: None,
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
        adapter.config.backtest_config.max_entry_sigma = 0.20;
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
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
                    end_time: now + chrono::Duration::seconds(280),
                    open_price: Some(dec!(100)),
                    window_secs: 300,
                },
                LiveWindow {
                    event_id: "evt-b".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up-b".into(),
                    down_token: "down-b".into(),
                    condition_id: None,
                    end_time: now + chrono::Duration::seconds(280),
                    open_price: Some(dec!(100)),
                    window_secs: 300,
                },
            ],
        );
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-a",
            Some(dec!(0.55)),
            Some(dec!(0.30)),
            now - chrono::Duration::seconds(10),
            now,
        );

        let actions = adapter.try_entry("BTCUSDT", now);

        assert_eq!(actions.len(), 1, "only the quoted event should be tradable");
        assert_eq!(adapter.positions.len(), 1);
        assert_eq!(adapter.positions[0].event_id, "evt-a");
    }

    #[test]
    fn test_try_entry_waits_for_post_open_delay_then_allows() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        let later = now + chrono::Duration::seconds(25);
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_entry_sigma = 0.20;
        adapter.config.backtest_config.entry_after_start_min_secs = 30;
        adapter.config.backtest_config.entry_after_start_max_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, later));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), later);
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-delayed",
            Some(dec!(0.55)),
            Some(dec!(0.48)),
            now - chrono::Duration::seconds(10),
            now,
        );

        let window = LiveWindow {
            event_id: "evt-delayed".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-delayed".into(),
            down_token: "down-delayed".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(290),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let too_early_action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        );
        assert!(
            too_early_action.is_none(),
            "entry should be blocked during the initial observation delay before the post-open entry window begins"
        );

        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-delayed",
            Some(dec!(0.55)),
            Some(dec!(0.48)),
            later - chrono::Duration::seconds(10),
            later,
        );

        let delayed_action = adapter.try_entry_for_window(
            "BTCUSDT",
            later,
            &window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        );
        assert!(
            delayed_action.is_some(),
            "entry should be allowed once the configured post-open delay has elapsed"
        );
    }

    #[test]
    fn test_try_entry_allows_high_sum_when_max_initial_sum_is_disabled() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_initial_sum = Decimal::ZERO;
        adapter.config.backtest_config.max_leg1_price = dec!(0.60);
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
        adapter.config.backtest_config.entry_after_start_max_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(103), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.03));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-premium",
            Some(dec!(0.58)),
            Some(dec!(0.50)),
            now - chrono::Duration::seconds(10),
            now,
        );

        let window = LiveWindow {
            event_id: "evt-premium".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-premium".into(),
            down_token: "down-premium".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(260),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(103),
            (Some(0.001), 100.0),
            Some(dec!(0.58)),
            Some(dec!(0.50)),
        );

        assert!(
            action.is_some(),
            "entry should be allowed to rely on OBI/direction when max_initial_sum is explicitly disabled"
        );
    }

    #[test]
    fn test_try_entry_does_not_cap_concurrency_when_max_concurrent_is_zero() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_concurrent_positions = 0;
        adapter.config.backtest_config.max_trades_per_event = 0;
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
        adapter.config.backtest_config.entry_after_start_max_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.03));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-new",
            Some(dec!(0.55)),
            Some(dec!(0.48)),
            now - chrono::Duration::seconds(10),
            now,
        );
        adapter.positions.push(PaperPosition {
            symbol: "ETHUSDT".into(),
            event_id: "evt-existing".into(),
            condition_id: None,
            up_token: "up-existing".into(),
            down_token: "down-existing".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.51),
            leg1_shares: 20,
            leg1_fee: dec!(0.153),
            leg1_time: now - chrono::Duration::seconds(20),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let window = LiveWindow {
            event_id: "evt-new".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-new".into(),
            down_token: "down-new".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(260),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.01), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        );

        assert!(
            action.is_some(),
            "max_concurrent=0 should disable the concurrency cap instead of blocking every new entry"
        );
    }

    #[test]
    fn test_try_entry_rejects_sigma_above_max_entry_sigma() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_initial_sum = dec!(1.20);
        adapter.config.backtest_config.max_entry_sigma = 0.01;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

        let window = LiveWindow {
            event_id: "evt-open".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-open".into(),
            down_token: "down-open".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(280),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.02), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );

        assert!(
            action.is_none(),
            "entry should be blocked when realized sigma exceeds the configured regime cap"
        );
    }

    #[tokio::test]
    async fn test_on_tick_retries_entry_during_opening_window_without_new_quote_callback() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.entry_after_start_min_secs = 30;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt-open".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-open".into(),
                down_token: "down-open".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(260),
                open_price: Some(dec!(100)),
                window_secs: 300,
            }],
        );
        seed_persistent_pm_quotes(
            &mut adapter,
            "evt-open",
            Some(dec!(0.55)),
            Some(dec!(0.45)),
            now - chrono::Duration::seconds(10),
            now,
        );

        let actions = adapter.on_tick(now).await.unwrap();

        assert_eq!(
            adapter.positions.len(),
            1,
            "tick-driven recheck should open leg1 inside the configured opening window"
        );
        assert!(
            actions.iter().any(|action| matches!(
                action,
                StrategyAction::LogEvent { event }
                    if matches!(event.event_type, StrategyEventType::EntryTriggered)
            )),
            "tick-driven recheck should emit an EntryTriggered event when it opens leg1"
        );
    }

    #[test]
    fn test_try_entry_requires_persistent_other_ask_before_leg1() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        let later = now + chrono::Duration::seconds(9);
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101), None, later));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), later);

        let window = LiveWindow {
            event_id: "evt-persist".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-persist".into(),
            down_token: "down-persist".into(),
            condition_id: None,
            end_time: later + chrono::Duration::seconds(280),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        adapter.record_pm_quote("evt-persist", Direction::Up, Some(dec!(0.55)), None, now);
        adapter.record_pm_quote("evt-persist", Direction::Down, Some(dec!(0.45)), None, now);
        let early_action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );
        assert!(
            early_action.is_none(),
            "entry should wait until the opposite-side ask has persisted for the configured duration"
        );

        adapter.record_pm_quote("evt-persist", Direction::Up, Some(dec!(0.55)), None, later);
        adapter.record_pm_quote(
            "evt-persist",
            Direction::Down,
            Some(dec!(0.45)),
            None,
            later,
        );
        let delayed_action = adapter.try_entry_for_window(
            "BTCUSDT",
            later,
            &window,
            dec!(101),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );
        assert!(
            delayed_action.is_some(),
            "entry should proceed once the opposite-side ask has stayed visible long enough"
        );
    }

    #[test]
    fn test_record_pm_quote_resets_persistence_after_stale_gap() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let first_seen_at = Utc::now();
        let reappeared_at = first_seen_at + chrono::Duration::seconds(20);

        adapter.record_pm_quote(
            "evt-persist",
            Direction::Down,
            Some(dec!(0.45)),
            None,
            first_seen_at,
        );
        adapter.record_pm_quote(
            "evt-persist",
            Direction::Down,
            Some(dec!(0.45)),
            None,
            reappeared_at,
        );

        let state = adapter
            .pm_quote_state_by_event
            .get("evt-persist")
            .copied()
            .expect("quote state should exist");
        assert_eq!(
            state.down.first_seen_at,
            Some(reappeared_at),
            "a quote that reappears after a stale gap must restart persistence timing"
        );
        assert!(
            !adapter
                .config
                .backtest_config
                .entry_quote_is_persistent(state.down.first_seen_at, reappeared_at),
            "reappearing quotes should not immediately satisfy the persistence gate"
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
            entry_obi: None,
            protective_stop_armed_at: None,
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
            entry_obi: None,
            protective_stop_armed_at: None,
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

    #[test]
    fn test_stop_loss_uses_protective_close_threshold() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
        adapter.config.backtest_config.protective_close_threshold = dec!(1.03);
        adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.48))));
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
            leg1_time: now - chrono::Duration::seconds(30),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
    }

    #[test]
    fn test_dynamic_protective_threshold_blocks_early_expensive_stop_loss() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
        adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.52))));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(300),
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
            leg1_time: now - chrono::Duration::seconds(10),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(actions.is_empty());
        assert_eq!(adapter.closed_trades.len(), 0);
    }

    #[test]
    fn test_supportive_obi_skips_protective_stop_loss() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.53))));
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.6), None, now));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.01));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(300),
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
            leg1_time: now - chrono::Duration::seconds(10),
            entry_obi: Some(0.02),
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(actions.is_empty());
        assert_eq!(adapter.closed_trades.len(), 0);
    }

    #[test]
    fn test_protective_stop_arms_then_waits_before_closing() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
        adapter
            .config
            .backtest_config
            .protective_recovery_window_secs = 12;
        adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.005));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(300),
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
            leg1_time: now - chrono::Duration::seconds(10),
            entry_obi: Some(0.02),
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(actions.is_empty());
        assert_eq!(adapter.closed_trades.len(), 0);
        assert_eq!(adapter.positions[0].protective_stop_armed_at, Some(now));

        let actions =
            adapter.check_leg2_opportunities("BTCUSDT", now + chrono::Duration::seconds(13));

        assert_eq!(actions.len(), 1);
        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
    }

    #[test]
    fn test_hard_obi_flip_bypasses_protective_recovery_window() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.max_leg1_loss = dec!(0.05);
        adapter
            .config
            .backtest_config
            .protective_recovery_window_secs = 12;
        adapter.config.backtest_config.protective_close_threshold = dec!(1.08);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.50)), Some(dec!(0.47))));
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(-0.02));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(300),
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
            leg1_time: now - chrono::Duration::seconds(10),
            entry_obi: Some(0.02),
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(actions.len(), 1);
        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "protective_stop_loss");
    }

    #[test]
    fn test_dynamic_force_threshold_allows_late_close_within_cap() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.force_complete_threshold = dec!(1.08);
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.75)), Some(dec!(0.32))));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(20),
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
            leg1_price: dec!(0.75),
            leg1_shares: 10,
            leg1_fee: dec!(0.1125),
            leg1_time: now - chrono::Duration::seconds(30),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now - chrono::Duration::seconds(1),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "forced_timeout");
    }

    #[test]
    fn test_theta_urgency_uses_protective_close_threshold() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.force_complete_threshold = Decimal::ONE;
        adapter.config.backtest_config.protective_close_threshold = dec!(1.02);
        adapter.config.backtest_config.max_theta_cost = 1e-12;
        adapter.config.backtest_config.use_greeks = true;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.2), None, now));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(20),
                open_price: Some(dec!(100)),
                window_secs: 300,
            }],
        );
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.47))));
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
            leg1_time: now - chrono::Duration::seconds(30),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let _actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "protective_theta");
    }

    #[test]
    fn test_try_entry_rejects_far_from_mid_fair_value_for_long_gamma_profile() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.max_fair_value_distance = 0.20;
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.02));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

        let window = LiveWindow {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up".into(),
            down_token: "down".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(250),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.01), 200.0),
            Some(dec!(0.55)),
            Some(dec!(0.42)),
        );

        assert!(
            action.is_none(),
            "entry should be rejected when fair value is too far from mid and the long-gamma band is enabled"
        );
    }

    #[test]
    fn test_try_entry_requires_stronger_obi_for_premium_sum() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.direction_threshold = 0.0;
        adapter.config.backtest_config.premium_sum_direction_slope = 0.0;
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.entry_after_start_min_secs = 0;
        adapter
            .binance_l2_obi_5
            .insert("BTCUSDT".into(), dec!(0.01));
        adapter.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

        let window = LiveWindow {
            event_id: "evt-premium".into(),
            symbol: "BTCUSDT".into(),
            up_token: "up-premium".into(),
            down_token: "down-premium".into(),
            condition_id: None,
            end_time: now + chrono::Duration::seconds(280),
            open_price: Some(dec!(100)),
            window_secs: 300,
        };

        let action = adapter.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(100.04),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        );

        assert!(
            action.is_none(),
            "premium-sum entries should require stronger OBI confirmation than base entries"
        );
        assert_eq!(
            adapter
                .entry_reject_counts
                .get("obi_not_confirmed_for_premium_entry")
                .copied()
                .unwrap_or(0),
            1
        );
    }

    #[test]
    fn test_live_greeks_can_accelerate_leg2_close_before_merge_target() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.merge_target_sum = dec!(0.90);
        adapter.config.backtest_config.min_profit_target = dec!(0.12);
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
                end_time: now + chrono::Duration::seconds(250),
                open_price: Some(dec!(100)),
                window_secs: 300,
            }],
        );
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.44))));
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
            leg1_time: now - chrono::Duration::seconds(20),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(
            actions.iter().any(|a| matches!(a, StrategyAction::LogEvent { .. })),
            "high-gamma state should allow live leg2 completion before the normal merge target is hit"
        );
        assert_eq!(adapter.closed_trades.len(), 1);
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
            entry_obi: None,
            protective_stop_armed_at: None,
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

    #[tokio::test]
    async fn test_event_expired_settles_partial_leg2_without_double_close() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(99), None, now));
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
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now - chrono::Duration::seconds(1),
            leg2_price: Some(dec!(0.40)),
            leg2_shares: Some(4),
            leg2_fee: Some(dec!(0.024)),
            leg2_time: Some(now - chrono::Duration::seconds(5)),
            state: PaperPositionState::Leg1Filled,
        });

        let client_id = "cid-expiry-leg2".to_string();
        let mut track = sample_leg2_track(now - chrono::Duration::seconds(10), 6, 0);
        track.event_id = "evt".to_string();
        track.symbol = "BTCUSDT".to_string();
        adapter.live_orders.insert(client_id.clone(), track);
        adapter.pending_leg2_positions.insert(0);

        let _actions = adapter
            .on_market_update(&MarketUpdate::EventExpired {
                event_id: "evt".into(),
            })
            .await
            .unwrap();

        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.positions[0].state, PaperPositionState::Settled);
        assert_eq!(adapter.closed_trades[0].payout, dec!(4));
        assert!(
            !adapter.pending_leg2_positions.contains(&0),
            "expiry settlement should clear pending leg2 markers for the event"
        );
        assert!(
            !adapter.live_orders.contains_key(&client_id),
            "expiry settlement should retire outstanding leg2 tracking for the event"
        );

        let late_update = OrderUpdate {
            order_id: "0xleg2fill".to_string(),
            client_order_id: Some(client_id),
            status: OrderStatus::Filled,
            filled_qty: 6,
            avg_fill_price: Some(dec!(0.39)),
            timestamp: now + chrono::Duration::seconds(1),
            error: None,
        };
        let late_actions = adapter.on_order_update(&late_update).await.unwrap();

        assert!(late_actions.is_empty());
        assert_eq!(
            adapter.closed_trades.len(),
            1,
            "late leg2 updates after settlement must not close the same cycle twice"
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
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(30),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let action = adapter.fill_leg2(0, dec!(0.62), "forced_timeout", now);
        assert!(
            matches!(action, Some(StrategyAction::SubmitIntent { .. })),
            "live leg2 should still submit even if active window already expired"
        );
    }

    #[test]
    fn test_live_fill_leg2_skips_residual_below_venue_minimum() {
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
            leg1_shares: 20,
            leg1_fee: dec!(0.12),
            leg1_time: now - chrono::Duration::seconds(60),
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(30),
            leg2_price: Some(dec!(0.63)),
            leg2_shares: Some(19),
            leg2_fee: Some(dec!(0.17955)),
            leg2_time: Some(now - chrono::Duration::seconds(5)),
            state: PaperPositionState::Leg1Filled,
        });

        let action = adapter.fill_leg2(0, dec!(0.63), "forced_timeout", now);

        assert!(
            action.is_none(),
            "live leg2 should not submit venue-invalid residual orders"
        );
        assert!(adapter.live_orders.is_empty());
        assert!(!adapter.pending_leg2_positions.contains(&0));
        assert_eq!(adapter.positions[0].leg2_shares, Some(19));
    }

    #[test]
    fn test_final_window_high_confidence_still_forces_leg2() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), true);
        let now = Utc::now();
        adapter.config.backtest_config.use_greeks = false;
        adapter.config.backtest_config.min_leg2_delay_secs = 0;
        adapter.config.backtest_config.min_time_remaining_secs = 0;
        adapter
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(101.2), None, now));
        adapter.active_windows.insert(
            "BTCUSDT".into(),
            vec![LiveWindow {
                event_id: "evt".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up".into(),
                down_token: "down".into(),
                condition_id: None,
                end_time: now + chrono::Duration::seconds(10),
                open_price: Some(dec!(100)),
                window_secs: 300,
            }],
        );
        adapter
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.40))));
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
            leg1_time: now - chrono::Duration::seconds(30),
            entry_obi: Some(0.02),
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(30),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let actions = adapter.check_leg2_opportunities("BTCUSDT", now);

        assert!(
            actions.iter().any(|a| matches!(a, StrategyAction::LogEvent { .. })),
            "final-window positions should close through leg2 instead of intentionally holding a single leg"
        );
        assert_eq!(adapter.closed_trades.len(), 1);
        assert_eq!(adapter.closed_trades[0].exit_reason, "forced_final_window");
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
    async fn test_leg1_partially_filled_updates_position_immediately_and_requests_cancel() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        let client_id = "cid-leg1-partial".to_string();
        let mut track = sample_leg1_track(now);
        track.cancel_requested_at = None;
        adapter.live_orders.insert(client_id.clone(), track);
        adapter.pending_leg1_events.insert("evt-1".to_string());

        let update = OrderUpdate {
            order_id: "0xabc".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::PartiallyFilled,
            filled_qty: 7,
            avg_fill_price: Some(dec!(0.52)),
            timestamp: now,
            error: None,
        };

        let actions = adapter.on_order_update(&update).await.unwrap();

        assert_eq!(
            adapter.positions.len(),
            1,
            "partial fill should create the live leg1 position immediately"
        );
        assert_eq!(adapter.positions[0].leg1_shares, 7);
        assert!(
            actions.iter().any(|action| matches!(action, StrategyAction::CancelOrder { .. })),
            "once we accept a partial leg1 as the actual position size, the remaining order should be cancelled promptly"
        );
        assert!(
            adapter.live_orders.contains_key(&client_id),
            "the live order track should remain until the exchange confirms terminal cleanup"
        );
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
            entry_obi: None,
            protective_stop_armed_at: None,
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
            Some(StrategyAction::SubmitIntent { intent }) => {
                assert_eq!(intent.shares, 13, "should only submit remaining shares")
            }
            _ => panic!("expected leg2 submit action"),
        }
    }

    #[tokio::test]
    async fn test_leg2_partially_filled_updates_progress_before_terminal_status() {
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
            entry_obi: None,
            protective_stop_armed_at: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            leg2_price: None,
            leg2_shares: None,
            leg2_fee: None,
            leg2_time: None,
            state: PaperPositionState::Leg1Filled,
        });

        let client_id = "cid-leg2-partial".to_string();
        adapter
            .live_orders
            .insert(client_id.clone(), sample_leg2_track(now, 20, 0));
        adapter.pending_leg2_positions.insert(0);

        let update = OrderUpdate {
            order_id: "0xleg2".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::PartiallyFilled,
            filled_qty: 7,
            avg_fill_price: Some(dec!(0.38)),
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();

        assert_eq!(
            adapter.positions[0].leg2_shares,
            Some(7),
            "leg2 partial progress should be recorded immediately instead of waiting for cancel/failed terminal callbacks"
        );
        assert!(
            adapter.live_orders.contains_key(&client_id),
            "leg2 live order should stay tracked while the exchange order is still active"
        );
        assert!(
            adapter.pending_leg2_positions.contains(&0),
            "leg2 should remain marked in-flight until the terminal update arrives"
        );
    }

    #[tokio::test]
    async fn test_orphan_leg1_cleanup_keeps_lock_and_allows_late_reconciliation() {
        let mut adapter = StaggeredArbAdapter::new("test".into(), default_config(), false);
        let now = Utc::now();
        let client_id = "cid-orphan-leg1".to_string();
        let mut track = sample_leg1_track(now - chrono::Duration::seconds(100));
        track.cancel_requested_at = Some(now - chrono::Duration::seconds(70));
        adapter.live_orders.insert(client_id.clone(), track);
        adapter.pending_leg1_events.insert("evt-1".to_string());

        let _actions = adapter.on_tick(now).await.unwrap();

        assert!(
            !adapter.live_orders.contains_key(&client_id),
            "hard cleanup should move the stale order out of active tracking"
        );
        assert!(
            adapter.archived_live_orders.contains_key(&client_id),
            "stale order should stay archived for later reconciliation"
        );
        assert!(
            adapter.pending_leg1_events.contains("evt-1"),
            "same-event lock must remain until reconciliation or expiry"
        );

        let update = OrderUpdate {
            order_id: "0xabc".to_string(),
            client_order_id: Some(client_id.clone()),
            status: OrderStatus::Filled,
            filled_qty: 7,
            avg_fill_price: Some(dec!(0.52)),
            timestamp: now,
            error: None,
        };

        let _actions = adapter.on_order_update(&update).await.unwrap();

        assert_eq!(
            adapter.positions.len(),
            1,
            "late fill should still reconcile into a real position"
        );
        assert_eq!(adapter.positions[0].leg1_shares, 7);
        assert!(
            !adapter.pending_leg1_events.contains("evt-1"),
            "late reconciliation should finally release the event lock"
        );
        assert!(
            !adapter.archived_live_orders.contains_key(&client_id),
            "terminal reconciliation should retire the archived track"
        );
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
            entry_obi: None,
            protective_stop_armed_at: None,
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
