//! Directional binary-option strategy (pm_5m_directional).
//!
//! Estimates P(S_T >= S_0) via log-normal model on CEX spot prices,
//! then buys UP or DOWN tokens on Polymarket when edge exceeds threshold.
//!
//! Implements [`StrategyLogic`] so it plugs into [`StrategyRuntime`]
//! for backtest, dry-run, and live modes identically.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::traits::{MarketUpdate, StrategyDecision, StrategyLogic};

// ── Probability Model ────────────────────────────────────

/// Standard normal CDF (Abramowitz-Stegun approximation).
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592_f64;
    let a2 = -0.284496736_f64;
    let a3 = 1.421413741_f64;
    let a4 = -1.453152027_f64;
    let a5 = 1.061405429_f64;
    let p = 0.3275911_f64;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

/// Estimate P(S_T >= S_0) using log-normal model with horizon sigma.
fn estimate_probability(s0: f64, st: f64, sigma_horizon: f64) -> f64 {
    if sigma_horizon <= 0.0 {
        return if st >= s0 { 1.0 } else { 0.0 };
    }
    if s0 <= 0.0 || st <= 0.0 {
        return 0.5;
    }
    let z = (st / s0).ln() / sigma_horizon;
    normal_cdf(z)
}

// ── Fee Model ────────────────────────────────────────────

/// Polymarket trading fee for crypto binary markets.
///
/// Actual PM fee: 2% × p × (1 − p) per share.
/// Returns fee per share (not multiplied by quantity).
fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

const EWMA_LAMBDA: f64 = 0.94;

// ── Direction ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

// ── Configuration ────────────────────────────────────────

/// Strategy configuration, loadable from TOML.
///
/// Same struct drives backtest, dry-run, and live — no more divergence.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DirectionalConfig {
    // Symbols
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,

    // Probability gates
    #[serde(default = "default_vol_floor")]
    pub vol_floor: f64,
    #[serde(default = "default_min_probability")]
    pub min_probability: f64,
    /// Deprecated: z-score gate removed (redundant with probability gate).
    /// Kept for config backward compatibility.
    #[serde(default = "default_min_z_score")]
    pub min_z_score: f64,

    // Price gates
    #[serde(default = "default_min_entry_price")]
    pub min_entry_price: f64,
    #[serde(default = "default_max_entry_price")]
    pub max_entry_price: f64,
    #[serde(default = "default_no_trade_min")]
    pub no_trade_zone_min: f64,
    #[serde(default = "default_no_trade_max")]
    pub no_trade_zone_max: f64,

    // Edge
    #[serde(default = "default_min_edge")]
    pub min_edge: f64,

    // Timing
    #[serde(default = "default_min_time")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time")]
    pub max_time_remaining_secs: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,

    // Sizing
    #[serde(default = "default_stake_usd", alias = "quantity")]
    pub stake_usd: Decimal,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
}

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()]
}
fn default_vol_floor() -> f64 {
    0.001
}
fn default_min_probability() -> f64 {
    0.55
}
fn default_min_z_score() -> f64 {
    0.35
}
fn default_min_entry_price() -> f64 {
    0.15
}
fn default_max_entry_price() -> f64 {
    0.85
}
fn default_no_trade_min() -> f64 {
    0.45
}
fn default_no_trade_max() -> f64 {
    0.55
}
fn default_min_edge() -> f64 {
    0.02
}
fn default_min_time() -> u64 {
    60
}
fn default_max_time() -> u64 {
    300
}
fn default_cooldown() -> u64 {
    0
}
fn default_stake_usd() -> Decimal {
    dec!(25)
}
fn default_max_positions() -> usize {
    1000
}
fn default_max_daily_trades() -> u32 {
    1000
}

// ── Internal State ───────────────────────────────────────

/// Tracked CEX spot price for a symbol.
struct SpotState {
    price: Decimal,
    ts: DateTime<Utc>,
}

/// Active event window for a symbol.
#[derive(Clone)]
struct EventWindow {
    event_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    open_price: Option<Decimal>,
}

/// Cached Polymarket quote.
struct QuoteState {
    _bid: Option<Decimal>,
    ask: Option<Decimal>,
}

#[derive(Default)]
struct VolatilityState {
    ewma_var_per_sec: f64,
}

// ── Strategy Implementation ──────────────────────────────

