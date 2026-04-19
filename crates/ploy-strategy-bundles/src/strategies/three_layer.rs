//! Three-layer strategy config and regime classification.

use chrono::{DateTime, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, OrderState, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::info;

use crate::strategies::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};
use ploy_operator_contracts::Regime;

/// Configuration for the three-layer directional strategy.
#[derive(Debug, Clone)]
pub struct ThreeLayerConfig {
    pub symbols: Vec<String>,
    pub min_direction_prob: f64,
    pub min_distance_over_sigma: f64,
    pub min_confirmation_score: f64,
    pub min_drift_confirmation: f64,
    pub min_edge: f64,
    pub min_reward_risk: f64,
    pub take_profit_ask: f64,
    pub stop_distance_pct: f64,
    pub max_pm_lag_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub cooldown_secs: u64,
    pub stake_usd: Decimal,
    pub max_positions: usize,
    pub max_daily_trades: u32,
    pub allowed_window_secs: Vec<u64>,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
}

impl From<DirectionalConfig> for ThreeLayerConfig {
    fn from(c: DirectionalConfig) -> Self {
        Self {
            symbols: c.symbols,
            min_direction_prob: c.three_layer_min_direction_prob,
            min_distance_over_sigma: c.three_layer_min_distance_over_sigma,
            min_confirmation_score: c.three_layer_min_confirmation_score,
            min_drift_confirmation: c.three_layer_min_drift_confirmation,
            min_edge: c.three_layer_min_edge,
            min_reward_risk: c.three_layer_min_reward_risk,
            take_profit_ask: c.three_layer_take_profit_ask,
            stop_distance_pct: c.three_layer_stop_distance_pct,
            max_pm_lag_secs: c.three_layer_max_pm_lag_secs,
            min_time_remaining_secs: c.min_time_remaining_secs,
            max_time_remaining_secs: c.max_time_remaining_secs,
            cooldown_secs: c.cooldown_secs,
            stake_usd: c.stake_usd,
            max_positions: c.max_positions,
            max_daily_trades: c.max_daily_trades,
            allowed_window_secs: c.allowed_window_secs,
            min_entry_price: c.min_entry_price,
            max_entry_price: c.max_entry_price,
        }
    }
}

const DRIFT_WINDOW_SECS: f64 = 30.0;
const VOL_WINDOW_SECS: f64 = 120.0;
const MIN_VOL_POINTS: usize = 5;
const TIME_STOP_SECS: i64 = 3;

struct DriftTracker {
    history: VecDeque<(DateTime<Utc>, f64)>,
}

