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
use crate::domain::Domain;
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::strategy::crypto::{all_updown_series_ids, symbol_and_window_for_series};

mod entry;
mod leg2;
mod lifecycle;
mod order_updates;
mod reporting;
mod runtime_flow;
mod state_support;

use entry::{
    has_opening_window_candidate as has_opening_window_candidate_impl, try_entry as try_entry_impl,
    try_entry_for_window as try_entry_for_window_impl,
};
use lifecycle::{LiveOrderTrack, PaperPosition, PaperPositionState, PaperTrade};
use state_support::{LiveWindow, QuoteRoute};

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
        self.strategy_state()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.exported_positions()
    }

    fn is_active(&self) -> bool {
        self.is_strategy_active()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        Ok(self.shutdown_actions())
    }

    fn reset(&mut self) {
        self.reset_runtime_state();
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