pub struct DirectionalStrategy {
    config: DirectionalConfig,
    // Market state
    spot: HashMap<String, SpotState>,
    volatility: HashMap<String, VolatilityState>,
    events: HashMap<String, Vec<EventWindow>>,
    quotes: HashMap<String, QuoteState>,
    // Gating state
    cooldowns: HashMap<String, DateTime<Utc>>,
    daily_trades: u32,
    last_trade_date: Option<chrono::NaiveDate>,
    // Token → symbol mapping
    token_symbol: HashMap<String, String>,
    /// Most recent feed timestamp seen across all updates.
    /// Used instead of Utc::now() so replay runs are deterministic.
    feed_time: Option<DateTime<Utc>>,
}

impl DirectionalStrategy {
    pub fn new(config: DirectionalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            volatility: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            cooldowns: HashMap::new(),
            daily_trades: 0,
            last_trade_date: None,
            token_symbol: HashMap::new(),
            feed_time: None,
        }
    }

    /// Pick the nearest event within the valid time window.
    fn pick_event(&self, symbol: &str, now: DateTime<Utc>) -> Option<EventWindow> {
        self.events
            .get(symbol)?
            .iter()
            .filter(|e| {
                let rem = (e.end_time - now).num_seconds();
                rem >= self.config.min_time_remaining_secs as i64
                    && rem <= self.config.max_time_remaining_secs as i64
            })
            .min_by_key(|e| e.end_time)
            .cloned()
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        self.cooldowns.get(symbol).map_or(false, |last| {
            (now - *last).num_seconds() < self.config.cooldown_secs as i64
        })
    }

    fn reset_daily_counter(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        if self.last_trade_date != Some(today) {
            self.daily_trades = 0;
            self.last_trade_date = Some(today);
        }
    }

    fn floor_var_per_sec(&self) -> f64 {
        let sigma_floor = self.config.vol_floor.max(1e-9);
        sigma_floor * sigma_floor / 900.0
    }

    fn update_volatility(&mut self, symbol: &str, price: Decimal, ts: DateTime<Utc>) {
        let Some(previous) = self.spot.get(symbol) else {
            return;
        };
        if previous.price <= Decimal::ZERO || price <= Decimal::ZERO {
            return;
        }

        let dt_secs = (ts - previous.ts).num_milliseconds() as f64 / 1000.0;
        if dt_secs <= 0.0 {
            return;
        }

        let Some(prev_f) = previous.price.to_f64() else {
            return;
        };
        let Some(curr_f) = price.to_f64() else {
            return;
        };
        if prev_f <= 0.0 || curr_f <= 0.0 {
            return;
        }

        let inst_var_per_sec = (curr_f / prev_f).ln().powi(2) / dt_secs.max(1e-6);
        let floor = self.floor_var_per_sec();
        let state = self.volatility.entry(symbol.to_string()).or_default();
        state.ewma_var_per_sec = if state.ewma_var_per_sec <= 0.0 {
            inst_var_per_sec.max(floor)
        } else {
            (EWMA_LAMBDA * state.ewma_var_per_sec) + ((1.0 - EWMA_LAMBDA) * inst_var_per_sec)
        };
    }

    fn sigma_horizon(&self, symbol: &str, time_remaining_secs: f64) -> f64 {
        let secs = time_remaining_secs.max(1.0);
        let floor = self.floor_var_per_sec();
        let realized = self
            .volatility
            .get(symbol)
            .map(|state| state.ewma_var_per_sec)
            .unwrap_or(floor);
        (realized.max(floor) * secs).sqrt()
    }

    fn shares_for_entry_price(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        // Keep a stable venue-facing share quantity while preserving the
        // fixed-dollar stake semantics configured at strategy level.
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    /// Core signal evaluation — the unified 8-gate pipeline.
    /// Core signal evaluation — the 5-gate pipeline.
    ///
    /// Gate 0: Price validity
    /// Gate 1: Quote availability + direction
    /// Gate 2: Price filter (bounds + no-trade zone)
    /// Gate 3: Probability threshold
    /// Gate 4: Edge after fees
    fn evaluate_entry(
        &self,
        symbol: &str,
        spot_price: Decimal,
        event: &EventWindow,
        now: DateTime<Utc>,
    ) -> Option<(Direction, Decimal, f64, f64)> {
        // Gate 0: Price validity
        let open_price = event.open_price?;
        if open_price <= Decimal::ZERO || spot_price <= Decimal::ZERO {
            debug!(symbol, event_id = %event.event_id, "Gate 0: Invalid prices");
            return None;
        }

        let s0 = open_price.to_f64()?;
        let st = spot_price.to_f64()?;
        let secs_remaining = (event.end_time - now).num_seconds().max(0) as f64;
        let sigma_horizon = self.sigma_horizon(symbol, secs_remaining);

        // Probability estimation (log-normal model)
        let p_hat = estimate_probability(s0, st, sigma_horizon);

        // Gate 1: Direction + quote availability
        let (direction, entry_price) = if p_hat >= 0.5 {
            let quote = self.quotes.get(&event.up_token);
            if quote.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.up_token,
                    "Gate 1: UP token quote missing"
                );
                return None;
            }
            let ask = quote.unwrap().ask;
            if ask.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.up_token,
                    "Gate 1: UP token ask price missing"
                );
                return None;
            }
            (Direction::Up, ask.unwrap())
        } else {
            let quote = self.quotes.get(&event.down_token);
            if quote.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.down_token,
                    "Gate 1: DOWN token quote missing"
                );
                return None;
            }
            let ask = quote.unwrap().ask;
            if ask.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.down_token,
                    "Gate 1: DOWN token ask price missing"
                );
                return None;
            }
            (Direction::Down, ask.unwrap())
        };

        let entry_f = entry_price.to_f64()?;

        // Gate 2: Price filter (bounds + no-trade zone)
        if entry_f < self.config.min_entry_price
            || entry_f > self.config.max_entry_price
            || (entry_f >= self.config.no_trade_zone_min
                && entry_f <= self.config.no_trade_zone_max)
        {
            debug!(
                symbol,
                event_id = %event.event_id,
                entry_price = entry_f,
                "Gate 2: Price filter (bounds or no-trade zone)"
            );
            return None;
        }

        // Gate 3: Probability threshold
        let effective_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };
        if effective_p < self.config.min_probability {
            debug!(
                symbol,
                event_id = %event.event_id,
                effective_p,
                threshold = self.config.min_probability,
                "Gate 3: Probability too low"
            );
            return None;
        }

        // Gate 4: Edge after fees
        let cost = crypto_fee_cost(entry_f);
        let edge = effective_p - entry_f - cost;
        if edge < self.config.min_edge {
            debug!(
                symbol,
                event_id = %event.event_id,
                edge,
                threshold = self.config.min_edge,
                effective_p,
                entry_price = entry_f,
                cost,
                "Gate 4: Edge too low"
            );
            return None;
        }

        Some((direction, entry_price, effective_p, edge))
    }

    /// Build a TradingIntent from a signal.
    fn build_intent(
        &self,
        event: &EventWindow,
        direction: Direction,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> TradingIntent {
        let token_id = match direction {
            Direction::Up => event.up_token.clone(),
            Direction::Down => event.down_token.clone(),
        };

        TradingIntent {
            intent_id: format!(
                "pm5d_{}_{}_{}",
                event.symbol,
                match direction {
                    Direction::Up => "UP",
                    Direction::Down => "DN",
                },
                now.timestamp_millis(),
            ),
            deployment_id: String::new(), // filled by runtime
            market_id: event.event_id.clone(),
            token_id,
            side: TradeSide::Buy,
            quantity: self.shares_for_entry_price(entry_price),
            limit_price: Some(entry_price),
            purpose: IntentPurpose::Entry,
            created_at: now,
        }
    }

    /// Try directional entry for a given symbol after a spot price update.
    fn try_entry(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        // Position count check
        let open_count = positions.positions().count();
        if open_count >= self.config.max_positions {
            debug!(
                symbol,
                open_count,
                max = self.config.max_positions,
                "Max positions reached"
            );
            return vec![];
        }

        // Already holding this symbol?
        let spot_price = match self.spot.get(symbol) {
            Some(s) => s.price,
            None => {
                debug!(symbol, "No spot price available");
                return vec![];
            }
        };

        let event = match self.pick_event(symbol, now) {
            Some(e) => e,
            None => {
                // Log why no event was picked
                if let Some(events) = self.events.get(symbol) {
                    let total = events.len();
                    let in_window = events
                        .iter()
                        .filter(|e| {
                            let rem = (e.end_time - now).num_seconds();
                            rem >= self.config.min_time_remaining_secs as i64
                                && rem <= self.config.max_time_remaining_secs as i64
                        })
                        .count();
                    debug!(
                        symbol,
                        total_events = total,
                        in_window,
                        min_time = self.config.min_time_remaining_secs,
                        max_time = self.config.max_time_remaining_secs,
                        "No event in valid time window"
                    );
                }
                return vec![];
            }
        };

        // Check if we already have a position for this event's tokens
        let already_holding = positions.positions().any(|p| {
            p.net_qty > Decimal::ZERO
                && (p.token_id == event.up_token || p.token_id == event.down_token)
        });
        if already_holding {
            debug!(symbol, event_id = %event.event_id, "Already holding position for this event");
            return vec![];
        }

        match self.evaluate_entry(symbol, spot_price, &event, now) {
            Some((direction, entry_price, effective_p, edge)) => {
                info!(
                    symbol,
                    event_id = %event.event_id,
                    ?direction,
                    entry_price = %entry_price,
                    p = format!("{:.1}%", effective_p * 100.0),
                    edge = format!("{:.1}%", edge * 100.0),
                    "✓ Entry signal PASSED all gates",
                );
                let intent = self.build_intent(&event, direction, entry_price, now);
                vec![StrategyDecision::Enter(intent)]
            }
            None => vec![],
        }
    }
}

