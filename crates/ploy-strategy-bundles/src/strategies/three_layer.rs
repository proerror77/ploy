//! Three-layer strategy config and regime classification.

use chrono::{DateTime, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, OrderState, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::three_layer_profile::ThreeLayerProfile;
use crate::strategies::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};
use ploy_operator_contracts::Regime;

/// Configuration for the three-layer directional strategy.
#[derive(Debug, Clone)]
pub struct ThreeLayerConfig {
    pub symbols: Vec<String>,
    pub profile: ThreeLayerProfile,
    pub min_direction_prob: f64,
    pub allowed_directions: Vec<String>,
    pub min_distance_over_sigma: f64,
    pub min_confirmation_score: f64,
    pub require_confirmation: bool,
    pub min_drift_confirmation: f64,
    pub min_edge: f64,
    pub min_reward_risk: f64,
    pub alpha_contrarian: bool,
    pub cex_contrarian: bool,
    pub probability_shrink: f64,
    pub probability_haircut: f64,
    pub market_prior_weight: f64,
    pub confirmation_logit_weight: f64,
    pub take_profit_ask: f64,
    pub stop_distance_pct: f64,
    pub pre_settlement_exit_secs: u64,
    pub pre_settlement_min_exit_bid: f64,
    pub late_hold_ev_margin: Option<f64>,
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
    pub min_entry_score: f64,
}

impl From<DirectionalConfig> for ThreeLayerConfig {
    fn from(c: DirectionalConfig) -> Self {
        Self {
            symbols: c.symbols,
            profile: c.three_layer_strategy_profile,
            min_direction_prob: c.three_layer_min_direction_prob,
            allowed_directions: c.three_layer_allowed_directions,
            min_distance_over_sigma: c.three_layer_min_distance_over_sigma,
            min_confirmation_score: c.three_layer_min_confirmation_score,
            require_confirmation: c.three_layer_require_confirmation,
            min_drift_confirmation: c.three_layer_min_drift_confirmation,
            min_edge: c.three_layer_min_edge,
            min_reward_risk: c.three_layer_min_reward_risk,
            alpha_contrarian: c.three_layer_alpha_contrarian,
            cex_contrarian: c.three_layer_cex_contrarian,
            probability_shrink: c.three_layer_probability_shrink,
            probability_haircut: c.three_layer_probability_haircut,
            market_prior_weight: c.three_layer_market_prior_weight,
            confirmation_logit_weight: c.three_layer_confirmation_logit_weight,
            take_profit_ask: c.three_layer_take_profit_ask,
            stop_distance_pct: c.three_layer_stop_distance_pct,
            pre_settlement_exit_secs: c.three_layer_pre_settlement_exit_secs,
            pre_settlement_min_exit_bid: c.three_layer_pre_settlement_min_exit_bid,
            late_hold_ev_margin: c.three_layer_late_hold_ev_margin,
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
            min_entry_score: c.three_layer_min_entry_score,
        }
    }
}

const DRIFT_WINDOW_SECS: f64 = 30.0;

impl ThreeLayerConfig {
    fn allows_direction(&self, direction: &str) -> bool {
        self.allowed_directions.is_empty()
            || self
                .allowed_directions
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(direction))
    }
}
const VOL_WINDOW_SECS: f64 = 120.0;
const MIN_VOL_POINTS: usize = 5;
const ACCOUNT_BALANCE_REJECT_PAUSE_SECS: i64 = 15;
const HARD_TOKEN_REJECT_PAUSE_SECS: i64 = 35;
const NO_LIQUIDITY_REJECT_PAUSE_SECS: i64 = 15;

struct DriftTracker {
    history: VecDeque<(DateTime<Utc>, f64)>,
}

