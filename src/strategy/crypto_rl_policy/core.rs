use chrono::{DateTime, Utc};
#[cfg(feature = "onnx")]
use chrono::{Datelike, Timelike};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

#[cfg(feature = "onnx")]
use crate::collector::LobSnapshot;
use crate::domain::Side;

#[cfg(feature = "onnx")]
pub const OBS_DIM_V1: usize = 25;
#[cfg(feature = "onnx")]
pub const OBS_DIM_V2: usize = 31;
#[cfg(feature = "onnx")]
pub const NUM_DISCRETE_ACTIONS: usize = 5;
#[cfg(feature = "onnx")]
pub const CONTINUOUS_ACTION_DIM: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscreteAction {
    Hold,
    BuyUp,
    BuyDown,
    SellPosition,
    EnterHedge,
}

impl DiscreteAction {
    #[cfg(feature = "onnx")]
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Hold),
            1 => Some(Self::BuyUp),
            2 => Some(Self::BuyDown),
            3 => Some(Self::SellPosition),
            4 => Some(Self::EnterHedge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ContinuousAction {
    pub position_delta: f32,
    pub side_preference: f32,
    pub urgency: f32,
    #[allow(dead_code)]
    pub tp_adjustment: f32,
    #[allow(dead_code)]
    pub sl_adjustment: f32,
}

impl Default for ContinuousAction {
    fn default() -> Self {
        Self {
            position_delta: 0.0,
            side_preference: 0.0,
            urgency: 0.5,
            tp_adjustment: 0.0,
            sl_adjustment: 0.0,
        }
    }
}

impl ContinuousAction {
    pub fn new(
        position_delta: f32,
        side_preference: f32,
        urgency: f32,
        tp_adjustment: f32,
        sl_adjustment: f32,
    ) -> Self {
        Self {
            position_delta: position_delta.clamp(-1.0, 1.0),
            side_preference: side_preference.clamp(-1.0, 1.0),
            urgency: urgency.clamp(0.0, 1.0),
            tp_adjustment: tp_adjustment.clamp(-1.0, 1.0),
            sl_adjustment: sl_adjustment.clamp(-1.0, 1.0),
        }
    }

    pub fn to_discrete(&self) -> DiscreteAction {
        if self.position_delta < -0.5 {
            return DiscreteAction::SellPosition;
        }
        if self.position_delta > 0.5 {
            if self.side_preference > 0.3 {
                return DiscreteAction::BuyUp;
            }
            if self.side_preference < -0.3 {
                return DiscreteAction::BuyDown;
            }
            return DiscreteAction::EnterHedge;
        }
        DiscreteAction::Hold
    }

    pub fn is_aggressive(&self) -> bool {
        self.urgency > 0.7
    }

    pub fn position_size_pct(&self) -> f32 {
        self.position_delta.abs().clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone)]
pub struct PositionLeg {
    pub token_id: String,
    pub side: Side,
    pub shares: u64,
    pub entry_price: Decimal,
    #[allow(dead_code)]
    pub entry_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TrackedPosition {
    #[allow(dead_code)]
    pub market_slug: String,
    pub symbol: String,
    pub legs: Vec<PositionLeg>,
}

pub fn deployment_id_for_symbol(symbol: &str) -> String {
    format!("crypto.pm.{}.rl_policy", symbol.trim().to_ascii_lowercase())
}

#[cfg(feature = "onnx")]
pub fn map_urgency(raw: f32) -> f32 {
    if !raw.is_finite() {
        return 0.5;
    }
    if (0.0..=1.0).contains(&raw) {
        return raw;
    }
    if (-1.0..=1.0).contains(&raw) {
        return (raw + 1.0) * 0.5;
    }
    1.0 / (1.0 + (-raw).exp())
}

#[cfg(feature = "onnx")]
pub fn argmax(values: &[f32]) -> Option<usize> {
    if values.is_empty() {
        return None;
    }
    let mut best_idx = 0usize;
    let mut best_val = values[0];
    for (i, &v) in values.iter().enumerate().skip(1) {
        if v > best_val {
            best_val = v;
            best_idx = i;
        }
    }
    Some(best_idx)
}

#[cfg(feature = "onnx")]
pub fn softmax(values: &[f32]) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut max = f32::NEG_INFINITY;
    for &v in values {
        if v.is_finite() && v > max {
            max = v;
        }
    }
    if !max.is_finite() {
        return vec![0.0; values.len()];
    }
    let mut exps = Vec::with_capacity(values.len());
    let mut sum = 0.0f32;
    for &v in values {
        let x = if v.is_finite() { (v - max).exp() } else { 0.0 };
        exps.push(x);
        sum += x;
    }
    if sum <= 0.0 {
        return vec![0.0; values.len()];
    }
    for v in &mut exps {
        *v /= sum;
    }
    exps
}

#[cfg(feature = "onnx")]
pub fn action_from_discrete(action: DiscreteAction) -> ContinuousAction {
    match action {
        DiscreteAction::Hold => ContinuousAction::default(),
        DiscreteAction::BuyUp => ContinuousAction::new(0.8, 1.0, 0.5, 0.0, 0.0),
        DiscreteAction::BuyDown => ContinuousAction::new(0.8, -1.0, 0.5, 0.0, 0.0),
        DiscreteAction::SellPosition => ContinuousAction::new(-0.8, 0.0, 0.8, 0.0, 0.0),
        DiscreteAction::EnterHedge => ContinuousAction::new(0.8, 0.0, 0.6, 0.0, 0.0),
    }
}

#[cfg(feature = "onnx")]
pub fn action_from_policy_output(policy_output: &str, output: &[f32]) -> Option<ContinuousAction> {
    match policy_output.trim().to_ascii_lowercase().as_str() {
        "continuous" => {
            if output.len() < CONTINUOUS_ACTION_DIM {
                return None;
            }
            let v = &output[..CONTINUOUS_ACTION_DIM];
            let urgency = map_urgency(v[2]);
            Some(ContinuousAction::new(v[0], v[1], urgency, v[3], v[4]))
        }
        "continuous_mean_logstd" | "mean_logstd" => {
            if output.len() < CONTINUOUS_ACTION_DIM * 2 {
                return None;
            }
            let mean = &output[..CONTINUOUS_ACTION_DIM];
            let urgency = map_urgency(mean[2]);
            Some(ContinuousAction::new(
                mean[0].tanh(),
                mean[1].tanh(),
                urgency,
                mean[3].tanh(),
                mean[4].tanh(),
            ))
        }
        "discrete_logits" | "discrete" => {
            if output.len() < NUM_DISCRETE_ACTIONS {
                return None;
            }
            let logits = &output[..NUM_DISCRETE_ACTIONS];
            let probs = softmax(logits);
            let idx = argmax(&probs)?;
            let act = DiscreteAction::from_index(idx)?;
            Some(action_from_discrete(act))
        }
        "discrete_probs" => {
            if output.len() < NUM_DISCRETE_ACTIONS {
                return None;
            }
            let probs = &output[..NUM_DISCRETE_ACTIONS];
            let idx = argmax(probs)?;
            let act = DiscreteAction::from_index(idx)?;
            Some(action_from_discrete(act))
        }
        _ => None,
    }
}

pub fn rule_based_policy(
    has_position: bool,
    sum_of_asks: Option<Decimal>,
    momentum_1s: Decimal,
    unrealized_pnl_pct: Option<Decimal>,
) -> ContinuousAction {
    if let Some(sum) = sum_of_asks {
        let sum_f32 = sum.to_f32().unwrap_or(1.0);

        if sum_f32 < 0.96 && !has_position {
            let side_pref = if momentum_1s > Decimal::ZERO {
                0.5
            } else if momentum_1s < Decimal::ZERO {
                -0.5
            } else {
                0.0
            };
            return ContinuousAction::new(0.7, side_pref, 0.5, 0.0, 0.0);
        }

        if sum_f32 > 1.0 && has_position {
            return ContinuousAction::new(-0.8, 0.0, 0.7, 0.0, 0.0);
        }

        if let Some(pnl) = unrealized_pnl_pct {
            let pnl_f32 = pnl.to_f32().unwrap_or(0.0);
            if pnl_f32 < -0.05 && has_position {
                return ContinuousAction::new(-1.0, 0.0, 1.0, 0.0, 0.0);
            }
        }
    }

    ContinuousAction::default()
}

#[cfg(feature = "onnx")]
pub fn time_features(now: DateTime<Utc>) -> (f32, f32, f32, f32) {
    use std::f32::consts::PI;

    let hour = now.hour() as f32;
    let day = now.weekday().num_days_from_monday() as f32;

    let hour_rad = 2.0 * PI * hour / 24.0;
    let day_rad = 2.0 * PI * day / 7.0;

    (hour_rad.sin(), hour_rad.cos(), day_rad.sin(), day_rad.cos())
}

#[cfg(feature = "onnx")]
#[allow(clippy::too_many_arguments)]
pub fn build_observation_v1(
    default_shares: u64,
    max_time_remaining_secs: u64,
    now: DateTime<Utc>,
    spot_price: Decimal,
    momentum_1s: Decimal,
    momentum_5s: Decimal,
    lob: &LobSnapshot,
    up_bid: Decimal,
    up_ask: Decimal,
    down_bid: Decimal,
    down_ask: Decimal,
    position: Option<&TrackedPosition>,
    time_remaining_secs: i64,
) -> Vec<f32> {
    let pm_sum = up_ask + down_ask;
    let pm_up_spread = up_ask - up_bid;
    let pm_down_spread = down_ask - down_bid;

    let (has_pos, pos_side, pos_shares_norm, entry_price, pnl_pct) = match position {
        Some(pos) if !pos.legs.is_empty() => {
            let leg = &pos.legs[0];
            let shares_norm = (leg.shares as f32) / (default_shares.max(1) as f32);
            let mark = match leg.side {
                Side::Up => up_bid,
                Side::Down => down_bid,
            };
            let pnl_pct = if leg.entry_price > Decimal::ZERO {
                (mark - leg.entry_price) / leg.entry_price
            } else {
                Decimal::ZERO
            };
            (
                1.0,
                match leg.side {
                    Side::Up => 1.0,
                    Side::Down => -1.0,
                },
                shares_norm,
                leg.entry_price,
                pnl_pct,
            )
        }
        _ => (0.0, 0.0, 0.0, Decimal::ZERO, Decimal::ZERO),
    };

    let time_remaining_norm = if max_time_remaining_secs > 0 {
        (time_remaining_secs.max(0) as f32) / (max_time_remaining_secs as f32)
    } else {
        0.0
    }
    .clamp(0.0, 1.0);

    let (hour_sin, hour_cos, day_sin, day_cos) = time_features(now);

    let mut obs = Vec::with_capacity(OBS_DIM_V1);
    obs.push(spot_price.to_f32().unwrap_or(0.0));
    obs.push(momentum_1s.to_f32().unwrap_or(0.0));
    obs.push(momentum_5s.to_f32().unwrap_or(0.0));
    obs.push(lob.spread_bps.to_f32().unwrap_or(0.0));
    obs.push(lob.obi_5.to_f32().unwrap_or(0.0));
    obs.push(lob.obi_10.to_f32().unwrap_or(0.0));
    obs.push(lob.bid_volume_5.to_f32().unwrap_or(0.0));
    obs.push(lob.ask_volume_5.to_f32().unwrap_or(0.0));
    obs.push(up_bid.to_f32().unwrap_or(0.0));
    obs.push(up_ask.to_f32().unwrap_or(0.0));
    obs.push(down_bid.to_f32().unwrap_or(0.0));
    obs.push(down_ask.to_f32().unwrap_or(0.0));
    obs.push(pm_sum.to_f32().unwrap_or(0.0));
    obs.push(pm_up_spread.to_f32().unwrap_or(0.0));
    obs.push(pm_down_spread.to_f32().unwrap_or(0.0));
    obs.push(has_pos);
    obs.push(pos_side);
    obs.push(pos_shares_norm);
    obs.push(entry_price.to_f32().unwrap_or(0.0));
    obs.push(pnl_pct.to_f32().unwrap_or(0.0));
    obs.push(time_remaining_norm);
    obs.push(hour_sin);
    obs.push(hour_cos);
    obs.push(day_sin);
    obs.push(day_cos);

    debug_assert_eq!(obs.len(), OBS_DIM_V1);
    obs
}

#[cfg(feature = "onnx")]
#[allow(clippy::too_many_arguments)]
pub fn build_observation_v2(
    default_shares: u64,
    max_time_remaining_secs: u64,
    now: DateTime<Utc>,
    spot_price: Decimal,
    momentum_1s: Decimal,
    momentum_5s: Decimal,
    lob: &LobSnapshot,
    up_bid: Decimal,
    up_ask: Decimal,
    down_bid: Decimal,
    down_ask: Decimal,
    position: Option<&TrackedPosition>,
    time_remaining_secs: i64,
    obi_1: Decimal,
    obi_2: Decimal,
    obi_3: Decimal,
    obi_20: Decimal,
) -> Vec<f32> {
    let mut obs = build_observation_v1(
        default_shares,
        max_time_remaining_secs,
        now,
        spot_price,
        momentum_1s,
        momentum_5s,
        lob,
        up_bid,
        up_ask,
        down_bid,
        down_ask,
        position,
        time_remaining_secs,
    );

    let obi_micro = obi_1 - lob.obi_5;
    let obi_slope = lob.obi_5 - obi_20;

    obs.push(obi_1.to_f32().unwrap_or(0.0));
    obs.push(obi_2.to_f32().unwrap_or(0.0));
    obs.push(obi_3.to_f32().unwrap_or(0.0));
    obs.push(obi_20.to_f32().unwrap_or(0.0));
    obs.push(obi_micro.to_f32().unwrap_or(0.0));
    obs.push(obi_slope.to_f32().unwrap_or(0.0));

    debug_assert_eq!(obs.len(), OBS_DIM_V2);
    obs
}

pub fn compute_shares(action: &ContinuousAction, fallback: u64) -> u64 {
    let base = fallback.max(1) as f32;
    let mult = action.position_size_pct();
    (base * mult).max(1.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn continuous_action_maps_to_expected_discrete_action() {
        assert_eq!(
            ContinuousAction::new(0.8, 1.0, 0.4, 0.0, 0.0).to_discrete(),
            DiscreteAction::BuyUp
        );
        assert_eq!(
            ContinuousAction::new(0.8, -1.0, 0.4, 0.0, 0.0).to_discrete(),
            DiscreteAction::BuyDown
        );
        assert_eq!(
            ContinuousAction::new(-0.8, 0.0, 0.8, 0.0, 0.0).to_discrete(),
            DiscreteAction::SellPosition
        );
    }

    #[test]
    fn compute_shares_scales_with_position_delta() {
        let action = ContinuousAction::new(0.5, 1.0, 0.5, 0.0, 0.0);
        assert_eq!(compute_shares(&action, 100), 50);
        assert_eq!(compute_shares(&ContinuousAction::default(), 100), 1);
    }

    #[test]
    fn deployment_id_for_symbol_normalizes_case() {
        assert_eq!(
            deployment_id_for_symbol("BTCUSDT"),
            "crypto.pm.btcusdt.rl_policy"
        );
    }

    #[test]
    fn rule_based_policy_sells_on_deep_loss() {
        let action = rule_based_policy(true, Some(dec!(0.98)), Decimal::ZERO, Some(dec!(-0.10)));
        assert_eq!(action.to_discrete(), DiscreteAction::SellPosition);
        assert!(action.is_aggressive());
    }
}
