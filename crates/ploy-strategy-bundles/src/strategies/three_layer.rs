//! Three-layer strategy config and regime classification.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{HashMap, VecDeque};
use crate::strategies::directional::DirectionalConfig;

/// Time-remaining regime for a binary option market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// 181..=300 seconds remaining.
    Early,
    /// 61..=180 seconds remaining.
    Middle,
    /// 6..=60 seconds remaining.
    Late,
    /// 0..=5 seconds remaining.
    Expiry,
}

impl Regime {
    pub fn from_secs(secs: i64) -> Self {
        match secs {
            181..=300 => Regime::Early,
            61..=180  => Regime::Middle,
            6..=60    => Regime::Late,
            _         => Regime::Expiry,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Regime::Early  => "early",
            Regime::Middle => "middle",
            Regime::Late   => "late",
            Regime::Expiry => "expiry",
        }
    }
}

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
    event_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
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
