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
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::domain::Domain;
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
        Ok(self.handle_market_update(update))
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(self.handle_order_update(update))
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
mod tests;