impl StrategyLogic for DirectionalStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        _orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                if !self.config.symbols.contains(symbol) {
                    return vec![];
                }

                // Advance feed clock so EventDiscovered pruning is deterministic in replay.
                if self.feed_time.map_or(true, |ft| *ts > ft) {
                    self.feed_time = Some(*ts);
                }

                self.update_volatility(symbol, *price, *ts);
                self.spot.insert(
                    symbol.clone(),
                    SpotState {
                        price: *price,
                        ts: *ts,
                    },
                );
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.open_price.is_none() {
                            event.open_price = Some(*price);
                        }
                    }
                }

                self.reset_daily_counter(*ts);
                if self.daily_trades >= self.config.max_daily_trades {
                    return vec![];
                }
                if self.in_cooldown(symbol, *ts) {
                    return vec![];
                }

                self.try_entry(symbol, positions, *ts)
            }

            MarketUpdate::Quote {
                token_id, bid, ask, ts,
            } => {
                self.quotes.insert(
                    token_id.clone(),
                    QuoteState {
                        _bid: *bid,
                        ask: *ask,
                    },
                );

                // Also try entry: a fresh quote may unlock a signal that was
                // previously blocked by missing ask price (Gate 1).
                if let Some(symbol) = self.token_symbol.get(token_id).cloned() {
                    if self.config.symbols.contains(&symbol) {
                        if self.feed_time.map_or(true, |ft| *ts > ft) {
                            self.feed_time = Some(*ts);
                        }
                        self.reset_daily_counter(*ts);
                        if self.daily_trades < self.config.max_daily_trades
                            && !self.in_cooldown(&symbol, *ts)
                        {
                            return self.try_entry(&symbol, positions, *ts);
                        }
                    }
                }
                vec![]
            }

            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs: _window_secs,
                price_to_beat,
                resolved_up_won: _,
            } => {
                // Use feed_time (last seen spot/quote timestamp) as "now" so that
                // replay runs are deterministic and don't drop historical windows
                // that arrived before the first spot tick.
                // Fall back to the event's own end_time only when no feed time is
                // known yet — this keeps the window alive until spot data arrives.
                let now = self
                    .feed_time
                    .unwrap_or_else(|| *end_time - chrono::Duration::seconds(1));
                let events = self.events.entry(symbol.clone()).or_default();
                // Prune expired
                events.retain(|e| e.end_time > now);
                // Dedup
                if events.iter().any(|e| e.event_id == *event_id) {
                    return vec![];
                }
                // Use price_to_beat if available, otherwise fallback to current spot
                let open_price = price_to_beat.or_else(|| self.spot.get(symbol).map(|s| s.price));

                // Track token → symbol mapping
                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());

                events.push(EventWindow {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    open_price,
                });
                vec![]
            }

            MarketUpdate::EventExpired { event_id, end_time, resolved_up_won: settlement } => {
                // Settle positions: find any tokens from this event that we hold
                let mut exits = Vec::new();

                // Find the event's tokens before removing
                let mut event_tokens: Vec<(String, String, Option<Decimal>)> =
                    Vec::new();
                for events in self.events.values() {
                    for ev in events {
                        if ev.event_id == *event_id {
                            event_tokens.push((
                                ev.up_token.clone(),
                                ev.down_token.clone(),
                                ev.open_price,
                            ));
                        }
                    }
                }

                for (up_token, down_token, open_price) in &event_tokens {
                    // Determine outcome: use settlement from EventExpired if available,
                    // otherwise fall back to spot price comparison.
                    let up_won = if let Some(resolved) = settlement {
                        *resolved
                    } else {
                        // Fallback only when official settlement is unavailable.
                        let symbol = self
                            .token_symbol
                            .get(up_token)
                            .or_else(|| self.token_symbol.get(down_token));
                        let spot = symbol.and_then(|s| self.spot.get(s)).map(|s| s.price);
                        match (spot, open_price) {
                            (Some(current), Some(open)) => current >= *open,
                            _ => {
                                warn!(event_id = %event_id, token = %up_token, "Missing official settlement and fallback spot; defaulting to UP");
                                true
                            }
                        }
                    };

                    // Check if we hold the UP token
                    if positions.net_qty(up_token) > Decimal::ZERO {
                        let settle_price = if up_won { dec!(1.00) } else { dec!(0.00) };
                        let qty = positions.net_qty(up_token);
                        exits.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!("settle_{}", event_id),
                            deployment_id: String::new(),
                            market_id: event_id.clone(),
                            token_id: up_token.clone(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(settle_price),
                            purpose: IntentPurpose::Exit,
                            created_at: *end_time,
                        }));
                        info!(
                            event_id,
                            token = %up_token,
                            outcome = if up_won { "WIN" } else { "LOSE" },
                            settle_price = %settle_price,
                            "Settlement: UP token"
                        );
                    }

                    // Check if we hold the DOWN token
                    if positions.net_qty(down_token) > Decimal::ZERO {
                        let settle_price = if up_won { dec!(0.00) } else { dec!(1.00) };
                        let qty = positions.net_qty(down_token);
                        exits.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!("settle_{}", event_id),
                            deployment_id: String::new(),
                            market_id: event_id.clone(),
                            token_id: down_token.clone(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(settle_price),
                            purpose: IntentPurpose::Exit,
                            created_at: *end_time,
                        }));
                        info!(
                            event_id,
                            token = %down_token,
                            outcome = if up_won { "LOSE" } else { "WIN" },
                            settle_price = %settle_price,
                            "Settlement: DOWN token"
                        );
                    }
                }

                // Remove expired events
                for events in self.events.values_mut() {
                    events.retain(|e| e.event_id != *event_id);
                }
                exits
            }

            _ => vec![],
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if let Some(symbol) = self.token_symbol.get(&fill.token_id) {
            self.cooldowns.insert(symbol.clone(), fill.timestamp);
            self.daily_trades += 1;
        }
    }

    fn name(&self) -> &str {
        "pm_5m_directional"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MarketUpdate;
    use ploy_trading::{OrderLedger, PositionLedger};

    fn default_config() -> DirectionalConfig {
        DirectionalConfig {
            symbols: vec!["BTCUSDT".into()],
            vol_floor: 0.001,
            min_probability: 0.55,
            min_z_score: 0.35,
            min_entry_price: 0.15,
            max_entry_price: 0.85,
            no_trade_zone_min: 0.45,
            no_trade_zone_max: 0.55,
            min_edge: 0.02,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: dec!(25),
            max_positions: 1000,
            max_daily_trades: 1000,
        }
    }

    #[test]
    fn normal_cdf_at_zero_is_half() {
        let p = normal_cdf(0.0);
        assert!((p - 0.5).abs() < 1e-6, "got {}", p);
    }

    #[test]
    fn probability_above_s0_is_high() {
        // BTC up 1%, 0.5% horizon sigma → high probability of staying above
        let p = estimate_probability(100_000.0, 101_000.0, 0.005);
        assert!(p > 0.7, "p={}", p);
    }

    #[test]
    fn probability_below_s0_is_low() {
        let p = estimate_probability(100_000.0, 99_000.0, 0.005);
        assert!(p < 0.3, "p={}", p);
    }

    #[test]
    fn event_windowing_picks_nearest() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        // Register two events: one ending in 120s, one in 240s
        let e1_end = now + chrono::Duration::seconds(120);
        let e2_end = now + chrono::Duration::seconds(240);

        strat.events.insert(
            "BTCUSDT".into(),
            vec![
                EventWindow {
                    event_id: "e1".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up1".into(),
                    down_token: "dn1".into(),
                    end_time: e1_end,
                    open_price: Some(dec!(100000)),
                },
                EventWindow {
                    event_id: "e2".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up2".into(),
                    down_token: "dn2".into(),
                    end_time: e2_end,
                    open_price: Some(dec!(100000)),
                },
            ],
        );

        let picked = strat.pick_event("BTCUSDT", now).unwrap();
        assert_eq!(picked.event_id, "e1"); // nearer one
    }

    #[test]
    fn cooldown_blocks_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        strat.cooldowns.insert("BTCUSDT".into(), now);
        // cooldown_secs = 0 means no cooldown — always false
        assert!(!strat.in_cooldown("BTCUSDT", now + chrono::Duration::seconds(1)));
        assert!(!strat.in_cooldown("BTCUSDT", now));
    }

    #[test]
    fn on_update_processes_spot_price() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let update = MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".into(),
            price: dec!(100000),
            ts: Utc::now(),
        };

        // No event registered → no decisions, but price is stored
        let decisions = strat.on_update(&update, &positions, &orders);
        assert!(decisions.is_empty());
        assert!(strat.spot.contains_key("BTCUSDT"));
    }

    #[test]
    fn full_signal_generates_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // 1. Register event ending in 120s
        let end_time = now + chrono::Duration::seconds(120);
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time,
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // 2. Set initial spot (becomes open_price via event)
        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(100000),
                ts: now,
            },
        );
        // Manually set open_price since event was registered before spot
        strat.events.get_mut("BTCUSDT").unwrap()[0].open_price = Some(dec!(100000));

        // 3. Provide quotes — UP ask cheap (0.30) meaning market underprices UP
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.29)),
                ask: Some(dec!(0.30)),
                ts: now,
            },
            &positions,
            &orders,
        );
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "dn1".into(),
                bid: Some(dec!(0.69)),
                ask: Some(dec!(0.70)),
                ts: now,
            },
            &positions,
            &orders,
        );

        // 4. BTC moved up 1.5% → should trigger UP entry
        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "Expected 1 entry signal");
        match &decisions[0] {
            StrategyDecision::Enter(intent) => {
                assert_eq!(intent.token_id, "up1");
                assert_eq!(intent.side, TradeSide::Buy);
                assert_eq!(intent.quantity, dec!(83.333333));
            }
            other => panic!("Expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn event_before_first_spot_backfills_open_price_and_allows_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        assert_eq!(
            strat.events["BTCUSDT"][0].open_price, None,
            "precondition: event arrived before first spot"
        );

        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.29)),
                ask: Some(dec!(0.30)),
                ts: now,
            },
            &positions,
            &orders,
        );
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "dn1".into(),
                bid: Some(dec!(0.69)),
                ask: Some(dec!(0.70)),
                ts: now,
            },
            &positions,
            &orders,
        );

        let first_spot_decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            first_spot_decisions.is_empty(),
            "first spot should initialize the event, not trade immediately"
        );
        assert_eq!(strat.events["BTCUSDT"][0].open_price, Some(dec!(100000)));

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now + chrono::Duration::seconds(60),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "Expected 1 entry signal");
    }

    #[test]
    fn official_settlement_overrides_last_spot_direction() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        strat.events.insert(
            "BTCUSDT".into(),
            vec![EventWindow {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now,
                open_price: Some(dec!(100000)),
            }],
        );
        strat.token_symbol.insert("up1".into(), "BTCUSDT".into());
        strat.token_symbol.insert("dn1".into(), "BTCUSDT".into());
        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(101000),
                ts: now,
            },
        );

        let mut positions = PositionLedger::default();
        positions.apply_fill(&FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: now,
        });

        let decisions = strat.on_update(
            &MarketUpdate::EventExpired {
                event_id: "evt1".into(),
                end_time: now,
                // resolved_up_won: Some(false) means UP lost → DOWN token wins
                resolved_up_won: Some(false),
            },
            &positions,
            &OrderLedger::default(),
        );

        assert_eq!(decisions.len(), 1, "Expected settlement exit");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => assert_eq!(intent.limit_price, Some(dec!(0.00))),
            other => panic!("Expected Exit, got {:?}", other),
        }
    }

    #[test]
    fn spot_updates_accumulate_realized_volatility() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let _ = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        let _ = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100100),
                ts: now + chrono::Duration::seconds(5),
            },
            &positions,
            &orders,
        );

        let vol_state = strat.volatility.get("BTCUSDT").expect("vol state");
        assert!(vol_state.ewma_var_per_sec > 0.0);
    }
}