impl DriftTracker {
    fn new() -> Self {
        Self {
            history: VecDeque::new(),
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        if price <= 0.0 {
            return;
        }
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
        if self.history.len() < 2 {
            return 0.0;
        }
        let now = self.history.back().unwrap();
        let cutoff = now.0 - chrono::Duration::seconds(30);
        let anchor = self
            .history
            .iter()
            .find(|(ts, _)| *ts >= cutoff)
            .unwrap_or(self.history.front().unwrap());
        now.1 - anchor.1
    }

    fn sigma_horizon(&self, horizon_secs: f64) -> f64 {
        if self.history.len() < MIN_VOL_POINTS {
            return 0.0;
        }
        let values = self
            .history
            .iter()
            .map(|(_, price)| *price)
            .collect::<Vec<_>>();
        let returns: Vec<f64> = values.windows(2).map(|w| w[1] - w[0]).collect();
        if returns.is_empty() {
            return 0.0;
        }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        let avg_dt = {
            let total_secs = (self.history.back().unwrap().0 - self.history.front().unwrap().0)
                .num_milliseconds() as f64
                / 1000.0;
            total_secs / returns.len() as f64
        };
        if avg_dt <= 0.0 {
            return 0.0;
        }
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
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
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
struct QuoteDepth {
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
}

struct QuoteAskHistory {
    entries: VecDeque<(DateTime<Utc>, f64)>,
}

impl QuoteAskHistory {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, ask: f64) {
        if !ask.is_finite() {
            return;
        }
        self.entries.push_back((ts, ask));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_seconds() > 35 {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn change_since(&self, ts: DateTime<Utc>, now_ask: f64, secs: i64) -> Option<f64> {
        let cutoff = ts - chrono::Duration::seconds(secs);
        self.entries
            .iter()
            .rev()
            .find(|(entry_ts, _)| *entry_ts <= cutoff)
            .map(|(_, ask)| now_ask - *ask)
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
        if total <= 0.0 {
            return 0.0;
        }
        (self.bid_depth_near - self.ask_depth_near) / total
    }

    fn obi_delta(&self) -> f64 {
        self.obi - self.obi_prev
    }

    fn apply_l2(
        &mut self,
        obi_raw: f64,
        spread_bps: u32,
        bid_near: f64,
        ask_near: f64,
        ts: DateTime<Utc>,
    ) {
        self.obi_prev = self.obi;
        // EWMA smoothing with 5-second half-life.
        // At 10 Hz LOB, raw OBI fluctuates wildly per 100ms tick.
        // Smoothing captures the 5-second trend, filtering noise while
        // preserving real microstructure signals. Causal (no look-ahead).
        if let Some(prev_ts) = self.ts {
            let dt = (ts - prev_ts).num_milliseconds() as f64 / 1000.0;
            if dt > 0.0 && dt < 60.0 {
                let alpha = 1.0 - (-dt / 5.0_f64).exp();
                self.obi = self.obi * (1.0 - alpha) + obi_raw * alpha;
            } else {
                self.obi = obi_raw;
            }
        } else {
            self.obi = obi_raw;
        }
        self.spread_bps = spread_bps;
        self.bid_depth_near = bid_near;
        self.ask_depth_near = ask_near;
        self.ts = Some(ts);
    }

    fn apply_aggtrade(&mut self, quantity: f64, is_buyer_maker: bool, ts: DateTime<Utc>) {
        let seconds = self
            .last_aggtrade_ts
            .map(|last| (ts - last).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0);
        let decay = if seconds > 0.0 {
            (-seconds / 30.0).exp()
        } else {
            1.0
        };
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

// ── Gate Functions ──────────────────────────────────────────────────

/// Normal CDF approximation (Abramowitz & Stegun).
fn norm_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327 * (-x * x / 2.0).exp();
    let p =
        d * t * (0.3193815 + t * (-0.3565638 + t * (1.781478 + t * (-1.821256 + t * 1.330274))));
    if x >= 0.0 { 1.0 - p } else { p }
}

fn threshold_score(value: f64, threshold: f64, scale: f64, contrarian: bool) -> f64 {
    if !value.is_finite() || !threshold.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return -0.50;
    }
    let signed = if contrarian {
        threshold - value
    } else {
        value - threshold
    };
    (signed / scale).clamp(-0.50, 1.0)
}

fn calibrate_direction_probability(
    direction_probability: f64,
    probability_shrink: f64,
    probability_haircut: f64,
) -> f64 {
    if !direction_probability.is_finite()
        || !probability_shrink.is_finite()
        || !probability_haircut.is_finite()
    {
        return f64::NAN;
    }
    let shrink = probability_shrink.clamp(0.0, 1.0);
    let haircut = probability_haircut.clamp(0.0, 0.49);
    (0.5 + (direction_probability - 0.5) * shrink - haircut).clamp(0.01, 0.99)
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(0.01, 0.99);
    (p / (1.0 - p)).ln()
}

fn inv_logit(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn bayesian_execution_probability(
    model_probability: f64,
    market_prior: f64,
    confirmation_score: f64,
    config: &ThreeLayerConfig,
) -> f64 {
    if !model_probability.is_finite() || !market_prior.is_finite() {
        return f64::NAN;
    }

    let prior_weight = config.market_prior_weight.clamp(0.0, 0.95);
    let model_weight = 1.0 - prior_weight;
    let confirmation_bump = if confirmation_score.is_finite() {
        confirmation_score * config.confirmation_logit_weight.clamp(0.0, 5.0)
    } else {
        0.0
    };

    inv_logit(
        model_weight * logit(model_probability)
            + prior_weight * logit(market_prior)
            + confirmation_bump,
    )
    .clamp(0.01, 0.99)
}

fn executable_edge_threshold(config: &ThreeLayerConfig) -> f64 {
    if config.profile.uses_snapshot_scoring() {
        config.min_edge.max(0.0)
    } else {
        config.min_edge
    }
}

fn profile_confirmation_score(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> f64 {
    match config.profile {
        ThreeLayerProfile::Mixed => evaluate_confirmation_bonus(
            direction_sign,
            lob,
            cum_mprice_drift_5m,
            drift_30s,
            regime,
            config,
        ),
        ThreeLayerProfile::Champion => 0.0,
        ThreeLayerProfile::ObiSoft | ThreeLayerProfile::ObiHard => {
            let obi = (lob.obi * direction_sign).clamp(-1.0, 1.0);
            let obi_delta = (lob.obi_delta() * direction_sign).clamp(-1.0, 1.0);
            let depth = (lob.depth_imbalance() * direction_sign).clamp(-1.0, 1.0);
            let microprice = (cum_mprice_drift_5m * direction_sign).clamp(-1.0, 1.0);
            let trade_imbalance =
                ((lob.signed_trade_imbalance / 50.0) * direction_sign).clamp(-1.0, 1.0);

            0.30 * obi
                + 0.20 * obi_delta
                + 0.20 * obi
                + 0.15 * depth
                + 0.10 * microprice
                + 0.05 * trade_imbalance
        }
        ThreeLayerProfile::ContinuationSoft => {
            let drift_continuation = (drift_30s * direction_sign * 800.0).clamp(-1.0, 1.0);
            let microprice = (cum_mprice_drift_5m * direction_sign).clamp(-1.0, 1.0);
            let trade_imbalance =
                ((lob.signed_trade_imbalance / 50.0) * direction_sign).clamp(-1.0, 1.0);
            0.50 * drift_continuation + 0.30 * microprice + 0.20 * trade_imbalance
        }
    }
}

struct EntryScoreInputs {
    direction_score: f64,
    distance_over_sigma: f64,
    direction_sign: f64,
    edge: f64,
    edge_score: f64,
    confirmation: f64,
    drift_30s: f64,
    pm_momentum_score: f64,
    liquidity_score: f64,
}

fn evaluate_entry_score(config: &ThreeLayerConfig, inputs: EntryScoreInputs) -> f64 {
    if !config.profile.uses_snapshot_scoring() {
        return inputs.direction_score * 0.50
            + inputs.edge_score * 0.35
            + inputs.confirmation * 0.15;
    }

    let side_distance = inputs.distance_over_sigma * inputs.direction_sign;
    let distance_score = threshold_score(
        side_distance,
        config.min_distance_over_sigma,
        0.60,
        config.alpha_contrarian,
    );
    let edge_score = threshold_score(inputs.edge, executable_edge_threshold(config), 0.08, false);
    let drift_side = inputs.drift_30s * inputs.direction_sign;
    let drift_score = ((drift_side - config.min_drift_confirmation) * 800.0).clamp(-0.50, 1.0);
    let confirmation_score = threshold_score(
        inputs.confirmation,
        config.min_confirmation_score,
        0.50,
        config.cex_contrarian,
    );

    match config.profile {
        ThreeLayerProfile::Champion => {
            0.33 * inputs.direction_score
                + 0.17 * distance_score
                + 0.25 * edge_score
                + 0.10 * drift_score
                + 0.10 * inputs.pm_momentum_score
                + 0.05 * inputs.liquidity_score
        }
        ThreeLayerProfile::ObiSoft
        | ThreeLayerProfile::ObiHard
        | ThreeLayerProfile::ContinuationSoft => {
            0.25 * inputs.direction_score
                + 0.12 * distance_score
                + 0.18 * edge_score
                + 0.15 * confirmation_score
                + 0.10 * drift_score
                + 0.12 * inputs.pm_momentum_score
                + 0.08 * inputs.liquidity_score
        }
        ThreeLayerProfile::Mixed => unreachable!("mixed profile returned above"),
    }
}

fn confirmation_gate_passes(value: f64, threshold: f64, contrarian: bool) -> bool {
    if !value.is_finite() || !threshold.is_finite() {
        return false;
    }
    if contrarian {
        value <= threshold
    } else {
        value >= threshold
    }
}

/// Layer 1: Direction score (0.0 – 1.0).
/// Returns Some((direction_sign, calibrated_probability, direction_score)) or None
/// only when the signal is too weak to even consider (below minimum distance in Early).
///
/// In snapshot-scored profiles, contrarian mode inverts the direction but keeps
/// the transformed alpha probability monotonic: stronger inverse alpha scores
/// higher. The hard direction gate uses raw alpha strength, while execution EV
/// is evaluated with the calibrated probability.
fn evaluate_direction_score(
    distance_over_sigma: f64,
    _sigma_horizon: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64)> {
    if !config.alpha_contrarian
        && distance_over_sigma.abs() < config.min_distance_over_sigma
        && regime == Regime::Early
    {
        return None;
    }

    let direction_prob =
        model_probability_up(distance_over_sigma, cum_mprice_drift_5m, drift_30s, regime);

    let (direction_sign, raw_effective_p) = if config.alpha_contrarian {
        let inverse_alpha_p = direction_prob.max(1.0 - direction_prob);
        if direction_prob >= 0.5 {
            (-1.0_f64, inverse_alpha_p)
        } else {
            (1.0_f64, inverse_alpha_p)
        }
    } else if direction_prob >= 0.5 {
        (1.0_f64, direction_prob)
    } else {
        (-1.0_f64, 1.0 - direction_prob)
    };
    if !raw_effective_p.is_finite() || raw_effective_p < config.min_direction_prob {
        return None;
    }

    let effective_p = calibrate_direction_probability(
        raw_effective_p,
        config.probability_shrink,
        config.probability_haircut,
    );
    if !effective_p.is_finite() {
        return None;
    }

    // In contrarian mode the direction is inverted, but the alpha strength is
    // still the distance from 50/50. Do not treat the faded side's raw model
    // probability as the executable probability.
    let direction_score = if config.profile.uses_snapshot_scoring() {
        threshold_score(raw_effective_p, config.min_direction_prob, 0.25, false)
    } else {
        ((effective_p - 0.50) / 0.50).clamp(0.0, 1.0)
    };

    Some((direction_sign, effective_p, direction_score))
}

fn model_probability_up(
    distance_over_sigma: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
) -> f64 {
    let model_prob_up = norm_cdf(distance_over_sigma);
    match regime {
        Regime::Early => model_prob_up,
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
    }
}

fn raw_probability_for_side(
    model_probability_up: f64,
    is_up: bool,
    config: &ThreeLayerConfig,
) -> f64 {
    if config.alpha_contrarian {
        if is_up {
            1.0 - model_probability_up
        } else {
            model_probability_up
        }
    } else if is_up {
        model_probability_up
    } else {
        1.0 - model_probability_up
    }
}

/// Layer 2: Confirmation bonus (-0.2 to +0.2).
/// Positive = LOB confirms direction, negative = LOB opposes (penalty, not veto).
fn evaluate_confirmation_bonus(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> f64 {
    let raw_score = lob.confirmation_score() + (cum_mprice_drift_5m / 200.0).clamp(-0.15, 0.15);
    let aligned_score = direction_sign * raw_score;

    // In Late/Expiry, drift agreement adds extra bonus/penalty.
    let drift_factor = match regime {
        Regime::Late | Regime::Expiry => {
            let drift_aligned = drift_30s * direction_sign;
            (drift_aligned * 500.0).clamp(-0.10, 0.10)
        }
        _ => 0.0,
    };

    let score = aligned_score + drift_factor;
    if config.cex_contrarian {
        (-score).clamp(-0.20, 0.20)
    } else {
        score.clamp(-0.20, 0.20)
    }
}

fn expected_value_per_share(direction_probability: f64, entry_price: f64) -> f64 {
    if !direction_probability.is_finite()
        || !entry_price.is_finite()
        || !(0.0..=1.0).contains(&direction_probability)
        || !(0.0..1.0).contains(&entry_price)
    {
        return f64::NAN;
    }
    let fee = crypto_fee_cost(entry_price);
    let win_payoff = 1.0 - entry_price - fee;
    let loss_cost = entry_price + fee;
    direction_probability * win_payoff - (1.0 - direction_probability) * loss_cost
}

fn expected_value_per_staked_dollar(direction_probability: f64, entry_price: f64) -> f64 {
    let expected_value = expected_value_per_share(direction_probability, entry_price);
    if !expected_value.is_finite() || !entry_price.is_finite() || entry_price <= 0.0 {
        return f64::NAN;
    }
    expected_value / entry_price
}

/// Layer 3: Expected-value score (0.0 – 1.0).
/// Returns Some((entry_price, expected_value_per_share, reward_risk, expectancy_score)) or None
/// only when the price is outside tradeable bounds.
fn evaluate_edge_score(
    direction_probability: f64,
    ask: f64,
    _regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64, f64)> {
    if ask < config.min_entry_price || ask > config.max_entry_price {
        return None;
    }

    let fee = crypto_fee_cost(ask);
    let edge = expected_value_per_share(direction_probability, ask);
    if !edge.is_finite() {
        return None;
    }

    let reward = 1.0 - ask - fee;
    let risk = ask + fee;
    let rr = if risk > 0.0 { reward / risk } else { 0.0 };

    // Execution edge is always monotonic: higher cost-adjusted edge scores
    // better. Contrarian mode can invert alpha, but not executable edge.
    let required_edge = executable_edge_threshold(config);
    if config.profile.uses_snapshot_scoring() && edge < required_edge {
        return None;
    }

    let stake_expectancy = expected_value_per_staked_dollar(direction_probability, ask);
    let edge_score = if config.profile.uses_snapshot_scoring() {
        let per_share_score = threshold_score(edge, required_edge, 0.08, false);
        let per_stake_score = threshold_score(stake_expectancy, 0.0, 0.25, false);
        (0.70 * per_share_score + 0.30 * per_stake_score).clamp(-0.50, 1.0)
    } else {
        (stake_expectancy / 0.40).clamp(0.0, 1.0)
    };

    Some((ask, edge, rr, edge_score))
}

// ── ThreeLayerStrategy ─────────────────────────────────────────────

pub struct ThreeLayerStrategy {
    config: ThreeLayerConfig,
    drift: HashMap<Arc<str>, DriftTracker>,
    lob: HashMap<Arc<str>, LobState>,
    mprice_acc: HashMap<Arc<str>, MpriceDriftAccumulator>,
    spot: HashMap<Arc<str>, Decimal>,
    quotes: HashMap<Arc<str>, QuoteState>,
    quote_depth: HashMap<Arc<str>, QuoteDepth>,
    quote_ask_history: HashMap<Arc<str>, QuoteAskHistory>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    token_reject_until: HashMap<Arc<str>, DateTime<Utc>>,
    balance_exhausted_until: Option<DateTime<Utc>>,
    daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
    feed_time: Option<DateTime<Utc>>,
    diagnostics: HashMap<&'static str, u64>,
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
            quote_depth: HashMap::new(),
            quote_ask_history: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            events: HashMap::new(),
            last_entry: HashMap::new(),
            token_reject_until: HashMap::new(),
            balance_exhausted_until: None,
            daily_trade_count: 0,
            daily_trade_date: None,
            feed_time: None,
            diagnostics: HashMap::new(),
        }
    }

    fn bump(&mut self, key: &'static str) {
        self.bump_by(key, 1);
    }

    fn bump_by(&mut self, key: &'static str, value: u64) {
        *self.diagnostics.entry(key).or_insert(0) += value;
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
                        self.window_allowed(e.window_secs) && e.end_time > now && {
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

    fn late_hold_ev_gap(
        &self,
        event: &EventWindow,
        is_up: bool,
        now: DateTime<Utc>,
        spot_price: f64,
        exit_bid: Decimal,
    ) -> Option<f64> {
        let margin = self.config.late_hold_ev_margin?;
        let price_to_beat = event.price_to_beat.and_then(|price| price.to_f64())?;
        if price_to_beat <= 0.0 {
            return None;
        }
        let drift_tracker = self.drift.get(event.symbol.as_ref())?;
        let time_remaining_secs = (event.end_time - now).num_seconds().max(1);
        let regime = Regime::from_secs(time_remaining_secs);
        if !matches!(regime, Regime::Late | Regime::Expiry) {
            return None;
        }
        let sigma_h = drift_tracker.sigma_horizon(time_remaining_secs as f64);
        let distance_over_sigma = if sigma_h > 0.0 {
            (spot_price - price_to_beat) / (sigma_h * price_to_beat)
        } else {
            0.0
        };
        let drift_30s = drift_tracker.drift_30s();
        let cum_mprice_drift_5m = self
            .mprice_acc
            .get(event.symbol.as_ref())
            .map(|acc| acc.cum_drift())
            .unwrap_or(0.0);
        let model_p_up =
            model_probability_up(distance_over_sigma, cum_mprice_drift_5m, drift_30s, regime);
        let raw_side_p = raw_probability_for_side(model_p_up, is_up, &self.config);
        if !raw_side_p.is_finite() {
            return None;
        }
        let calibrated_p = calibrate_direction_probability(
            raw_side_p,
            self.config.probability_shrink,
            self.config.probability_haircut,
        );
        if !calibrated_p.is_finite() {
            return None;
        }
        let direction_sign = if is_up { 1.0 } else { -1.0 };
        let lob = self
            .lob
            .get(event.symbol.as_ref())
            .copied()
            .unwrap_or_default();
        let confirmation_score = profile_confirmation_score(
            direction_sign,
            &lob,
            cum_mprice_drift_5m,
            drift_30s,
            regime,
            &self.config,
        );
        let adverse_confirmation = confirmation_score <= -self.config.min_confirmation_score.abs();
        let adverse_drift = drift_30s * direction_sign < -self.config.min_drift_confirmation.abs();
        if !adverse_confirmation && !adverse_drift {
            return None;
        }
        let bid = exit_bid.to_f64()?;
        if !bid.is_finite() || bid <= 0.0 {
            return None;
        }
        let posterior_p =
            bayesian_execution_probability(calibrated_p, bid, confirmation_score, &self.config);
        if !posterior_p.is_finite() {
            return None;
        }
        let executable_sell_value = (bid - crypto_fee_cost(bid)).max(0.0);
        let hold_gap = posterior_p - executable_sell_value;
        (hold_gap < -margin.max(0.0)).then_some(hold_gap)
    }

    fn reject_cooldown_secs(&self) -> i64 {
        self.config.cooldown_secs.max(1) as i64
    }

    fn balance_pause_active(&self, now: DateTime<Utc>) -> bool {
        self.balance_exhausted_until
            .is_some_and(|until| now < until)
    }

    fn token_reject_active(&self, token_id: &str, now: DateTime<Utc>) -> bool {
        self.token_reject_until
            .get(token_id)
            .is_some_and(|until| now < *until)
    }

    fn set_token_reject_until(&mut self, token_id: &str, until: DateTime<Utc>) {
        self.token_reject_until
            .entry(Arc::<str>::from(token_id))
            .and_modify(|existing| {
                if until > *existing {
                    *existing = until;
                }
            })
            .or_insert(until);
    }

    fn set_balance_exhausted_until(&mut self, until: DateTime<Utc>) {
        match self.balance_exhausted_until {
            Some(existing) if existing >= until => {}
            _ => self.balance_exhausted_until = Some(until),
        }
    }

    fn record_quote_ask(&mut self, token_id: Arc<str>, ask: Option<Decimal>, ts: DateTime<Utc>) {
        let Some(ask) = ask.and_then(|price| price.to_f64()) else {
            return;
        };
        self.quote_ask_history
            .entry(token_id)
            .or_insert_with(QuoteAskHistory::new)
            .push(ts, ask);
    }

    fn pm_momentum_score(&self, token_id: &str, ask: f64, now: DateTime<Utc>) -> f64 {
        let Some(history) = self.quote_ask_history.get(token_id) else {
            return 0.0;
        };
        let pm_momentum = [
            history.change_since(now, ask, 10),
            history.change_since(now, ask, 30),
        ]
        .into_iter()
        .flatten()
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);

        if pm_momentum.is_finite() {
            (pm_momentum / 0.08).clamp(-0.50, 1.0)
        } else {
            0.0
        }
    }

    fn active_order_exists(orders: &OrderLedger, token_id: &str) -> bool {
        orders.orders().any(|o| {
            o.token_id == token_id
                && matches!(
                    o.state,
                    OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
                )
        })
    }

    fn try_entry(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        self.bump("entry_evaluations");
        if self.balance_pause_active(now) {
            self.bump("skip_balance_pause");
            debug!(symbol, "Balance exhausted pause active, skipping entry");
            return None;
        }
        if self.daily_trade_count >= self.config.max_daily_trades {
            self.bump("skip_max_daily_trades");
            return None;
        }
        if positions.positions().count() >= self.config.max_positions {
            self.bump("skip_max_positions");
            return None;
        }

        let Some(spot_price) = self.spot.get(symbol).and_then(|price| price.to_f64()) else {
            self.bump("skip_no_spot");
            return None;
        };
        if spot_price <= 0.0 {
            self.bump("skip_bad_spot");
            return None;
        }

        // Cooldown check
        if let Some(last) = self.last_entry.get(symbol) {
            if (now - *last).num_seconds() < self.config.cooldown_secs as i64 {
                self.bump("skip_symbol_cooldown");
                return None;
            }
        }

        let candidates = self.candidate_events(symbol, now);
        if candidates.is_empty() {
            self.bump("skip_no_candidate_events");
        } else {
            self.bump_by("candidate_events", candidates.len() as u64);
        }
        for event in &candidates {
            let time_remaining = (event.end_time - now).num_seconds();
            if self.config.pre_settlement_exit_secs > 0
                && time_remaining >= 0
                && time_remaining as u64 <= self.config.pre_settlement_exit_secs
            {
                self.bump("skip_pre_settlement_entry");
                continue;
            }
            let Some(price_to_beat) = event.price_to_beat.and_then(|price| price.to_f64()) else {
                self.bump("skip_no_price_to_beat");
                continue;
            };
            if price_to_beat <= 0.0 {
                self.bump("skip_bad_price_to_beat");
                continue;
            }

            let regime = Regime::from_secs(time_remaining);

            let Some(drift_tracker) = self.drift.get_mut(symbol) else {
                self.bump("skip_no_drift_tracker");
                return None;
            };
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

            // Layer 1: Direction score
            let Some((direction_sign, effective_p, direction_score)) = evaluate_direction_score(
                distance_over_sigma,
                sigma_h,
                cum_mprice_drift_5m,
                drift_30s,
                regime,
                &self.config,
            ) else {
                self.bump("skip_direction_score");
                continue;
            };

            let betting_up = direction_sign > 0.0;
            let (token_id, direction) = if betting_up {
                (&event.up_token, "UP")
            } else {
                (&event.down_token, "DOWN")
            };
            if !self.config.allows_direction(direction) {
                self.bump("skip_direction_filter");
                continue;
            }

            // Skip if already positioned or have active order on this token
            if positions.net_qty(token_id) > Decimal::ZERO {
                self.bump("skip_existing_position");
                continue;
            }
            if Self::active_order_exists(orders, token_id) {
                self.bump("skip_active_order");
                continue;
            }
            if self.token_reject_active(token_id, now) {
                self.bump("skip_token_reject_cooldown");
                continue;
            }

            // Check quote freshness
            let Some(quote) = self.quotes.get(token_id) else {
                self.bump("skip_no_pm_quote");
                continue;
            };
            let Some(ask) = quote.ask.and_then(|price| price.to_f64()) else {
                self.bump("skip_no_pm_ask");
                continue;
            };
            let quote_age = (now - quote.ts).num_seconds();
            if quote_age > self.config.max_pm_lag_secs as i64 {
                self.bump("skip_stale_pm_quote");
                continue;
            }

            // Layer 2: profile-specific confirmation component
            let lob = self.lob.get(symbol).copied().unwrap_or_default();
            let confirmation_score = profile_confirmation_score(
                direction_sign,
                &lob,
                cum_mprice_drift_5m,
                drift_30s,
                regime,
                &self.config,
            );
            if self.config.require_confirmation
                && !confirmation_gate_passes(
                    confirmation_score,
                    self.config.min_confirmation_score,
                    self.config.cex_contrarian,
                )
            {
                self.bump("skip_confirmation_gate");
                continue;
            }

            // Layer 3: Edge score
            let posterior_p =
                bayesian_execution_probability(effective_p, ask, confirmation_score, &self.config);
            if !posterior_p.is_finite() {
                self.bump("skip_bad_posterior_probability");
                continue;
            }
            let Some((entry_price_f, edge, rr, edge_score)) =
                evaluate_edge_score(posterior_p, ask, regime, &self.config)
            else {
                self.bump("skip_edge_score");
                continue;
            };
            if self.config.profile.uses_snapshot_scoring() && rr < self.config.min_reward_risk {
                self.bump("skip_reward_risk");
                continue;
            }

            let pm_momentum_score = self.pm_momentum_score(token_id, ask, now);
            let total_score = evaluate_entry_score(
                &self.config,
                EntryScoreInputs {
                    direction_score,
                    distance_over_sigma,
                    direction_sign,
                    edge,
                    edge_score,
                    confirmation: confirmation_score,
                    drift_30s,
                    pm_momentum_score,
                    liquidity_score: 1.0,
                },
            );

            if total_score < self.config.min_entry_score {
                self.bump("skip_entry_score");
                info!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    total_score = format!("{:.3}", total_score),
                    direction_score = format!("{:.3}", direction_score),
                    edge_score = format!("{:.3}", edge_score),
                    confirmation_score = format!("{:.3}", confirmation_score),
                    model_p = effective_p,
                    posterior_p,
                    pm_momentum_score = format!("{:.3}", pm_momentum_score),
                    min_entry_score = self.config.min_entry_score,
                    profile = %self.config.profile,
                    regime = regime.as_str(),
                    "score below threshold"
                );
                continue;
            }

            let Some(entry_price) = Decimal::try_from(entry_price_f).ok() else {
                self.bump("skip_bad_entry_price");
                continue;
            };
            let quantity = self.entry_quantity(entry_price);
            if quantity <= Decimal::ZERO {
                self.bump("skip_zero_quantity");
                continue;
            }
            let Some(ask_size) = self
                .quote_depth
                .get(token_id)
                .and_then(|depth| depth.ask_size)
            else {
                self.bump("skip_no_ask_size");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    "PM quote has no ask size; skipping non-executable entry"
                );
                continue;
            };
            if ask_size < quantity {
                self.bump("skip_insufficient_ask_size");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    ask_size = %ask_size,
                    "PM ask size cannot fill fixed stake; skipping entry"
                );
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
                p_hat: posterior_p,
                edge,
                entry_price,
                decision: "enter".to_string(),
                ts: now,
            };

            info!(
                strategy = "three_layer",
                symbol = %symbol,
                direction,
                total_score = format!("{:.3}", total_score),
                direction_score = format!("{:.3}", direction_score),
                edge_score = format!("{:.3}", edge_score),
                confirmation_score = format!("{:.3}", confirmation_score),
                pm_momentum_score = format!("{:.3}", pm_momentum_score),
                model_p = effective_p,
                p_hat = posterior_p,
                edge,
                entry_price = %entry_price,
                profile = %self.config.profile,
                regime = regime.as_str(),
                "entry signal"
            );

            self.bump("entry_signals");
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
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }
                if Self::active_order_exists(orders, token_id) {
                    continue;
                }
                if self.token_reject_active(token_id, now) {
                    continue;
                }
                let Some(bid_size) = self
                    .quote_depth
                    .get(token_id)
                    .and_then(|depth| depth.bid_size)
                else {
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        "PM quote has no bid size; skipping non-executable exit"
                    );
                    continue;
                };
                if bid_size < qty {
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        bid_size = %bid_size,
                        "PM bid size cannot fill current position; skipping exit"
                    );
                    continue;
                }

                let Some(exit_bid) = self.quotes.get(token_id).and_then(|q| q.bid) else {
                    continue;
                };

                // Take profit must be executable: SELL can only hit the bid.
                // The config field keeps its historical name for TOML compatibility.
                if let Some(bid) = exit_bid.to_f64() {
                    if bid >= self.config.take_profit_ask {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!("tl_tp_{}_{}", token_id, now.timestamp_millis()),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_bid),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                        continue;
                    }
                }

                if self.config.pre_settlement_exit_secs > 0 {
                    let secs_to_end = (event.end_time - now).num_seconds();
                    if secs_to_end >= 0
                        && secs_to_end as u64 <= self.config.pre_settlement_exit_secs
                        && exit_bid
                            .to_f64()
                            .is_some_and(|bid| bid >= self.config.pre_settlement_min_exit_bid)
                    {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!(
                                "tl_pre_settle_{}_{}",
                                token_id,
                                now.timestamp_millis()
                            ),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_bid),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                        continue;
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
                            intent_id: format!("tl_sl_{}_{}", token_id, now.timestamp_millis()),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_bid),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                        continue;
                    }
                }

                if let Some(spot_price) = spot {
                    if self
                        .late_hold_ev_gap(event, is_up, now, spot_price, exit_bid)
                        .is_some()
                    {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!(
                                "tl_late_ev_{}_{}",
                                token_id,
                                now.timestamp_millis()
                            ),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_bid),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                        continue;
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
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        if positions.net_qty(&event.up_token) > Decimal::ZERO {
            if Self::active_order_exists(orders, &event.up_token) {
                debug!(
                    token_id = %event.up_token,
                    "Active order exists, skipping settlement exit"
                );
            } else if self.token_reject_active(&event.up_token, now) {
                debug!(
                    token_id = %event.up_token,
                    "Token reject cooldown active, skipping settlement exit"
                );
            } else {
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
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            if Self::active_order_exists(orders, &event.down_token) {
                debug!(
                    token_id = %event.down_token,
                    "Active order exists, skipping settlement exit"
                );
            } else if self.token_reject_active(&event.down_token, now) {
                debug!(
                    token_id = %event.down_token,
                    "Token reject cooldown active, skipping settlement exit"
                );
            } else {
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
        }

        exits
    }

    fn resolve_up_won(&self, event: &EventWindow, resolved: Option<bool>) -> Option<bool> {
        settlement::resolve_up_won(
            resolved,
            self.spot.get(&event.symbol).copied(),
            event.price_to_beat,
        )
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
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
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
                    self.exit_decisions_for_symbol(symbol, *ts, spot_opt, positions, orders);

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
                bid_size,
                ask_size,
                ts,
            } => {
                self.feed_time = Some(*ts);
                self.quotes.insert(
                    token_id.clone(),
                    QuoteState {
                        bid: *bid,
                        ask: *ask,
                        ts: *ts,
                    },
                );
                self.quote_depth.insert(
                    token_id.clone(),
                    QuoteDepth {
                        bid_size: *bid_size,
                        ask_size: *ask_size,
                    },
                );
                self.record_quote_ask(token_id.clone(), *ask, *ts);

                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                let spot = self.spot.get(&symbol).and_then(|p| p.to_f64());
                self.exit_decisions_for_symbol(&symbol, *ts, spot, positions, orders)
            }

            MarketUpdate::AggTrade {
                symbol,
                quantity,
                is_buyer_maker,
                ts,
                ..
            } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                let qty_f64 = quantity.to_f64().unwrap_or(0.0);
                self.lob.entry(symbol.clone()).or_default().apply_aggtrade(
                    qty_f64,
                    *is_buyer_maker,
                    *ts,
                );
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
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.lob.entry(symbol.clone()).or_default().apply_l2(
                    *obi,
                    *spread_bps,
                    *bid_depth_near,
                    *ask_depth_near,
                    *ts,
                );

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
                    // Same EWMA smoothing as L2Depth path
                    if let Some(prev_ts) = lob.ts {
                        let dt = (*ts - prev_ts).num_milliseconds() as f64 / 1000.0;
                        if dt > 0.0 && dt < 60.0 {
                            let alpha = 1.0 - (-dt / 5.0_f64).exp();
                            lob.obi = lob.obi * (1.0 - alpha) + obi * alpha;
                        } else {
                            lob.obi = *obi;
                        }
                    } else {
                        lob.obi = *obi;
                    }
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
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                    || !self.window_allowed(*window_secs)
                {
                    return Vec::new();
                }

                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());
                self.token_event.insert(up_token.clone(), event_id.clone());
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
                        if let Some(up_won) = self.resolve_up_won(event, *resolved_up_won) {
                            decisions.extend(self.build_settlement_exits(
                                event, up_won, *end_time, positions, orders,
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

    fn on_reject(&mut self, intent: &TradingIntent, reason: &str) {
        let now = self.now();
        let reason_lc = reason.to_ascii_lowercase();
        // Reject-string provenance:
        // - Polymarket CLOB/live executor: "not enough balance", "not enough allowance",
        //   "no orders found to match with FAK order", "insufficient liquidity", "no match".
        // - Venue precision/min-size rejects observed in live records: "invalid amount(s)".
        // - Local engine dust guard: "below retry threshold".
        let balance_or_allowance = reason_lc.contains("not enough balance")
            || reason_lc.contains("not enough allowance")
            || (reason_lc.contains("not enough") && reason_lc.contains("allowance"))
            || (reason_lc.contains("insufficient") && reason_lc.contains("allowance"));
        let invalid_amount =
            reason_lc.contains("invalid amount") || reason_lc.contains("below retry threshold");
        let no_liquidity = reason_lc.contains("no orders found")
            || reason_lc.contains("insufficient liquidity")
            || reason_lc.contains("no match");
        let token_cooldown_secs = if balance_or_allowance || invalid_amount {
            HARD_TOKEN_REJECT_PAUSE_SECS
        } else if no_liquidity {
            NO_LIQUIDITY_REJECT_PAUSE_SECS
        } else {
            self.reject_cooldown_secs()
        };
        let token_until = now + chrono::Duration::seconds(token_cooldown_secs);

        if balance_or_allowance && intent.side == TradeSide::Buy {
            self.set_balance_exhausted_until(
                now + chrono::Duration::seconds(ACCOUNT_BALANCE_REJECT_PAUSE_SECS),
            );
        }

        self.set_token_reject_until(&intent.token_id, token_until);

        if intent.side == TradeSide::Buy {
            if let Some(symbol) = self.token_symbol.get(intent.token_id.as_str()).cloned() {
                self.last_entry.insert(symbol, now);
            }
        }

        warn!(
            strategy = self.name(),
            intent_id = %intent.intent_id,
            token_id = %intent.token_id,
            reason = %reason,
            cooldown_secs = token_cooldown_secs,
            "Order rejected; suppressing duplicate live intents during cooldown"
        );
    }

    fn name(&self) -> &str {
        "three_layer"
    }

    fn diagnostics(&self) -> Vec<(String, u64)> {
        let mut diagnostics = self
            .diagnostics
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect::<Vec<_>>();
        diagnostics.sort_by(|a, b| a.0.cmp(&b.0));
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn regime_from_secs_boundaries() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(181), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(61), Regime::Middle);
        assert_eq!(Regime::from_secs(60), Regime::Late);
        assert_eq!(Regime::from_secs(6), Regime::Late);
        assert_eq!(Regime::from_secs(5), Regime::Expiry);
        assert_eq!(Regime::from_secs(0), Regime::Expiry);
    }

    #[test]
    fn config_from_directional_preserves_fields() {
        let dc: DirectionalConfig = serde_json::from_str("{}").unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::Mixed);
        assert_eq!(tlc.min_edge, 0.03);
        assert_eq!(tlc.min_reward_risk, 1.2);
        assert_eq!(tlc.take_profit_ask, 0.70);
        assert_eq!(tlc.pre_settlement_exit_secs, 0);
        assert_eq!(tlc.pre_settlement_min_exit_bid, 0.01);
        assert_eq!(tlc.late_hold_ev_margin, None);
        assert!((tlc.min_entry_score - 0.30).abs() < f64::EPSILON);
        assert!(!tlc.symbols.is_empty());
    }

    #[test]
    fn config_from_directional_accepts_profile_aliases() {
        let dc: DirectionalConfig = serde_json::from_str(r#"{"strategy_profile":"obi"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::ObiSoft);

        let dc: DirectionalConfig =
            serde_json::from_str(r#"{"strategy_profile":"obi_hard"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::ObiHard);
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

    #[test]
    fn diagnostics_count_entry_gate_reasons() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());

        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );

        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("entry_evaluations"), Some(&1));
        assert_eq!(diagnostics.get("skip_no_candidate_events"), Some(&1));
    }

    fn test_config() -> ThreeLayerConfig {
        ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            profile: ThreeLayerProfile::Mixed,
            min_direction_prob: 0.56,
            allowed_directions: Vec::new(),
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            require_confirmation: false,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            alpha_contrarian: false,
            cex_contrarian: false,
            probability_shrink: 1.0,
            probability_haircut: 0.0,
            market_prior_weight: 0.35,
            confirmation_logit_weight: 1.0,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            pre_settlement_exit_secs: 0,
            pre_settlement_min_exit_bid: 0.01,
            late_hold_ev_margin: None,
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
            min_entry_score: 0.30,
        }
    }

    #[test]
    fn direction_filter_allows_research_side_gates_without_changing_default() {
        let mut config = test_config();
        assert!(config.allows_direction("UP"));
        assert!(config.allows_direction("DOWN"));

        config.allowed_directions = vec!["UP".to_string()];
        assert!(config.allows_direction("up"));
        assert!(!config.allows_direction("DOWN"));
    }

    fn discover_test_event(strategy: &mut ThreeLayerStrategy, now: DateTime<Utc>) {
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(300),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );
    }

    fn position_with_token(token_id: &str, now: DateTime<Utc>) -> PositionLedger {
        let mut positions = PositionLedger::default();
        positions.apply_fill(&FillRecord {
            fill_id: format!("fill-{token_id}"),
            order_id: format!("order-{token_id}"),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            price: dec!(0.45),
            fee: Decimal::ZERO,
            timestamp: now,
        });
        positions
    }

    fn run_late_hold_exit_case<F>(
        now: DateTime<Utc>,
        configure: F,
        final_price: Decimal,
    ) -> Vec<StrategyDecision>
    where
        F: FnOnce(&mut ThreeLayerConfig),
    {
        let mut config = test_config();
        config.take_profit_ask = 0.99;
        configure(&mut config);
        let mut strategy = ThreeLayerStrategy::new(config);
        let positions = position_with_token("token-up", now);
        let orders = OrderLedger::default();

        for (offset_secs, price) in [
            (-24, dec!(100000)),
            (-18, dec!(100100)),
            (-12, dec!(99900)),
            (-6, dec!(100000)),
        ] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: Arc::from("BTCUSDT"),
                    price,
                    ts: now + chrono::Duration::seconds(offset_secs),
                },
                &positions,
                &orders,
            );
        }
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(30),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: final_price,
                ts: now,
            },
            &positions,
            &orders,
        );

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.70)),
                ask: Some(dec!(0.72)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                ts: now,
            },
            &positions,
            &orders,
        )
    }

    fn active_order_for_token(token_id: &str, now: DateTime<Utc>) -> OrderLedger {
        let mut orders = OrderLedger::default();
        let intent = TradingIntent {
            intent_id: format!("intent-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        };
        orders.insert_from_intent(format!("order-{token_id}"), &intent);
        orders.acknowledge(&format!("order-{token_id}"), format!("venue-{token_id}"));
        orders
    }

    fn take_profit_quote(token_id: &str, ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.72)),
            ask: Some(dec!(0.75)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            ts,
        }
    }

    fn entry_quote(token_id: &str, ts: DateTime<Utc>, ask_size: Option<Decimal>) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.19)),
            ask: Some(dec!(0.20)),
            bid_size: Some(dec!(100)),
            ask_size,
            ts,
        }
    }

    fn rejected_exit_intent(token_id: &str, now: DateTime<Utc>) -> TradingIntent {
        TradingIntent {
            intent_id: format!("reject-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        }
    }

    fn rejected_buy_intent(token_id: &str, now: DateTime<Utc>) -> TradingIntent {
        TradingIntent {
            intent_id: format!("buy-reject-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        }
    }

    #[test]
    fn reject_cooldown_suppresses_duplicate_live_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(30),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let first_decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);
        let exit_intent = match first_decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => intent.clone(),
            other => panic!("expected one take-profit exit, got {other:?}"),
        };

        strategy.on_reject(
            &exit_intent,
            "not enough balance / allowance: available balance is lower than order size",
        );
        let next_decisions = strategy.on_update(
            &take_profit_quote("token-down", now + chrono::Duration::seconds(1)),
            &positions,
            &orders,
        );

        assert!(
            next_decisions.is_empty(),
            "rejected token should not emit duplicate live exit"
        );
    }

    #[test]
    fn reject_cooldown_preserves_longer_existing_pause() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        strategy.feed_time = Some(now);
        let intent = rejected_exit_intent("token-down", now);

        strategy.on_reject(&intent, "invalid amounts");
        let first_token_until = strategy
            .token_reject_until
            .get("token-down")
            .copied()
            .expect("token cooldown");

        strategy.feed_time = Some(now + chrono::Duration::seconds(10));
        strategy.on_reject(&intent, "no orders found to match with FAK order");

        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            first_token_until
        );
        assert!(strategy.balance_exhausted_until.is_none());
    }

    #[test]
    fn reject_cooldown_classifies_observed_live_reasons() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let intent = rejected_exit_intent("token-down", now);

        strategy.feed_time = Some(now);
        strategy.on_reject(&intent, "invalid amounts");
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert!(strategy.balance_exhausted_until.is_none());

        strategy.token_reject_until.clear();
        strategy.feed_time = Some(now);
        strategy.on_reject(&intent, "no orders found to match with FAK order");
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(NO_LIQUIDITY_REJECT_PAUSE_SECS)
        );

        strategy.token_reject_until.clear();
        strategy.feed_time = Some(now);
        strategy.on_reject(
            &intent,
            "not enough allowance: allowance is lower than order size",
        );
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert!(strategy.balance_exhausted_until.is_none());
    }

    #[test]
    fn buy_balance_reject_uses_short_account_pause() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let intent = rejected_buy_intent("token-up", now);

        strategy.feed_time = Some(now);
        strategy.on_reject(
            &intent,
            "not enough balance: available balance is lower than order size",
        );

        assert_eq!(
            strategy
                .token_reject_until
                .get("token-up")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert_eq!(
            strategy.balance_exhausted_until.expect("balance pause"),
            now + chrono::Duration::seconds(ACCOUNT_BALANCE_REJECT_PAUSE_SECS)
        );
    }

    #[test]
    fn active_order_suppresses_duplicate_live_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = active_order_for_token("token-down", now);

        let decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);

        assert!(
            decisions.is_empty(),
            "active order should gate duplicate live exit"
        );
    }

    #[test]
    fn take_profit_requires_executable_bid_not_ask_only() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.62)),
                ask: Some(dec!(0.75)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "a high ask alone is not an executable take-profit sell"
        );
    }

    #[test]
    fn pre_settlement_exit_emits_before_event_expiry_when_enabled() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.take_profit_ask = 0.99;
        config.pre_settlement_exit_secs = 90;
        config.pre_settlement_min_exit_bid = 0.05;
        let mut strategy = ThreeLayerStrategy::new(config);
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(80),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );
        let positions = position_with_token("token-down", now);

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.12)),
                ask: Some(dec!(0.14)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                ts: now,
            },
            &positions,
            &OrderLedger::default(),
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert!(intent.intent_id.starts_with("tl_pre_settle_"));
                assert_eq!(intent.limit_price, Some(dec!(0.12)));
            }
            other => panic!("Expected one pre-settlement exit, got {:?}", other),
        }
    }

    #[test]
    fn late_hold_ev_exit_emits_only_when_enabled_and_adverse() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();

        assert!(
            run_late_hold_exit_case(now, |_| {}, dec!(98500)).is_empty(),
            "disabled late-hold EV margin must preserve existing behavior"
        );

        let decisions = run_late_hold_exit_case(
            now,
            |config| {
                config.late_hold_ev_margin = Some(0.0);
            },
            dec!(98500),
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert!(intent.intent_id.starts_with("tl_late_ev_"));
                assert_eq!(intent.limit_price, Some(dec!(0.70)));
            }
            other => panic!("Expected one late-hold EV exit, got {:?}", other),
        }
    }

    #[test]
    fn late_hold_ev_does_not_override_pre_settlement_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let decisions = run_late_hold_exit_case(
            now,
            |config| {
                config.late_hold_ev_margin = Some(0.0);
                config.pre_settlement_exit_secs = 60;
                config.pre_settlement_min_exit_bid = 0.05;
            },
            dec!(98500),
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert!(intent.intent_id.starts_with("tl_pre_settle_"));
                assert_eq!(intent.limit_price, Some(dec!(0.70)));
            }
            other => panic!("Expected pre-settlement priority, got {:?}", other),
        }
    }

    #[test]
    fn late_hold_ev_does_not_override_stop_loss_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let decisions = run_late_hold_exit_case(
            now,
            |config| {
                config.late_hold_ev_margin = Some(0.0);
            },
            dec!(97000),
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert!(intent.intent_id.starts_with("tl_sl_"));
                assert_eq!(intent.limit_price, Some(dec!(0.70)));
            }
            other => panic!("Expected stop-loss priority, got {:?}", other),
        }
    }

    #[test]
    fn take_profit_requires_bid_size_for_position_quantity() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.72)),
                ask: Some(dec!(0.75)),
                bid_size: Some(dec!(5)),
                ask_size: Some(dec!(100)),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "sell exits must not be emitted when top bid cannot fill the position"
        );
    }

    #[test]
    fn entry_requires_ask_size_for_fixed_stake() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(25))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not fire when ask size cannot fill the fixed stake"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_ask_size"), Some(&1));
    }

    #[test]
    fn entry_requires_known_ask_size() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(&entry_quote("token-up", now, None), &positions, &orders);
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not fire when quote size is unavailable"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_no_ask_size"), Some(&1));
    }

    #[test]
    fn entry_emits_when_ask_size_covers_fixed_stake() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, .. }] => {
                assert_eq!(intent.token_id, "token-up");
                assert_eq!(intent.side, TradeSide::Buy);
                assert_eq!(intent.limit_price, Some(dec!(0.20)));
                assert_eq!(intent.quantity, dec!(125));
            }
            other => panic!("expected one executable entry, got {other:?}"),
        }
    }

    #[test]
    fn entry_skips_inside_pre_settlement_exit_window_when_enabled() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.pre_settlement_exit_secs = 90;
        let mut strategy = ThreeLayerStrategy::new(config);
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(80),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not open inside the configured pre-settlement exit window"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_pre_settlement_entry"), Some(&1));
    }

    #[test]
    fn balance_pause_blocks_entries_but_not_take_profit_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        strategy.balance_exhausted_until = Some(now + chrono::Duration::seconds(15));
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);

        assert_eq!(
            decisions.len(),
            1,
            "risk exits must not wait for buy balance cooldown"
        );
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.limit_price, Some(dec!(0.72)));
            }
            other => panic!("expected take-profit exit, got {other:?}"),
        }
    }

    #[test]
    fn balance_pause_does_not_block_official_settlement_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        strategy.balance_exhausted_until = Some(now + chrono::Duration::seconds(15));
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::EventExpired {
                event_id: Arc::from("evt1"),
                end_time: now,
                resolved_up_won: Some(false),
            },
            &positions,
            &orders,
        );

        assert_eq!(
            decisions.len(),
            1,
            "settlement exits must ignore entry cooldowns"
        );
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.limit_price, Some(Decimal::new(1, 0)));
            }
            other => panic!("expected settlement exit, got {other:?}"),
        }
    }

    #[test]
    fn direction_rejects_weak_early_signal() {
        let config = test_config();
        let result = evaluate_direction_score(0.1, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_none(), "should reject weak direction signal");
    }

    #[test]
    fn direction_passes_strong_early_signal() {
        let config = test_config();
        let result = evaluate_direction_score(1.5, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_some(), "should pass strong early signal");
        let (dir, prob, score) = result.unwrap();
        assert!(dir > 0.0);
        assert!(prob > 0.56);
        assert!(
            score > 0.0,
            "direction score should be positive for strong signal"
        );
    }

    #[test]
    fn direction_rejects_when_raw_alpha_probability_is_neutral() {
        let config = test_config();

        let result = evaluate_direction_score(0.05, 0.02, 0.0, 0.0, Regime::Middle, &config);

        assert!(
            result.is_none(),
            "cheap executable odds must not override neutral directional alpha"
        );
    }

    #[test]
    fn direction_gate_allows_strong_alpha_to_reach_calibrated_ev() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.probability_shrink = 0.0;
        config.probability_haircut = 0.0;

        let result = evaluate_direction_score(2.0, 0.02, 0.0, 0.0, Regime::Early, &config);

        let (_dir, prob, score) =
            result.expect("strong raw directional alpha should pass the direction gate");
        assert!((prob - 0.50).abs() < 1e-9);
        assert!(
            score > 0.0,
            "direction score should remain based on alpha strength before EV calibration"
        );
    }

    #[test]
    fn alpha_contrarian_direction_fades_model_favored_side() {
        let mut config = test_config();
        config.alpha_contrarian = true;
        config.min_direction_prob = 0.56;

        let result = evaluate_direction_score(1.5, 0.02, 0.0, 0.0, Regime::Early, &config)
            .expect("contrarian mode should score instead of early-vetoing");
        let (dir, prob, score) = result;
        assert!(dir < 0.0, "positive distance should fade into DOWN");
        assert!(
            prob > 0.50,
            "contrarian effective probability should be transformed alpha confidence"
        );
        assert!(score > 0.0, "strong inverse alpha should be rewarded");
    }

    #[test]
    fn confirmation_returns_negative_for_opposing_lob() {
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
        let bonus = evaluate_confirmation_bonus(1.0, &lob, -5.0, -0.001, Regime::Late, &config);
        assert!(
            bonus < 0.0,
            "opposing LOB should produce negative bonus, got {}",
            bonus
        );
    }

    #[test]
    fn cex_contrarian_inverts_confirmation_bonus() {
        let mut config = test_config();
        config.cex_contrarian = true;
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
        let bonus = evaluate_confirmation_bonus(1.0, &lob, -5.0, -0.001, Regime::Late, &config);
        assert!(
            bonus > 0.0,
            "opposing LOB should be rewarded in contrarian confirmation mode, got {}",
            bonus
        );
    }

    #[test]
    fn champion_profile_ignores_confirmation_component() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        let total = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.50,
                distance_over_sigma: -0.20,
                direction_sign: -1.0,
                edge: 0.01,
                edge_score: 0.20,
                confirmation: -1.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );
        let with_positive_confirmation = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.50,
                distance_over_sigma: -0.20,
                direction_sign: -1.0,
                edge: 0.01,
                edge_score: 0.20,
                confirmation: 1.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );
        assert!((total - with_positive_confirmation).abs() < f64::EPSILON);
    }

    #[test]
    fn obi_soft_profile_scores_side_aware_book_imbalance() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::ObiSoft;
        let lob = LobState {
            obi: -0.5,
            obi_prev: -0.2,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,
            signed_trade_imbalance: -20.0,
            last_aggtrade_ts: None,
            ts: None,
        };
        let score = profile_confirmation_score(-1.0, &lob, -0.5, 0.0, Regime::Middle, &config);
        assert!(
            score > 0.0,
            "DOWN-side OBI profile should reward opposing raw UP pressure, got {score}"
        );
    }

    #[test]
    fn hard_confirmation_gate_rejects_weak_obi_when_required() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::ObiHard;
        config.require_confirmation = true;
        config.min_confirmation_score = 0.10;

        assert!(!confirmation_gate_passes(
            0.0,
            config.min_confirmation_score,
            config.cex_contrarian
        ));

        let mut soft_config = config.clone();
        soft_config.profile = ThreeLayerProfile::ObiSoft;
        soft_config.require_confirmation = false;
        let soft_score = evaluate_entry_score(
            &soft_config,
            EntryScoreInputs {
                direction_score: 0.80,
                distance_over_sigma: 0.40,
                direction_sign: 1.0,
                edge: 0.06,
                edge_score: 0.70,
                confirmation: 0.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );

        assert!(
            soft_score > soft_config.min_entry_score,
            "OBI-soft should still be able to score weak confirmation instead of hard rejecting"
        );
    }

    #[test]
    fn edge_score_returns_continuous_value() {
        let config = test_config();
        // ask=0.35 → fee≈0.00455, edge=0.55-0.35-0.00455≈0.195 → edge_score≈1.95 clamped to 1.0
        let result = evaluate_edge_score(0.55, 0.35, Regime::Early, &config);
        assert!(result.is_some());
        let (_ask, edge, _rr, edge_score) = result.unwrap();
        assert!(edge > 0.0);
        assert!(edge_score > 0.0 && edge_score <= 1.0);
        // ask=0.50 → fee=0.005, edge=0.52-0.50-0.005=0.015 → edge_score=0.15
        let result = evaluate_edge_score(0.52, 0.50, Regime::Early, &config);
        assert!(
            result.is_some(),
            "edge_score model should not reject low edge, just score it low"
        );
        let (_ask, _edge, _rr, edge_score) = result.unwrap();
        assert!(
            edge_score < 0.3,
            "low edge should produce low score, got {}",
            edge_score
        );
    }

    #[test]
    fn expectancy_rejects_high_probability_when_entry_is_too_rich() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.min_edge = 0.01;
        config.max_entry_price = 0.95;

        assert!(
            evaluate_edge_score(0.70, 0.75, Regime::Early, &config).is_none(),
            "direction probability alone is insufficient when executable entry price makes EV negative"
        );
        assert!(
            evaluate_edge_score(0.58, 0.35, Regime::Early, &config).is_some(),
            "lower direction probability can pass when executable price/payoff create positive EV"
        );
    }

    #[test]
    fn expectancy_prefers_better_price_even_with_lower_probability() {
        let high_probability_rich_entry = expected_value_per_staked_dollar(0.72, 0.70);
        let lower_probability_cheap_entry = expected_value_per_staked_dollar(0.57, 0.28);

        assert!(
            lower_probability_cheap_entry > high_probability_rich_entry,
            "EV should compare probability together with executable price, not probability alone"
        );
    }

    #[test]
    fn probability_calibration_shrinks_and_haircuts_overconfident_alpha() {
        let calibrated = calibrate_direction_probability(0.70, 0.50, 0.03);
        assert!((calibrated - 0.57).abs() < 1e-9);
        assert!(
            expected_value_per_share(calibrated, 0.60) < 0.0,
            "calibrated probability should prevent rich-entry EV overstatement"
        );
    }

    #[test]
    fn bayesian_execution_probability_blends_model_with_market_prior() {
        let mut config = test_config();
        config.market_prior_weight = 0.50;
        config.confirmation_logit_weight = 0.0;

        let posterior = bayesian_execution_probability(0.80, 0.55, 0.0, &config);

        assert!(posterior > 0.55);
        assert!(posterior < 0.80);
    }

    #[test]
    fn bayesian_execution_probability_uses_confirmation_as_evidence() {
        let mut config = test_config();
        config.market_prior_weight = 0.35;
        config.confirmation_logit_weight = 1.0;

        let neutral = bayesian_execution_probability(0.62, 0.58, 0.0, &config);
        let confirmed = bayesian_execution_probability(0.62, 0.58, 0.20, &config);
        let opposed = bayesian_execution_probability(0.62, 0.58, -0.20, &config);

        assert!(confirmed > neutral);
        assert!(opposed < neutral);
    }

    #[test]
    fn alpha_contrarian_edge_score_does_not_reward_negative_edge() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.alpha_contrarian = true;
        config.min_edge = -0.005;
        let result = evaluate_edge_score(0.35, 0.50, Regime::Early, &config);
        assert!(
            result.is_none(),
            "negative edge should be rejected for snapshot profiles even in contrarian mode"
        );
    }

    #[test]
    fn norm_cdf_basic_values() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 0.001);
        assert!(norm_cdf(2.0) > 0.97);
        assert!(norm_cdf(-2.0) < 0.03);
    }
}