impl DriftTracker {
    fn new() -> Self {
        Self { history: VecDeque::new() }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        if price <= 0.0 { return; }
        self.history.push_back((ts, price.ln()));
        while self.history.len() > 1 {
            let oldest = self.history.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > VOL_WINDOW_SECS {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    fn drift_30s(&self) -> f64 {
        if self.history.len() < 2 { return 0.0; }
        let now = self.history.back().unwrap();
        let cutoff = now.0 - chrono::Duration::seconds(30);
        let anchor = self.history.iter()
            .find(|(ts, _)| *ts >= cutoff)
            .unwrap_or(self.history.front().unwrap());
        now.1 - anchor.1
    }

    fn sigma_horizon(&mut self, horizon_secs: f64) -> f64 {
        if self.history.len() < MIN_VOL_POINTS { return 0.0; }
        let contiguous = self.history.make_contiguous();
        let returns: Vec<f64> = contiguous.windows(2)
            .map(|w| w[1].1 - w[0].1)
            .collect();
        if returns.is_empty() { return 0.0; }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / returns.len() as f64;
        let avg_dt = {
            let total_secs = (self.history.back().unwrap().0
                - self.history.front().unwrap().0)
                .num_milliseconds() as f64 / 1000.0;
            total_secs / returns.len() as f64
        };
        if avg_dt <= 0.0 { return 0.0; }
        let var_per_sec = var / avg_dt;
        (var_per_sec * horizon_secs).sqrt()
    }
}

struct MpriceDriftAccumulator {
    entries: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: f64,
}

impl MpriceDriftAccumulator {
    fn new(window_secs: f64) -> Self {
        Self { entries: VecDeque::new(), window_secs }
    }

    fn push(&mut self, ts: DateTime<Utc>, microprice_offset_bps: f64) {
        self.entries.push_back((ts, microprice_offset_bps));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn cum_drift(&self) -> f64 {
        self.entries.iter().map(|e| e.1).sum()
    }
}

#[derive(Clone, Copy, Default)]
struct LobState {
    obi: f64,
    obi_prev: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
    signed_trade_imbalance: f64,
    last_aggtrade_ts: Option<DateTime<Utc>>,
    ts: Option<DateTime<Utc>>,
}

impl LobState {
    fn depth_imbalance(&self) -> f64 {
        let total = self.bid_depth_near + self.ask_depth_near;
        if total <= 0.0 { return 0.0; }
        (self.bid_depth_near - self.ask_depth_near) / total
    }

    fn obi_delta(&self) -> f64 {
        self.obi - self.obi_prev
    }

    fn apply_l2(&mut self, obi: f64, spread_bps: u32, bid_near: f64, ask_near: f64, ts: DateTime<Utc>) {
        self.obi_prev = self.obi;
        self.obi = obi;
        self.spread_bps = spread_bps;
        self.bid_depth_near = bid_near;
        self.ask_depth_near = ask_near;
        self.ts = Some(ts);
    }

    fn apply_aggtrade(&mut self, quantity: f64, is_buyer_maker: bool, ts: DateTime<Utc>) {
        let seconds = self.last_aggtrade_ts
            .map(|last| (ts - last).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0);
        let decay = if seconds > 0.0 { (-seconds / 30.0).exp() } else { 1.0 };
        let signed_qty = if is_buyer_maker { -quantity } else { quantity };
        self.signed_trade_imbalance = self.signed_trade_imbalance * decay + signed_qty;
        self.last_aggtrade_ts = Some(ts);
    }

    fn confirmation_score(&self) -> f64 {
        let trade_score = (self.signed_trade_imbalance / 50.0).clamp(-1.0, 1.0) * 0.30;
        let obi_score = self.obi.clamp(-1.0, 1.0) * 0.25;
        let obi_delta_score = self.obi_delta().clamp(-1.0, 1.0) * 0.25;
        let depth_score = self.depth_imbalance().clamp(-1.0, 1.0) * 0.20;
        trade_score + obi_score + obi_delta_score + depth_score
    }
}

#[derive(Clone, Copy)]
struct QuoteState {
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    ts: DateTime<Utc>,
}

#[derive(Clone)]
struct EventWindow {
    event_id: Arc<str>,
    symbol: Arc<str>,
    up_token: Arc<str>,
    down_token: Arc<str>,
    end_time: DateTime<Utc>,
    window_secs: u64,
    price_to_beat: Option<Decimal>,
}

fn crypto_fee_cost(ask: f64) -> f64 {
    0.02 * ask * (1.0 - ask)
}

// ── Gate Functions ──────────────────────────────────────────────────

/// Normal CDF approximation (Abramowitz & Stegun).
fn norm_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327 * (-x * x / 2.0).exp();
    let p = d * t * (0.3193815 + t * (-0.3565638 + t * (1.781478 + t * (-1.821256 + t * 1.330274))));
    if x >= 0.0 { 1.0 - p } else { p }
}

/// Gate 1: Direction.
/// Returns Some((direction_sign, effective_probability)) or None.
///
/// - `distance_over_sigma`: (spot - price_to_beat) / (sigma * price_to_beat)
/// - In Early regime: direction driven by distance_over_sigma + model_prob.
/// - In Middle regime: cum_mprice_drift_5m co-drives direction.
/// - In Late/Expiry: drift_30s becomes primary directional signal.
fn evaluate_direction(
    distance_over_sigma: f64,
    _sigma_horizon: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64)> {
    if distance_over_sigma.abs() < config.min_distance_over_sigma
        && regime == Regime::Early
    {
        return None;
    }

    let model_prob_up = norm_cdf(distance_over_sigma);

    let direction_prob = match regime {
        Regime::Early => {
            model_prob_up
        }
        Regime::Middle => {
            let lob_nudge = (cum_mprice_drift_5m / 100.0).clamp(-0.08, 0.08);
            (model_prob_up + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Late => {
            let drift_nudge = (drift_30s * 500.0).clamp(-0.12, 0.12);
            let lob_nudge = (cum_mprice_drift_5m / 80.0).clamp(-0.06, 0.06);
            (model_prob_up + drift_nudge + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Expiry => {
            let drift_nudge = (drift_30s * 800.0).clamp(-0.15, 0.15);
            (model_prob_up + drift_nudge).clamp(0.01, 0.99)
        }
    };

    let (direction_sign, effective_p) = if direction_prob >= 0.5 {
        (1.0_f64, direction_prob)
    } else {
        (-1.0_f64, 1.0 - direction_prob)
    };

    if effective_p < config.min_direction_prob {
        return None;
    }

    Some((direction_sign, effective_p))
}

/// Gate 2: Confirmation.
/// Returns true if LOB microstructure agrees with the chosen direction.
fn evaluate_confirmation(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> bool {
    let raw_score = lob.confirmation_score() + (cum_mprice_drift_5m / 200.0).clamp(-0.15, 0.15);
    let aligned_score = direction_sign * raw_score;

    let threshold = match regime {
        Regime::Early  => config.min_confirmation_score * 0.5,
        Regime::Middle => config.min_confirmation_score,
        Regime::Late   => config.min_confirmation_score * 1.5,
        Regime::Expiry => config.min_confirmation_score * 2.0,
    };

    if aligned_score < threshold {
        return false;
    }

    if matches!(regime, Regime::Late | Regime::Expiry) {
        let drift_agrees = (direction_sign > 0.0 && drift_30s > config.min_drift_confirmation)
            || (direction_sign < 0.0 && drift_30s < -config.min_drift_confirmation);
        if !drift_agrees {
            return false;
        }
    }

    true
}

/// Gate 3: Worth-It.
/// Returns Some((entry_price, edge, reward_risk)) or None.
fn evaluate_worth_it(
    effective_p: f64,
    ask: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64)> {
    if ask < config.min_entry_price || ask > config.max_entry_price {
        return None;
    }

    let fee = crypto_fee_cost(ask);
    let edge = effective_p - ask - fee;

    let min_edge = match regime {
        Regime::Early  => config.min_edge,
        Regime::Middle => config.min_edge,
        Regime::Late   => config.min_edge * 1.2,
        Regime::Expiry => config.min_edge * 1.5,
    };

    if edge < min_edge {
        return None;
    }

    let reward = 1.0 - ask - fee;
    let risk = ask + fee;
    let rr = if risk > 0.0 { reward / risk } else { 0.0 };

    if rr < config.min_reward_risk {
        return None;
    }

    Some((ask, edge, rr))
}

// ── ThreeLayerStrategy ─────────────────────────────────────────────

pub struct ThreeLayerStrategy {
    config: ThreeLayerConfig,
    drift: HashMap<Arc<str>, DriftTracker>,
    lob: HashMap<Arc<str>, LobState>,
    mprice_acc: HashMap<Arc<str>, MpriceDriftAccumulator>,
    spot: HashMap<Arc<str>, Decimal>,
    quotes: HashMap<Arc<str>, QuoteState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
    feed_time: Option<DateTime<Utc>>,
}

impl ThreeLayerStrategy {
    pub fn new(config: ThreeLayerConfig) -> Self {
        Self {
            config,
            drift: HashMap::new(),
            lob: HashMap::new(),
            mprice_acc: HashMap::new(),
            spot: HashMap::new(),
            quotes: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            events: HashMap::new(),
            last_entry: HashMap::new(),
            daily_trade_count: 0,
            daily_trade_date: None,
            feed_time: None,
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.feed_time.unwrap_or_else(Utc::now)
    }

    fn reset_daily_counter(&mut self, ts: DateTime<Utc>) {
        let today = ts.date_naive();
        if self.daily_trade_date != Some(today) {
            self.daily_trade_count = 0;
            self.daily_trade_date = Some(today);
        }
    }

    fn window_allowed(&self, window_secs: u64) -> bool {
        self.config.allowed_window_secs.is_empty()
            || self.config.allowed_window_secs.contains(&window_secs)
    }

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events
            .get(symbol)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| {
                        self.window_allowed(e.window_secs)
                            && e.end_time > now
                            && {
                                let remaining = (e.end_time - now).num_seconds();
                                remaining >= self.config.min_time_remaining_secs as i64
                                    && remaining <= self.config.max_time_remaining_secs as i64
                            }
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn try_entry(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        if self.daily_trade_count >= self.config.max_daily_trades {
            return None;
        }
        if positions.positions().count() >= self.config.max_positions {
            return None;
        }

        let spot_price = (*self.spot.get(symbol)?).to_f64()?;
        if spot_price <= 0.0 {
            return None;
        }

        // Cooldown check
        if let Some(last) = self.last_entry.get(symbol) {
            if (now - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        let candidates = self.candidate_events(symbol, now);
        for event in &candidates {
            let price_to_beat = event.price_to_beat?.to_f64()?;
            if price_to_beat <= 0.0 {
                continue;
            }

            let time_remaining = (event.end_time - now).num_seconds();
            let regime = Regime::from_secs(time_remaining);

            let drift_tracker = self.drift.get_mut(symbol)?;
            let sigma_h = drift_tracker.sigma_horizon(time_remaining as f64);
            let drift_30s = drift_tracker.drift_30s();

            let distance_over_sigma = if sigma_h > 0.0 {
                (spot_price - price_to_beat) / (sigma_h * price_to_beat)
            } else {
                0.0
            };

            let cum_mprice_drift_5m = self
                .mprice_acc
                .get(symbol)
                .map(|acc| acc.cum_drift())
                .unwrap_or(0.0);

            // Gate 1: Direction
            let (direction_sign, effective_p) = evaluate_direction(
                distance_over_sigma,
                sigma_h,
                cum_mprice_drift_5m,
                drift_30s,
                regime,
                &self.config,
            )?;

            let betting_up = direction_sign > 0.0;
            let (token_id, direction) = if betting_up {
                (&event.up_token, "UP")
            } else {
                (&event.down_token, "DOWN")
            };

            // Skip if already positioned or have active order on this token
            if positions.net_qty(token_id) > Decimal::ZERO {
                continue;
            }
            if orders.orders().any(|o| {
                o.token_id.as_str() == &**token_id
                    && matches!(
                        o.state,
                        OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
                    )
            }) {
                continue;
            }

            // Check quote freshness
            let quote = self.quotes.get(token_id)?;
            let ask = quote.ask?.to_f64()?;
            let quote_age = (now - quote.ts).num_seconds();
            if quote_age > self.config.max_pm_lag_secs as i64 {
                continue;
            }

            // Gate 2: Confirmation
            let lob = self.lob.get(symbol).copied().unwrap_or_default();
            if !evaluate_confirmation(
                direction_sign,
                &lob,
                cum_mprice_drift_5m,
                drift_30s,
                regime,
                &self.config,
            ) {
                continue;
            }

            // Gate 3: Worth-It
            let (entry_price_f, edge, _rr) =
                evaluate_worth_it(effective_p, ask, regime, &self.config)?;

            let entry_price = Decimal::try_from(entry_price_f).ok()?;
            let quantity = self.entry_quantity(entry_price);
            if quantity <= Decimal::ZERO {
                continue;
            }

            let intent_id = format!(
                "tl_{}_{}_{}_{}",
                symbol.to_lowercase(),
                direction.to_lowercase(),
                event.event_id,
                now.timestamp_millis()
            );

            let intent = TradingIntent {
                intent_id: intent_id.clone(),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: token_id.to_string(),
                side: TradeSide::Buy,
                quantity,
                limit_price: Some(entry_price),
                purpose: IntentPurpose::Entry,
                created_at: now,
            };

            let signal = SignalRecord {
                strategy: self.name().to_string(),
                event_id: Some(event.event_id.to_string()),
                token_id: Some(token_id.to_string()),
                intent_id: Some(intent_id),
                symbol: symbol.to_string(),
                direction: direction.to_string(),
                p_hat: effective_p,
                edge,
                entry_price,
                decision: "enter".to_string(),
                ts: now,
            };

            info!(
                strategy = "three_layer",
                symbol = %symbol,
                direction,
                p_hat = effective_p,
                edge,
                entry_price = %entry_price,
                regime = regime.as_str(),
                "entry signal"
            );

            return Some(StrategyDecision::Enter {
                intent,
                signal: Some(signal),
            });
        }
        None
    }

    fn exit_decisions_for_symbol(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        spot: Option<f64>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            let time_remaining = (event.end_time - now).num_seconds();

            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }

                // Time stop
                if time_remaining < TIME_STOP_SECS {
                    decisions.push(StrategyDecision::Exit(TradingIntent {
                        intent_id: format!(
                            "tl_time_exit_{}_{}",
                            token_id,
                            now.timestamp_millis()
                        ),
                        deployment_id: String::new(),
                        market_id: event.event_id.to_string(),
                        token_id: token_id.to_string(),
                        side: TradeSide::Sell,
                        quantity: qty,
                        limit_price: None,
                        purpose: IntentPurpose::Exit,
                        created_at: now,
                    }));
                    continue;
                }

                // Take profit
                if let Some(quote) = self.quotes.get(token_id) {
                    if let Some(ask) = quote.ask.and_then(|v| v.to_f64()) {
                        if ask >= self.config.take_profit_ask {
                            decisions.push(StrategyDecision::Exit(TradingIntent {
                                intent_id: format!(
                                    "tl_tp_{}_{}",
                                    token_id,
                                    now.timestamp_millis()
                                ),
                                deployment_id: String::new(),
                                market_id: event.event_id.to_string(),
                                token_id: token_id.to_string(),
                                side: TradeSide::Sell,
                                quantity: qty,
                                limit_price: quote.bid,
                                purpose: IntentPurpose::Exit,
                                created_at: now,
                            }));
                            continue;
                        }
                    }
                }

                // Stop loss
                if let (Some(price_to_beat), Some(spot_price)) =
                    (event.price_to_beat.and_then(|v| v.to_f64()), spot)
                {
                    let dist = (spot_price - price_to_beat) / price_to_beat;
                    let wrong_direction = if is_up {
                        dist < -self.config.stop_distance_pct
                    } else {
                        dist > self.config.stop_distance_pct
                    };
                    if wrong_direction {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!(
                                "tl_sl_{}_{}",
                                token_id,
                                now.timestamp_millis()
                            ),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: None,
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                    }
                }
            }
        }

        decisions
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        up_won: bool,
        now: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        if positions.net_qty(&event.up_token) > Decimal::ZERO {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("tl_settle_{}_up", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.up_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.up_token),
                limit_price: Some(if up_won {
                    Decimal::new(1, 0)
                } else {
                    Decimal::ZERO
                }),
                purpose: IntentPurpose::Exit,
                created_at: now,
            }));
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("tl_settle_{}_down", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.down_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.down_token),
                limit_price: Some(if up_won {
                    Decimal::ZERO
                } else {
                    Decimal::new(1, 0)
                }),
                purpose: IntentPurpose::Exit,
                created_at: now,
            }));
        }

        exits
    }

    fn resolve_up_won(&self, event: &EventWindow, resolved: Option<bool>) -> Option<bool> {
        if resolved.is_some() {
            return resolved;
        }
        let price_to_beat = event.price_to_beat?.to_f64()?;
        let spot = (*self.spot.get(&event.symbol)?).to_f64()?;
        Some(spot >= price_to_beat)
    }
}

// ── StrategyLogic impl ────────────────────────────────────────────

impl StrategyLogic for ThreeLayerStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                if !self.config.symbols.iter().any(|s| s.as_str() == symbol.as_ref()) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.reset_daily_counter(*ts);

                let price_f64 = match price.to_f64() {
                    Some(p) if p > 0.0 => p,
                    _ => return Vec::new(),
                };

                self.drift
                    .entry(symbol.clone())
                    .or_insert_with(DriftTracker::new)
                    .push(*ts, price_f64);
                self.spot.insert(symbol.clone(), *price);

                // Set price_to_beat for events that don't have one yet
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.price_to_beat.is_none() {
                            event.price_to_beat = Some(*price);
                        }
                    }
                }

                // Check exits first
                let spot_opt = Some(price_f64);
                let mut decisions =
                    self.exit_decisions_for_symbol(symbol, *ts, spot_opt, positions);

                // Cooldown check before entry
                if let Some(last) = self.last_entry.get(symbol) {
                    if (*ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                        return decisions;
                    }
                }

                if let Some(entry) = self.try_entry(symbol, *ts, positions, orders) {
                    decisions.push(entry);
                }
                decisions
            }

            MarketUpdate::Quote {
                token_id,
                bid,
                ask,
                ts,
                ..
            } => {
                self.quotes.insert(
                    token_id.clone(),
                    QuoteState {
                        bid: *bid,
                        ask: *ask,
                        ts: *ts,
                    },
                );

                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                let spot = self.spot.get(&symbol).and_then(|p| p.to_f64());
                self.exit_decisions_for_symbol(&symbol, *ts, spot, positions)
            }

            MarketUpdate::AggTrade {
                symbol,
                quantity,
                is_buyer_maker,
                ts,
                ..
            } => {
                if !self.config.symbols.iter().any(|s| s.as_str() == symbol.as_ref()) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                let qty_f64 = quantity.to_f64().unwrap_or(0.0);
                self.lob
                    .entry(symbol.clone())
                    .or_default()
                    .apply_aggtrade(qty_f64, *is_buyer_maker, *ts);
                Vec::new()
            }

            MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ts,
            } => {
                if !self.config.symbols.iter().any(|s| s.as_str() == symbol.as_ref()) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.lob
                    .entry(symbol.clone())
                    .or_default()
                    .apply_l2(*obi, *spread_bps, *bid_depth_near, *ask_depth_near, *ts);

                let mid = (bid_depth_near + ask_depth_near) / 2.0;
                if mid > 0.0 {
                    let microprice_offset = (bid_depth_near - ask_depth_near) / mid;
                    self.mprice_acc
                        .entry(symbol.clone())
                        .or_insert_with(|| MpriceDriftAccumulator::new(300.0))
                        .push(*ts, microprice_offset);
                }
                Vec::new()
            }

            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts,
            } => {
                if let Some(lob) = self.lob.get_mut(symbol) {
                    lob.obi_prev = lob.obi;
                    lob.obi = *obi;
                    lob.spread_bps = *spread_bps;
                    lob.ts = Some(*ts);
                }
                Vec::new()
            }

            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                price_to_beat,
                ..
            } => {
                if !self.config.symbols.iter().any(|s| s.as_str() == symbol.as_ref())
                    || !self.window_allowed(*window_secs)
                {
                    return Vec::new();
                }

                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol
                    .insert(down_token.clone(), symbol.clone());
                self.token_event
                    .insert(up_token.clone(), event_id.clone());
                self.token_event
                    .insert(down_token.clone(), event_id.clone());

                self.events
                    .entry(symbol.clone())
                    .or_default()
                    .push(EventWindow {
                        event_id: event_id.clone(),
                        symbol: symbol.clone(),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                        end_time: *end_time,
                        window_secs: *window_secs,
                        price_to_beat: *price_to_beat,
                    });
                Vec::new()
            }

            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won,
            } => {
                let mut decisions = Vec::new();
                let mut resolved_events = Vec::new();

                for events in self.events.values() {
                    for event in events {
                        if event.event_id != *event_id {
                            continue;
                        }
                        if let Some(up_won) =
                            self.resolve_up_won(event, *resolved_up_won)
                        {
                            decisions.extend(self.build_settlement_exits(
                                event, up_won, *end_time, positions,
                            ));
                            resolved_events.push(event.event_id.clone());
                        }
                    }
                }

                if !resolved_events.is_empty() {
                    for events in self.events.values_mut() {
                        events.retain(|e| !resolved_events.contains(&e.event_id));
                    }
                }

                decisions
            }

            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if fill.side == TradeSide::Buy {
            if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()).cloned() {
                self.last_entry.insert(symbol, fill.timestamp);
            }
            self.daily_trade_count += 1;
        }
    }

    fn name(&self) -> &str {
        "three_layer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_from_secs_boundaries() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(181), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(61),  Regime::Middle);
        assert_eq!(Regime::from_secs(60),  Regime::Late);
        assert_eq!(Regime::from_secs(6),   Regime::Late);
        assert_eq!(Regime::from_secs(5),   Regime::Expiry);
        assert_eq!(Regime::from_secs(0),   Regime::Expiry);
    }

    #[test]
    fn config_from_directional_preserves_fields() {
        let dc: DirectionalConfig = serde_json::from_str("{}").unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.min_edge, 0.03);
        assert_eq!(tlc.min_reward_risk, 1.2);
        assert_eq!(tlc.take_profit_ask, 0.70);
        assert!(!tlc.symbols.is_empty());
    }

    #[test]
    fn mprice_drift_accumulator_evicts_old_entries() {
        use chrono::TimeZone;
        let mut acc = MpriceDriftAccumulator::new(300.0);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        acc.push(t0, 1.5);
        acc.push(t0 + chrono::Duration::seconds(100), 2.0);
        acc.push(t0 + chrono::Duration::seconds(301), 3.0);
        assert!((acc.cum_drift() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn drift_tracker_detects_direction() {
        use chrono::TimeZone;
        let mut tracker = DriftTracker::new();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        for i in 0..30 {
            let t = t0 + chrono::Duration::seconds(i);
            let price = 50000.0 + i as f64 * 10.0;
            tracker.push(t, price);
        }
        assert!(tracker.drift_30s() > 0.0);
    }

    fn test_config() -> ThreeLayerConfig {
        ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
        }
    }

    #[test]
    fn direction_rejects_weak_early_signal() {
        let config = test_config();
        let result = evaluate_direction(0.1, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_none(), "should reject weak direction signal");
    }

    #[test]
    fn direction_passes_strong_early_signal() {
        let config = test_config();
        let result = evaluate_direction(1.5, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_some(), "should pass strong early signal");
        let (dir, prob) = result.unwrap();
        assert!(dir > 0.0);
        assert!(prob > 0.56);
    }

    #[test]
    fn confirmation_rejects_opposing_lob_in_late() {
        let config = test_config();
        let lob = LobState {
            obi: -0.5,
            obi_prev: -0.3,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,
            signed_trade_imbalance: -20.0,
            last_aggtrade_ts: None,
            ts: None,
        };
        let pass = evaluate_confirmation(1.0, &lob, -5.0, -0.001, Regime::Late, &config);
        assert!(!pass, "should reject opposing LOB in late regime");
    }

    #[test]
    fn worth_it_rejects_low_edge() {
        let config = test_config();
        // ask=0.35 → fee≈0.00455, edge=0.55-0.35-0.00455≈0.195, rr≈1.82 → passes
        let result = evaluate_worth_it(0.55, 0.35, Regime::Early, &config);
        assert!(result.is_some());
        // ask=0.50 → fee=0.005, edge=0.52-0.50-0.005=0.015 < min_edge → rejected
        let result = evaluate_worth_it(0.52, 0.50, Regime::Early, &config);
        assert!(result.is_none(), "should reject low edge");
    }

    #[test]
    fn norm_cdf_basic_values() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 0.001);
        assert!(norm_cdf(2.0) > 0.97);
        assert!(norm_cdf(-2.0) < 0.03);
    }
}
