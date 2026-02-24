//! CryptoRlPolicyAgent — pull-based RL policy agent for crypto UP/DOWN markets
//!
//! This agent is designed for 24/7 deployment:
//! - Pull-based loop with fixed decision cadence
//! - Uses Binance LOB (depth) features + Polymarket quotes
//! - Runs an ONNX policy model (preferred) to output actions:
//!   buy/sell/hold + position size
//!
//! ## Observation Schema (v1)
//! The default observation vector length is 25 and is intentionally stable.
//! Train/export your policy to accept this exact ordering.
//!
//! Index -> Feature
//!  0  spot_price
//!  1  momentum_1s
//!  2  momentum_5s
//!  3  lob_spread_bps
//!  4  lob_obi_5
//!  5  lob_obi_10
//!  6  lob_bid_volume_5
//!  7  lob_ask_volume_5
//!  8  pm_up_bid
//!  9  pm_up_ask
//!  10 pm_down_bid
//!  11 pm_down_ask
//!  12 pm_sum_of_asks
//!  13 pm_up_spread
//!  14 pm_down_spread
//!  15 has_position (0/1)
//!  16 position_side (UP=1, DOWN=-1, none=0)
//!  17 position_shares_norm (shares / default_shares)
//!  18 entry_price
//!  19 unrealized_pnl_pct
//!  20 time_remaining_norm (0..1)
//!  21 hour_sin
//!  22 hour_cos
//!  23 day_sin
//!  24 day_cos
//!
//! ## Observation Schema (v2)
//! v2 is: v1 (25 dims) + 6 additional Binance OBI features appended.
//!
//!  25 lob_obi_1
//!  26 lob_obi_2
//!  27 lob_obi_3
//!  28 lob_obi_20
//!  29 lob_obi_micro (obi_1 - obi_5)
//!  30 lob_obi_slope (obi_5 - obi_20)

use async_trait::async_trait;
use chrono::{DateTime, Utc};
#[cfg(feature = "onnx")]
use chrono::{Datelike, Timelike};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::adapters::{BinanceWebSocket, PolymarketWebSocket};
use crate::agents::{AgentContext, TradingAgent};
use crate::collector::LobCache;
#[cfg(feature = "onnx")]
use crate::collector::LobSnapshot;
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

#[cfg(feature = "onnx")]
const OBS_DIM_V1: usize = 25;
#[cfg(feature = "onnx")]
const OBS_DIM_V2: usize = 31;
#[cfg(feature = "onnx")]
const NUM_DISCRETE_ACTIONS: usize = 5;
#[cfg(feature = "onnx")]
const CONTINUOUS_ACTION_DIM: usize = 5;

fn default_policy_output() -> String {
    "continuous".to_string()
}

fn default_observation_version() -> u32 {
    2
}

/// Configuration for the CryptoRlPolicyAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRlPolicyConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,

    /// Refresh interval for Gamma event discovery (seconds)
    pub event_refresh_secs: u64,
    /// Minimum time remaining for selected event (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining for selected event (seconds)
    pub max_time_remaining_secs: u64,
    /// Prefer events closest to end (confirmatory mode)
    pub prefer_close_to_end: bool,

    pub default_shares: u64,

    /// Max ask price to pay for entry (YES/NO).
    pub max_entry_price: Decimal,

    /// Minimum seconds between actions per symbol (avoid thrash).
    pub cooldown_secs: u64,

    /// Reject LOB snapshots older than this age (seconds).
    pub max_lob_snapshot_age_secs: u64,

    /// How often to run the policy loop (milliseconds).
    pub decision_interval_ms: u64,

    /// Observation schema version (affects ONNX input dim).
    ///
    /// - 1: 25 dims (baseline)
    /// - 2: 31 dims (adds more Binance OBI levels + derived OBI features)
    #[serde(default = "default_observation_version")]
    pub observation_version: u32,

    /// Optional ONNX policy model path (recommended for production).
    #[serde(default)]
    pub policy_model_path: Option<String>,

    /// How to interpret the policy model output.
    ///
    /// Supported values:
    /// - "continuous" (default): expects >= 5 floats: position_delta, side_preference, urgency, tp_adjustment, sl_adjustment
    /// - "continuous_mean_logstd": expects >= 10 floats: mean(5) then log_std(5), uses mean only
    /// - "discrete_logits": expects 5 floats, logits for [Hold, BuyUp, BuyDown, SellPosition, EnterHedge]
    /// - "discrete_probs": expects 5 floats, probabilities for the same discrete actions
    #[serde(default = "default_policy_output")]
    pub policy_output: String,

    /// Optional policy model version label recorded in order metadata.
    #[serde(default)]
    pub policy_model_version: Option<String>,

    /// Exploration rate (0..1). Recommended 0 for production.
    #[serde(default)]
    pub exploration_rate: f32,

    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

impl Default for CryptoRlPolicyConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto_rl_policy".into(),
            name: "Crypto RL Policy".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            event_refresh_secs: 15,
            // Cover both 5m + 15m windows by default.
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            prefer_close_to_end: true,
            default_shares: 50,
            max_entry_price: dec!(0.70),
            cooldown_secs: 10,
            max_lob_snapshot_age_secs: 2,
            decision_interval_ms: 1000,
            observation_version: default_observation_version(),
            policy_model_path: None,
            policy_output: default_policy_output(),
            policy_model_version: None,
            exploration_rate: 0.0,
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscreteAction {
    Hold,
    BuyUp,
    BuyDown,
    SellPosition,
    EnterHedge,
}

impl DiscreteAction {
    #[cfg(feature = "onnx")]
    fn from_index(index: usize) -> Option<Self> {
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
struct ContinuousAction {
    position_delta: f32,
    side_preference: f32,
    urgency: f32,
    #[allow(dead_code)]
    tp_adjustment: f32,
    #[allow(dead_code)]
    sl_adjustment: f32,
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
    fn new(
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

    fn to_discrete(&self) -> DiscreteAction {
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

    fn is_aggressive(&self) -> bool {
        self.urgency > 0.7
    }

    fn position_size_pct(&self) -> f32 {
        self.position_delta.abs().clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone)]
struct PositionLeg {
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    #[allow(dead_code)]
    entry_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct TrackedPosition {
    market_slug: String,
    symbol: String,
    legs: Vec<PositionLeg>, // 1 or 2 (hedged)
}

fn deployment_id_for_symbol(symbol: &str) -> String {
    format!("crypto.pm.{}.rl_policy", symbol.trim().to_ascii_lowercase())
}

pub struct CryptoRlPolicyAgent {
    config: CryptoRlPolicyConfig,
    binance_ws: Arc<BinanceWebSocket>,
    pm_ws: Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
    lob_cache: LobCache,

    #[cfg(feature = "onnx")]
    policy_model: Option<OnnxModel>,
}

impl CryptoRlPolicyAgent {
    pub fn new(
        mut config: CryptoRlPolicyConfig,
        binance_ws: Arc<BinanceWebSocket>,
        pm_ws: Arc<PolymarketWebSocket>,
        event_matcher: Arc<EventMatcher>,
        lob_cache: LobCache,
    ) -> Self {
        let obs_version = match config.observation_version {
            1 => 1,
            2 => 2,
            other => {
                warn!(
                    agent = config.agent_id,
                    observation_version = other,
                    "unsupported observation_version; defaulting to v2"
                );
                2
            }
        };
        config.observation_version = obs_version;

        #[cfg(feature = "onnx")]
        let policy_model: Option<OnnxModel> = match config.policy_model_path.as_deref() {
            Some(path) if !path.trim().is_empty() => {
                let primary_version = obs_version;
                let primary_dim = if primary_version == 1 {
                    OBS_DIM_V1
                } else {
                    OBS_DIM_V2
                };

                match OnnxModel::load_for_vec_input(path, primary_dim) {
                    Ok(m) => {
                        info!(
                            agent = config.agent_id,
                            model_path = %path,
                            input_dim = m.input_dim(),
                            output_dim = m.output_dim(),
                            policy_output = %config.policy_output,
                            observation_version = primary_version,
                            "loaded crypto RL policy ONNX model"
                        );
                        Some(m)
                    }
                    Err(e_primary) => {
                        // Try the other observation schema before giving up.
                        let fallback_version = if primary_version == 1 { 2 } else { 1 };
                        let fallback_dim = if fallback_version == 1 {
                            OBS_DIM_V1
                        } else {
                            OBS_DIM_V2
                        };

                        match OnnxModel::load_for_vec_input(path, fallback_dim) {
                            Ok(m) => {
                                warn!(
                                    agent = config.agent_id,
                                    model_path = %path,
                                    observation_version_primary = primary_version,
                                    observation_version = fallback_version,
                                    "loaded crypto RL policy ONNX model using fallback observation schema"
                                );
                                config.observation_version = fallback_version;
                                Some(m)
                            }
                            Err(e_fallback) => {
                                warn!(
                                    agent = config.agent_id,
                                    model_path = %path,
                                    observation_version = primary_version,
                                    error = %e_primary,
                                    "failed to load crypto RL policy ONNX model (primary schema)"
                                );
                                warn!(
                                    agent = config.agent_id,
                                    model_path = %path,
                                    observation_version = fallback_version,
                                    error = %e_fallback,
                                    "failed to load crypto RL policy ONNX model (fallback schema)"
                                );
                                warn!(
                                    agent = config.agent_id,
                                    model_path = %path,
                                    "policy model not loaded; falling back to rule-based policy"
                                );
                                None
                            }
                        }
                    }
                }
            }
            _ => None,
        };

        #[cfg(not(feature = "onnx"))]
        if let Some(path) = config.policy_model_path.as_deref() {
            if !path.trim().is_empty() {
                warn!(
                    agent = config.agent_id,
                    model_path = %path,
                    "policy_model_path is set but binary is built without --features onnx; using rule-based policy"
                );
            }
        }

        Self {
            config,
            binance_ws,
            pm_ws,
            event_matcher,
            lob_cache,
            #[cfg(feature = "onnx")]
            policy_model,
        }
    }

    fn config_hash(&self) -> String {
        let payload = serde_json::to_vec(&self.config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(payload);
        format!("{:x}", hasher.finalize())
    }

    #[cfg(feature = "onnx")]
    fn map_urgency(raw: f32) -> f32 {
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
    fn argmax(values: &[f32]) -> Option<usize> {
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
    fn softmax(values: &[f32]) -> Vec<f32> {
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
    fn action_from_discrete(action: DiscreteAction) -> ContinuousAction {
        match action {
            DiscreteAction::Hold => ContinuousAction::default(),
            DiscreteAction::BuyUp => ContinuousAction::new(0.8, 1.0, 0.5, 0.0, 0.0),
            DiscreteAction::BuyDown => ContinuousAction::new(0.8, -1.0, 0.5, 0.0, 0.0),
            DiscreteAction::SellPosition => ContinuousAction::new(-0.8, 0.0, 0.8, 0.0, 0.0),
            DiscreteAction::EnterHedge => ContinuousAction::new(0.8, 0.0, 0.6, 0.0, 0.0),
        }
    }

    #[cfg(feature = "onnx")]
    fn action_from_policy_output(&self, output: &[f32]) -> Option<ContinuousAction> {
        let kind = self.config.policy_output.trim().to_ascii_lowercase();

        match kind.as_str() {
            "continuous" => {
                if output.len() < CONTINUOUS_ACTION_DIM {
                    return None;
                }
                let v = &output[..CONTINUOUS_ACTION_DIM];
                let urgency = Self::map_urgency(v[2]);
                Some(ContinuousAction::new(v[0], v[1], urgency, v[3], v[4]))
            }
            "continuous_mean_logstd" | "mean_logstd" => {
                if output.len() < CONTINUOUS_ACTION_DIM * 2 {
                    return None;
                }
                let mean = &output[..CONTINUOUS_ACTION_DIM];
                let urgency = Self::map_urgency(mean[2]);
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
                let probs = Self::softmax(logits);
                let idx = Self::argmax(&probs)?;
                let act = DiscreteAction::from_index(idx)?;
                Some(Self::action_from_discrete(act))
            }
            "discrete_probs" => {
                if output.len() < NUM_DISCRETE_ACTIONS {
                    return None;
                }
                let probs = &output[..NUM_DISCRETE_ACTIONS];
                let idx = Self::argmax(probs)?;
                let act = DiscreteAction::from_index(idx)?;
                Some(Self::action_from_discrete(act))
            }
            _ => None,
        }
    }

    fn rule_based_policy(
        &self,
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
    fn time_features(now: DateTime<Utc>) -> (f32, f32, f32, f32) {
        use std::f32::consts::PI;
        let hour = now.hour() as f32;
        let day = now.weekday().num_days_from_monday() as f32;

        let hour_rad = 2.0 * PI * hour / 24.0;
        let day_rad = 2.0 * PI * day / 7.0;

        (hour_rad.sin(), hour_rad.cos(), day_rad.sin(), day_rad.cos())
    }

    #[cfg(feature = "onnx")]
    fn build_observation_v1(
        &self,
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
                let shares_norm = (leg.shares as f32) / (self.config.default_shares.max(1) as f32);
                // Use mid of the held leg side for unrealized PnL proxy (best bid is used elsewhere for exits).
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

        let time_remaining_norm = if self.config.max_time_remaining_secs > 0 {
            (time_remaining_secs.max(0) as f32) / (self.config.max_time_remaining_secs as f32)
        } else {
            0.0
        }
        .clamp(0.0, 1.0);

        let (hour_sin, hour_cos, day_sin, day_cos) = Self::time_features(now);

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
    fn build_observation_v2(
        &self,
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
        let mut obs = self.build_observation_v1(
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

    fn compute_shares(&self, action: &ContinuousAction, fallback: u64) -> u64 {
        let base = fallback.max(1) as f32;
        let mult = action.position_size_pct();
        (base * mult).max(1.0) as u64
    }
}

#[async_trait]
impl TradingAgent for CryptoRlPolicyAgent {
    fn id(&self) -> &str {
        &self.config.agent_id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn domain(&self) -> Domain {
        Domain::Crypto
    }

    fn risk_params(&self) -> AgentRiskParams {
        self.config.risk_params.clone()
    }

    async fn run(self, mut ctx: AgentContext) -> Result<()> {
        info!(
            agent = self.config.agent_id,
            "crypto RL policy agent starting"
        );
        let config_hash = self.config_hash();

        let mut status = AgentStatus::Running;
        let mut positions: HashMap<String, TrackedPosition> = HashMap::new(); // slug -> pos
        let mut active_events: HashMap<String, EventInfo> = HashMap::new(); // symbol -> event
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut last_action_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();

        let daily_pnl = Decimal::ZERO;
        let mut total_exposure = Decimal::ZERO;

        let refresh_dur = tokio::time::Duration::from_secs(self.config.event_refresh_secs.max(1));
        let mut refresh_tick = tokio::time::interval(refresh_dur);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let decision_dur =
            tokio::time::Duration::from_millis(self.config.decision_interval_ms.max(50));
        let mut decision_tick = tokio::time::interval(decision_dur);
        decision_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let heartbeat_dur =
            tokio::time::Duration::from_secs(self.config.heartbeat_interval_secs.max(1));
        let mut heartbeat_tick = tokio::time::interval(heartbeat_dur);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                // --- Refresh event discovery (Gamma) ---
                _ = refresh_tick.tick() => {
                    if let Err(e) = self.event_matcher.refresh().await {
                        warn!(agent = self.config.agent_id, error = %e, "event refresh failed");
                        continue;
                    }

                    let mut refreshed_events: HashMap<String, EventInfo> = HashMap::new();
                    for coin in &self.config.coins {
                        let symbol = format!("{}USDT", coin.to_uppercase());
                        let ev = self.event_matcher.find_event_with_timing(
                            &symbol,
                            self.config.min_time_remaining_secs,
                            self.config.max_time_remaining_secs as i64,
                            self.config.prefer_close_to_end,
                        ).await;

                        if let Some(event) = ev {
                            refreshed_events.insert(symbol, event);
                        }
                    }
                    active_events = refreshed_events;

                    // Ensure we are subscribed to the latest token set.
                    let mut desired_tokens: HashSet<String> = HashSet::new();
                    for event in active_events.values() {
                        desired_tokens.insert(event.up_token_id.clone());
                        desired_tokens.insert(event.down_token_id.clone());
                    }

                    if desired_tokens != subscribed_tokens {
                        for event in active_events.values() {
                            self.pm_ws
                                .register_tokens(&event.up_token_id, &event.down_token_id)
                                .await;
                        }
                        self.pm_ws.request_resubscribe();
                        info!(
                            agent = self.config.agent_id,
                            token_count = desired_tokens.len(),
                            "updated PM subscription token set"
                        );
                        subscribed_tokens = desired_tokens;
                    }
                }

                // --- Decision tick (policy) ---
                _ = decision_tick.tick() => {
                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }

                    let now = Utc::now();
                    let quote_cache = self.pm_ws.quote_cache();
                    let spot_cache = self.binance_ws.price_cache();

                    for coin in &self.config.coins {
                        let symbol = format!("{}USDT", coin.to_uppercase());
                        let Some(event) = active_events.get(&symbol) else {
                            continue;
                        };

                        // Cooldown per symbol.
                        if let Some(last) = last_action_by_symbol.get(&symbol) {
                            if now.signed_duration_since(*last).num_seconds() < self.config.cooldown_secs as i64 {
                                continue;
                            }
                        }

                        // Spot price + momentum.
                        let spot = match spot_cache.get(&symbol).await {
                            Some(s) => s,
                            None => continue,
                        };
                        let momentum_1s = spot_cache.momentum(&symbol, 1).await.unwrap_or(Decimal::ZERO);
                        let momentum_5s = spot_cache.momentum(&symbol, 5).await.unwrap_or(Decimal::ZERO);

                        // PM quotes.
                        let up = quote_cache.get(&event.up_token_id);
                        let down = quote_cache.get(&event.down_token_id);
                        let (up_bid, up_ask, down_bid, down_ask) = match (up, down) {
                            (Some(uq), Some(dq)) => (
                                uq.best_bid.unwrap_or(Decimal::ZERO),
                                uq.best_ask.unwrap_or(Decimal::ZERO),
                                dq.best_bid.unwrap_or(Decimal::ZERO),
                                dq.best_ask.unwrap_or(Decimal::ZERO),
                            ),
                            _ => continue,
                        };
                        if up_ask <= Decimal::ZERO || down_ask <= Decimal::ZERO {
                            continue;
                        }

                        // LOB snapshot.
                        let lob = match self.lob_cache.get_snapshot(&symbol).await {
                            Some(s) => s,
                            None => continue,
                        };
                        let age_secs = now.signed_duration_since(lob.timestamp).num_seconds();
                        if age_secs > self.config.max_lob_snapshot_age_secs as i64 {
                            continue;
                        }

                        let pos = positions.get(&event.slug);
                        let has_pos = pos.is_some();
                        let time_remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
                        let time_remaining_norm = if self.config.max_time_remaining_secs > 0 {
                            (time_remaining_secs.max(0) as f32)
                                / (self.config.max_time_remaining_secs as f32)
                        } else {
                            0.0
                        }
                        .clamp(0.0, 1.0);

                        let obs_version = self.config.observation_version;
                        let (obi_1, obi_2, obi_3, obi_20) = if obs_version == 2 {
                            (
                                self.lob_cache
                                    .get_obi(&symbol, 1)
                                    .await
                                    .unwrap_or(Decimal::ZERO),
                                self.lob_cache
                                    .get_obi(&symbol, 2)
                                    .await
                                    .unwrap_or(Decimal::ZERO),
                                self.lob_cache
                                    .get_obi(&symbol, 3)
                                    .await
                                    .unwrap_or(Decimal::ZERO),
                                self.lob_cache
                                    .get_obi(&symbol, 20)
                                    .await
                                    .unwrap_or(Decimal::ZERO),
                            )
                        } else {
                            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
                        };

                        // --- Policy inference ---
                        #[cfg(feature = "onnx")]
                        let mut policy_source = "rule_based";
                        #[cfg(not(feature = "onnx"))]
                        let policy_source = "rule_based";

                        #[cfg(feature = "onnx")]
                        let mut raw_output: Option<Vec<f32>> = None;

                        #[cfg(feature = "onnx")]
                        let obs = if obs_version == 2 {
                            self.build_observation_v2(
                                now,
                                spot.price,
                                momentum_1s,
                                momentum_5s,
                                &lob,
                                up_bid,
                                up_ask,
                                down_bid,
                                down_ask,
                                pos,
                                time_remaining_secs,
                                obi_1,
                                obi_2,
                                obi_3,
                                obi_20,
                            )
                        } else {
                            self.build_observation_v1(
                                now,
                                spot.price,
                                momentum_1s,
                                momentum_5s,
                                &lob,
                                up_bid,
                                up_ask,
                                down_bid,
                                down_ask,
                                pos,
                                time_remaining_secs,
                            )
                        };

                        #[cfg(feature = "onnx")]
                        if let Some(model) = &self.policy_model {
                            let out = model.predict(&obs).ok();
                            if let Some(ref o) = out {
                                raw_output = Some(o.clone());
                            }
                            if let Some(o) = out.as_deref() {
                                if let Some(act) = self.action_from_policy_output(o) {
                                    policy_source = "onnx";
                                    // Exploration: override the action.
                                    let mut action = act;
                                    if self.config.exploration_rate > 0.0 && rand::random::<f32>() < self.config.exploration_rate {
                                        action = ContinuousAction::new(
                                            rand::random::<f32>() * 2.0 - 1.0,
                                            rand::random::<f32>() * 2.0 - 1.0,
                                            rand::random::<f32>(),
                                            0.0,
                                            0.0,
                                        );
                                        policy_source = "explore";
                                    }

                                    // Execute action.
                                    let discrete = action.to_discrete();
                                    let priority = if action.is_aggressive() { OrderPriority::High } else { OrderPriority::Normal };
                                    let policy_version = self.config.policy_model_version.as_deref().unwrap_or("");
                                    let deployment_id = deployment_id_for_symbol(symbol);

                                    match discrete {
                                        DiscreteAction::Hold => {}

                                        DiscreteAction::BuyUp => {
                                            if has_pos {
                                                continue;
                                            }
                                            if up_ask > self.config.max_entry_price {
                                                continue;
                                            }

                                            let shares = self.compute_shares(&action, self.config.default_shares);
                                            let mut intent = OrderIntent::new(
                                                &self.config.agent_id,
                                                Domain::Crypto,
                                                event.slug.as_str(),
                                                event.up_token_id.as_str(),
                                                Side::Up,
                                                true,
                                                shares,
                                                up_ask,
                                            )
                                            .with_priority(priority)
                                            .with_metadata("strategy", "crypto_rl_policy")
                                            .with_deployment_id(deployment_id.as_str())
                                            .with_metadata("signal_type", "crypto_rl_policy")
                                            .with_metadata("action", "buy_up")
                                            .with_metadata("coin", coin.as_str())
                                            .with_metadata("symbol", symbol.as_str())
                                            .with_metadata("spot_price", spot.price.to_string())
                                            .with_metadata(
                                                "event_time_remaining_secs",
                                                time_remaining_secs.to_string(),
                                            )
                                            .with_metadata(
                                                "event_time_remaining_norm",
                                                format!("{time_remaining_norm:.6}"),
                                            )
                                            .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                            .with_metadata("lob_age_secs", age_secs.to_string())
                                            .with_condition_id(event.condition_id.as_str())
                                            .with_metadata("event_end_time", event.end_time.to_rfc3339())
                                            .with_metadata("event_title", event.title.as_str())
                                            .with_metadata("policy_source", policy_source)
                                            .with_metadata("policy_output", self.config.policy_output.as_str())
                                            .with_metadata("policy_model_version", policy_version)
                                            .with_metadata("obs_version", obs_version.to_string())
                                            .with_metadata("pm_up_bid", up_bid.to_string())
                                            .with_metadata("pm_up_ask", up_ask.to_string())
                                            .with_metadata("pm_down_bid", down_bid.to_string())
                                            .with_metadata("pm_down_ask", down_ask.to_string())
                                            .with_metadata("pm_sum_of_asks", (up_ask + down_ask).to_string())
                                            .with_metadata("lob_spread_bps", lob.spread_bps.to_string())
                                            .with_metadata("lob_obi_5", lob.obi_5.to_string())
                                            .with_metadata("lob_obi_10", lob.obi_10.to_string())
                                            .with_metadata("lob_bid_volume_5", lob.bid_volume_5.to_string())
                                            .with_metadata("lob_ask_volume_5", lob.ask_volume_5.to_string())
                                            .with_metadata("signal_momentum_1s", momentum_1s.to_string())
                                            .with_metadata("signal_momentum_5s", momentum_5s.to_string())
                                            .with_metadata("config_hash", config_hash.clone());

                                            if obs_version == 2 {
                                                intent.metadata.insert("lob_obi_1".into(), obi_1.to_string());
                                                intent.metadata.insert("lob_obi_2".into(), obi_2.to_string());
                                                intent.metadata.insert("lob_obi_3".into(), obi_3.to_string());
                                                intent.metadata.insert("lob_obi_20".into(), obi_20.to_string());
                                                intent.metadata.insert("lob_obi_micro".into(), (obi_1 - lob.obi_5).to_string());
                                                intent.metadata.insert("lob_obi_slope".into(), (lob.obi_5 - obi_20).to_string());
                                            }

                                            if let Some(ref o) = raw_output {
                                                if let Some(v0) = o.get(0) { intent.metadata.insert("policy_out_0".into(), format!("{v0:.6}")); }
                                                if let Some(v1) = o.get(1) { intent.metadata.insert("policy_out_1".into(), format!("{v1:.6}")); }
                                                if let Some(v2) = o.get(2) { intent.metadata.insert("policy_out_2".into(), format!("{v2:.6}")); }
                                            }

                                            if let Err(e) = ctx.submit_order(intent).await {
                                                warn!(agent = self.config.agent_id, error = %e, "failed to submit RL buy_up order");
                                                continue;
                                            }

                                            // Track position locally (single-leg).
                                            positions.insert(event.slug.clone(), TrackedPosition {
                                                market_slug: event.slug.clone(),
                                                symbol: symbol.clone(),
                                                legs: vec![PositionLeg {
                                                    token_id: event.up_token_id.clone(),
                                                    side: Side::Up,
                                                    shares,
                                                    entry_price: up_ask,
                                                    entry_time: now,
                                                }],
                                            });
                                            last_action_by_symbol.insert(symbol.clone(), now);
                                        }

                                        DiscreteAction::BuyDown => {
                                            if has_pos {
                                                continue;
                                            }
                                            if down_ask > self.config.max_entry_price {
                                                continue;
                                            }

                                            let shares = self.compute_shares(&action, self.config.default_shares);
                                            let mut intent = OrderIntent::new(
                                                &self.config.agent_id,
                                                Domain::Crypto,
                                                event.slug.as_str(),
                                                event.down_token_id.as_str(),
                                                Side::Down,
                                                true,
                                                shares,
                                                down_ask,
                                            )
                                            .with_priority(priority)
                                            .with_metadata("strategy", "crypto_rl_policy")
                                            .with_deployment_id(deployment_id.as_str())
                                            .with_metadata("signal_type", "crypto_rl_policy")
                                            .with_metadata("action", "buy_down")
                                            .with_metadata("coin", coin.as_str())
                                            .with_metadata("symbol", symbol.as_str())
                                            .with_metadata("spot_price", spot.price.to_string())
                                            .with_metadata(
                                                "event_time_remaining_secs",
                                                time_remaining_secs.to_string(),
                                            )
                                            .with_metadata(
                                                "event_time_remaining_norm",
                                                format!("{time_remaining_norm:.6}"),
                                            )
                                            .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                            .with_metadata("lob_age_secs", age_secs.to_string())
                                            .with_condition_id(event.condition_id.as_str())
                                            .with_metadata("event_end_time", event.end_time.to_rfc3339())
                                            .with_metadata("event_title", event.title.as_str())
                                            .with_metadata("policy_source", policy_source)
                                            .with_metadata("policy_output", self.config.policy_output.as_str())
                                            .with_metadata("policy_model_version", policy_version)
                                            .with_metadata("obs_version", obs_version.to_string())
                                            .with_metadata("pm_up_bid", up_bid.to_string())
                                            .with_metadata("pm_up_ask", up_ask.to_string())
                                            .with_metadata("pm_down_bid", down_bid.to_string())
                                            .with_metadata("pm_down_ask", down_ask.to_string())
                                            .with_metadata("pm_sum_of_asks", (up_ask + down_ask).to_string())
                                            .with_metadata("lob_spread_bps", lob.spread_bps.to_string())
                                            .with_metadata("lob_obi_5", lob.obi_5.to_string())
                                            .with_metadata("lob_obi_10", lob.obi_10.to_string())
                                            .with_metadata("lob_bid_volume_5", lob.bid_volume_5.to_string())
                                            .with_metadata("lob_ask_volume_5", lob.ask_volume_5.to_string())
                                            .with_metadata("signal_momentum_1s", momentum_1s.to_string())
                                            .with_metadata("signal_momentum_5s", momentum_5s.to_string())
                                            .with_metadata("config_hash", config_hash.clone());

                                            if obs_version == 2 {
                                                intent.metadata.insert("lob_obi_1".into(), obi_1.to_string());
                                                intent.metadata.insert("lob_obi_2".into(), obi_2.to_string());
                                                intent.metadata.insert("lob_obi_3".into(), obi_3.to_string());
                                                intent.metadata.insert("lob_obi_20".into(), obi_20.to_string());
                                                intent.metadata.insert("lob_obi_micro".into(), (obi_1 - lob.obi_5).to_string());
                                                intent.metadata.insert("lob_obi_slope".into(), (lob.obi_5 - obi_20).to_string());
                                            }

                                            if let Some(ref o) = raw_output {
                                                if let Some(v0) = o.get(0) { intent.metadata.insert("policy_out_0".into(), format!("{v0:.6}")); }
                                                if let Some(v1) = o.get(1) { intent.metadata.insert("policy_out_1".into(), format!("{v1:.6}")); }
                                                if let Some(v2) = o.get(2) { intent.metadata.insert("policy_out_2".into(), format!("{v2:.6}")); }
                                            }

                                            if let Err(e) = ctx.submit_order(intent).await {
                                                warn!(agent = self.config.agent_id, error = %e, "failed to submit RL buy_down order");
                                                continue;
                                            }

                                            positions.insert(event.slug.clone(), TrackedPosition {
                                                market_slug: event.slug.clone(),
                                                symbol: symbol.clone(),
                                                legs: vec![PositionLeg {
                                                    token_id: event.down_token_id.clone(),
                                                    side: Side::Down,
                                                    shares,
                                                    entry_price: down_ask,
                                                    entry_time: now,
                                                }],
                                            });
                                            last_action_by_symbol.insert(symbol.clone(), now);
                                        }

                                        DiscreteAction::SellPosition => {
                                            let Some(pos) = positions.get(&event.slug).cloned() else { continue };
                                            if pos.legs.is_empty() { continue; }

                                            let mut ok = true;
                                            for leg in &pos.legs {
                                                let bid = match leg.side {
                                                    Side::Up => up_bid,
                                                    Side::Down => down_bid,
                                                };
                                                if bid <= Decimal::ZERO {
                                                    ok = false;
                                                    continue;
                                                }

                                                let intent = OrderIntent::new(
                                                    &self.config.agent_id,
                                                    Domain::Crypto,
                                                    event.slug.as_str(),
                                                    leg.token_id.as_str(),
                                                    leg.side,
                                                    false,
                                                    leg.shares,
                                                    bid,
                                                )
                                                .with_priority(OrderPriority::High)
                                                .with_metadata("strategy", "crypto_rl_policy")
                                                .with_deployment_id(deployment_id.as_str())
                                                .with_metadata("signal_type", "crypto_rl_policy")
                                                .with_metadata("action", "sell")
                                                .with_metadata("coin", coin.as_str())
                                                .with_metadata("symbol", symbol.as_str())
                                                .with_metadata("spot_price", spot.price.to_string())
                                                .with_metadata(
                                                    "event_time_remaining_secs",
                                                    time_remaining_secs.to_string(),
                                                )
                                                .with_metadata(
                                                    "event_time_remaining_norm",
                                                    format!("{time_remaining_norm:.6}"),
                                                )
                                                .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                                .with_metadata("lob_age_secs", age_secs.to_string())
                                                .with_condition_id(event.condition_id.as_str())
                                                .with_metadata("policy_source", policy_source)
                                                .with_metadata("policy_output", self.config.policy_output.as_str())
                                                .with_metadata("policy_model_version", policy_version)
                                                .with_metadata("config_hash", config_hash.clone());

                                                if let Err(e) = ctx.submit_order(intent).await {
                                                    ok = false;
                                                    warn!(
                                                        agent = self.config.agent_id,
                                                        slug = %event.slug,
                                                        error = %e,
                                                        "failed to submit RL sell order"
                                                    );
                                                }
                                            }

                                            if ok {
                                                positions.remove(&event.slug);
                                                last_action_by_symbol.insert(symbol.clone(), now);
                                            }
                                        }

                                        DiscreteAction::EnterHedge => {
                                            // If already hedged, do nothing.
                                            if let Some(pos) = positions.get(&event.slug) {
                                                if pos.legs.len() >= 2 {
                                                    continue;
                                                }
                                            }

                                            // If we have an existing single-leg position, hedge with the opposite side using the same share count.
                                            if let Some(pos) = positions.get(&event.slug).cloned() {
                                                let Some(existing_leg) = pos.legs.get(0) else { continue };
                                                let (other_side, other_token_id, other_ask) = match existing_leg.side {
                                                    Side::Up => (Side::Down, event.down_token_id.as_str(), down_ask),
                                                    Side::Down => (Side::Up, event.up_token_id.as_str(), up_ask),
                                                };
                                                if other_ask <= Decimal::ZERO || other_ask > self.config.max_entry_price {
                                                    continue;
                                                }
                                                let total_cost = existing_leg.entry_price + other_ask;
                                                if total_cost >= dec!(1.0) {
                                                    continue;
                                                }

                                                let intent = OrderIntent::new(
                                                    &self.config.agent_id,
                                                    Domain::Crypto,
                                                    event.slug.as_str(),
                                                    other_token_id,
                                                    other_side,
                                                    true,
                                                    existing_leg.shares,
                                                    other_ask,
                                                )
                                                .with_priority(OrderPriority::High)
                                                .with_metadata("strategy", "crypto_rl_policy")
                                                .with_deployment_id(deployment_id.as_str())
                                                .with_metadata("signal_type", "crypto_rl_policy")
                                                .with_metadata("action", "hedge_complete")
                                                .with_metadata("coin", coin.as_str())
                                                .with_metadata("symbol", symbol.as_str())
                                                .with_metadata("spot_price", spot.price.to_string())
                                                .with_metadata(
                                                    "event_time_remaining_secs",
                                                    time_remaining_secs.to_string(),
                                                )
                                                .with_metadata(
                                                    "event_time_remaining_norm",
                                                    format!("{time_remaining_norm:.6}"),
                                                )
                                                .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                                .with_metadata("lob_age_secs", age_secs.to_string())
                                                .with_condition_id(event.condition_id.as_str())
                                                .with_metadata("policy_source", policy_source)
                                                .with_metadata("policy_model_version", policy_version)
                                                .with_metadata("locked_profit_per_share", (dec!(1.0) - total_cost).to_string())
                                                .with_metadata("config_hash", config_hash.clone());

                                                if let Err(e) = ctx.submit_order(intent).await {
                                                    warn!(agent = self.config.agent_id, error = %e, "failed to submit hedge completion leg");
                                                    continue;
                                                }

                                                let mut new_pos = pos.clone();
                                                new_pos.legs.push(PositionLeg {
                                                    token_id: other_token_id.to_string(),
                                                    side: other_side,
                                                    shares: existing_leg.shares,
                                                    entry_price: other_ask,
                                                    entry_time: now,
                                                });
                                                positions.insert(event.slug.clone(), new_pos);
                                                last_action_by_symbol.insert(symbol.clone(), now);
                                                continue;
                                            }

                                            // No position: enter a full hedge (buy both sides) if sum < 1.
                                            let sum_cost = up_ask + down_ask;
                                            if sum_cost >= dec!(1.0) {
                                                continue;
                                            }
                                            if up_ask > self.config.max_entry_price || down_ask > self.config.max_entry_price {
                                                continue;
                                            }

                                            let shares = self.compute_shares(&action, self.config.default_shares);
                                            let intent_up = OrderIntent::new(
                                                &self.config.agent_id,
                                                Domain::Crypto,
                                                event.slug.as_str(),
                                                event.up_token_id.as_str(),
                                                Side::Up,
                                                true,
                                                shares,
                                                up_ask,
                                            )
                                            .with_priority(OrderPriority::High)
                                            .with_metadata("strategy", "crypto_rl_policy")
                                            .with_deployment_id(deployment_id.as_str())
                                            .with_metadata("signal_type", "crypto_rl_policy")
                                            .with_metadata("action", "hedge_buy_up")
                                            .with_metadata("coin", coin.as_str())
                                            .with_metadata("symbol", symbol.as_str())
                                            .with_metadata("spot_price", spot.price.to_string())
                                            .with_metadata(
                                                "event_time_remaining_secs",
                                                time_remaining_secs.to_string(),
                                            )
                                            .with_metadata(
                                                "event_time_remaining_norm",
                                                format!("{time_remaining_norm:.6}"),
                                            )
                                            .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                            .with_metadata("lob_age_secs", age_secs.to_string())
                                            .with_condition_id(event.condition_id.as_str())
                                            .with_metadata("policy_source", policy_source)
                                            .with_metadata("policy_model_version", policy_version)
                                            .with_metadata("locked_profit_per_share", (dec!(1.0) - sum_cost).to_string())
                                            .with_metadata("config_hash", config_hash.clone());

                                            let intent_down = OrderIntent::new(
                                                &self.config.agent_id,
                                                Domain::Crypto,
                                                event.slug.as_str(),
                                                event.down_token_id.as_str(),
                                                Side::Down,
                                                true,
                                                shares,
                                                down_ask,
                                            )
                                            .with_priority(OrderPriority::High)
                                            .with_metadata("strategy", "crypto_rl_policy")
                                            .with_deployment_id(deployment_id.as_str())
                                            .with_metadata("signal_type", "crypto_rl_policy")
                                            .with_metadata("action", "hedge_buy_down")
                                            .with_metadata("coin", coin.as_str())
                                            .with_metadata("symbol", symbol.as_str())
                                            .with_metadata("spot_price", spot.price.to_string())
                                            .with_metadata(
                                                "event_time_remaining_secs",
                                                time_remaining_secs.to_string(),
                                            )
                                            .with_metadata(
                                                "event_time_remaining_norm",
                                                format!("{time_remaining_norm:.6}"),
                                            )
                                            .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                            .with_metadata("lob_age_secs", age_secs.to_string())
                                            .with_condition_id(event.condition_id.as_str())
                                            .with_metadata("policy_source", policy_source)
                                            .with_metadata("policy_model_version", policy_version)
                                            .with_metadata("locked_profit_per_share", (dec!(1.0) - sum_cost).to_string())
                                            .with_metadata("config_hash", config_hash.clone());

                                            let mut ok = true;
                                            if let Err(e) = ctx.submit_order(intent_up).await {
                                                ok = false;
                                                warn!(agent = self.config.agent_id, error = %e, "failed to submit hedge leg up");
                                            }
                                            if let Err(e) = ctx.submit_order(intent_down).await {
                                                ok = false;
                                                warn!(agent = self.config.agent_id, error = %e, "failed to submit hedge leg down");
                                            }
                                            if !ok {
                                                continue;
                                            }

                                            positions.insert(event.slug.clone(), TrackedPosition {
                                                market_slug: event.slug.clone(),
                                                symbol: symbol.clone(),
                                                legs: vec![
                                                    PositionLeg {
                                                        token_id: event.up_token_id.clone(),
                                                        side: Side::Up,
                                                        shares,
                                                        entry_price: up_ask,
                                                        entry_time: now,
                                                    },
                                                    PositionLeg {
                                                        token_id: event.down_token_id.clone(),
                                                        side: Side::Down,
                                                        shares,
                                                        entry_price: down_ask,
                                                        entry_time: now,
                                                    },
                                                ],
                                            });
                                            last_action_by_symbol.insert(symbol.clone(), now);
                                        }
                                    }

                                    total_exposure = positions.values()
                                        .flat_map(|p| p.legs.iter())
                                        .map(|l| l.entry_price * Decimal::from(l.shares))
                                        .sum();

                                    continue;
                                }
                            }
                        }

                        // No ONNX policy (or parse failure): run safe baseline.
                        let unrealized = pos.and_then(|p| p.legs.get(0)).map(|leg| {
                            let mark = match leg.side { Side::Up => up_bid, Side::Down => down_bid };
                            if leg.entry_price > Decimal::ZERO { (mark - leg.entry_price) / leg.entry_price } else { Decimal::ZERO }
                        });
                        let base_action = self.rule_based_policy(has_pos, Some(up_ask + down_ask), momentum_1s, unrealized);
                        let discrete = base_action.to_discrete();
                        if matches!(discrete, DiscreteAction::Hold) {
                            continue;
                        }
                        // Baseline only exits or enters single-leg. Hedge is ignored here.
                        let priority = if base_action.is_aggressive() { OrderPriority::High } else { OrderPriority::Normal };
                        let deployment_id = deployment_id_for_symbol(symbol.as_str());

                        match discrete {
                            DiscreteAction::BuyUp => {
                                if has_pos || up_ask > self.config.max_entry_price { continue; }
                                let shares = self.compute_shares(&base_action, self.config.default_shares);
                                let mut intent = OrderIntent::new(
                                    &self.config.agent_id,
                                    Domain::Crypto,
                                    event.slug.as_str(),
                                    event.up_token_id.as_str(),
                                    Side::Up,
                                    true,
                                    shares,
                                    up_ask,
                                )
                                .with_priority(priority)
                                .with_metadata("strategy", "crypto_rl_policy")
                                .with_deployment_id(deployment_id.as_str())
                                .with_metadata("signal_type", "crypto_rl_policy")
                                .with_metadata("action", "buy_up")
                                .with_metadata("coin", coin.as_str())
                                .with_metadata("symbol", symbol.as_str())
                                .with_metadata("spot_price", spot.price.to_string())
                                .with_metadata(
                                    "event_time_remaining_secs",
                                    time_remaining_secs.to_string(),
                                )
                                .with_metadata(
                                    "event_time_remaining_norm",
                                    format!("{time_remaining_norm:.6}"),
                                )
                                .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                .with_metadata("lob_age_secs", age_secs.to_string())
                                .with_condition_id(event.condition_id.as_str())
                                .with_metadata("event_end_time", event.end_time.to_rfc3339())
                                .with_metadata("event_title", event.title.as_str())
                                .with_metadata("policy_source", policy_source)
                                .with_metadata("obs_version", obs_version.to_string())
                                .with_metadata("pm_up_bid", up_bid.to_string())
                                .with_metadata("pm_up_ask", up_ask.to_string())
                                .with_metadata("pm_down_bid", down_bid.to_string())
                                .with_metadata("pm_down_ask", down_ask.to_string())
                                .with_metadata("pm_sum_of_asks", (up_ask + down_ask).to_string())
                                .with_metadata("lob_spread_bps", lob.spread_bps.to_string())
                                .with_metadata("lob_obi_5", lob.obi_5.to_string())
                                .with_metadata("lob_obi_10", lob.obi_10.to_string())
                                .with_metadata("lob_bid_volume_5", lob.bid_volume_5.to_string())
                                .with_metadata("lob_ask_volume_5", lob.ask_volume_5.to_string())
                                .with_metadata("signal_momentum_1s", momentum_1s.to_string())
                                .with_metadata("signal_momentum_5s", momentum_5s.to_string())
                                .with_metadata("config_hash", config_hash.clone());
                                if obs_version == 2 {
                                    intent.metadata.insert("lob_obi_1".into(), obi_1.to_string());
                                    intent.metadata.insert("lob_obi_2".into(), obi_2.to_string());
                                    intent.metadata.insert("lob_obi_3".into(), obi_3.to_string());
                                    intent.metadata.insert("lob_obi_20".into(), obi_20.to_string());
                                    intent.metadata.insert(
                                        "lob_obi_micro".into(),
                                        (obi_1 - lob.obi_5).to_string(),
                                    );
                                    intent.metadata.insert(
                                        "lob_obi_slope".into(),
                                        (lob.obi_5 - obi_20).to_string(),
                                    );
                                }
                                if let Err(e) = ctx.submit_order(intent).await {
                                    warn!(agent = self.config.agent_id, error = %e, "failed to submit baseline buy_up order");
                                    continue;
                                }
                                positions.insert(event.slug.clone(), TrackedPosition {
                                    market_slug: event.slug.clone(),
                                    symbol: symbol.clone(),
                                    legs: vec![PositionLeg {
                                        token_id: event.up_token_id.clone(),
                                        side: Side::Up,
                                        shares,
                                        entry_price: up_ask,
                                        entry_time: now,
                                    }],
                                });
                                last_action_by_symbol.insert(symbol.clone(), now);
                            }
                            DiscreteAction::BuyDown => {
                                if has_pos || down_ask > self.config.max_entry_price { continue; }
                                let shares = self.compute_shares(&base_action, self.config.default_shares);
                                let mut intent = OrderIntent::new(
                                    &self.config.agent_id,
                                    Domain::Crypto,
                                    event.slug.as_str(),
                                    event.down_token_id.as_str(),
                                    Side::Down,
                                    true,
                                    shares,
                                    down_ask,
                                )
                                .with_priority(priority)
                                .with_metadata("strategy", "crypto_rl_policy")
                                .with_deployment_id(deployment_id.as_str())
                                .with_metadata("signal_type", "crypto_rl_policy")
                                .with_metadata("action", "buy_down")
                                .with_metadata("coin", coin.as_str())
                                .with_metadata("symbol", symbol.as_str())
                                .with_metadata("spot_price", spot.price.to_string())
                                .with_metadata(
                                    "event_time_remaining_secs",
                                    time_remaining_secs.to_string(),
                                )
                                .with_metadata(
                                    "event_time_remaining_norm",
                                    format!("{time_remaining_norm:.6}"),
                                )
                                .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                .with_metadata("lob_age_secs", age_secs.to_string())
                                .with_condition_id(event.condition_id.as_str())
                                .with_metadata("event_end_time", event.end_time.to_rfc3339())
                                .with_metadata("event_title", event.title.as_str())
                                .with_metadata("policy_source", policy_source)
                                .with_metadata("obs_version", obs_version.to_string())
                                .with_metadata("pm_up_bid", up_bid.to_string())
                                .with_metadata("pm_up_ask", up_ask.to_string())
                                .with_metadata("pm_down_bid", down_bid.to_string())
                                .with_metadata("pm_down_ask", down_ask.to_string())
                                .with_metadata("pm_sum_of_asks", (up_ask + down_ask).to_string())
                                .with_metadata("lob_spread_bps", lob.spread_bps.to_string())
                                .with_metadata("lob_obi_5", lob.obi_5.to_string())
                                .with_metadata("lob_obi_10", lob.obi_10.to_string())
                                .with_metadata("lob_bid_volume_5", lob.bid_volume_5.to_string())
                                .with_metadata("lob_ask_volume_5", lob.ask_volume_5.to_string())
                                .with_metadata("signal_momentum_1s", momentum_1s.to_string())
                                .with_metadata("signal_momentum_5s", momentum_5s.to_string())
                                .with_metadata("config_hash", config_hash.clone());
                                if obs_version == 2 {
                                    intent.metadata.insert("lob_obi_1".into(), obi_1.to_string());
                                    intent.metadata.insert("lob_obi_2".into(), obi_2.to_string());
                                    intent.metadata.insert("lob_obi_3".into(), obi_3.to_string());
                                    intent.metadata.insert("lob_obi_20".into(), obi_20.to_string());
                                    intent.metadata.insert(
                                        "lob_obi_micro".into(),
                                        (obi_1 - lob.obi_5).to_string(),
                                    );
                                    intent.metadata.insert(
                                        "lob_obi_slope".into(),
                                        (lob.obi_5 - obi_20).to_string(),
                                    );
                                }
                                if let Err(e) = ctx.submit_order(intent).await {
                                    warn!(agent = self.config.agent_id, error = %e, "failed to submit baseline buy_down order");
                                    continue;
                                }
                                positions.insert(event.slug.clone(), TrackedPosition {
                                    market_slug: event.slug.clone(),
                                    symbol: symbol.clone(),
                                    legs: vec![PositionLeg {
                                        token_id: event.down_token_id.clone(),
                                        side: Side::Down,
                                        shares,
                                        entry_price: down_ask,
                                        entry_time: now,
                                    }],
                                });
                                last_action_by_symbol.insert(symbol.clone(), now);
                            }
                            DiscreteAction::SellPosition => {
                                let Some(pos) = positions.get(&event.slug).cloned() else { continue };
                                if pos.legs.is_empty() { continue; }
                                let leg = &pos.legs[0];
                                let bid = match leg.side { Side::Up => up_bid, Side::Down => down_bid };
                                if bid <= Decimal::ZERO { continue; }
                                let intent = OrderIntent::new(
                                    &self.config.agent_id,
                                    Domain::Crypto,
                                    event.slug.as_str(),
                                    leg.token_id.as_str(),
                                    leg.side,
                                    false,
                                    leg.shares,
                                    bid,
                                )
                                .with_priority(OrderPriority::High)
                                .with_metadata("strategy", "crypto_rl_policy")
                                .with_deployment_id(deployment_id.as_str())
                                .with_metadata("signal_type", "crypto_rl_policy")
                                .with_metadata("action", "sell")
                                .with_metadata("coin", coin.as_str())
                                .with_metadata("symbol", symbol.as_str())
                                .with_metadata("spot_price", spot.price.to_string())
                                .with_metadata(
                                    "event_time_remaining_secs",
                                    time_remaining_secs.to_string(),
                                )
                                .with_metadata(
                                    "event_time_remaining_norm",
                                    format!("{time_remaining_norm:.6}"),
                                )
                                .with_metadata("lob_timestamp", lob.timestamp.to_rfc3339())
                                .with_metadata("lob_age_secs", age_secs.to_string())
                                .with_condition_id(event.condition_id.as_str())
                                .with_metadata("policy_source", policy_source)
                                .with_metadata("config_hash", config_hash.clone());
                                if let Err(e) = ctx.submit_order(intent).await {
                                    warn!(agent = self.config.agent_id, error = %e, "failed to submit baseline sell order");
                                    continue;
                                }
                                positions.remove(&event.slug);
                                last_action_by_symbol.insert(symbol.clone(), now);
                            }
                            _ => {}
                        }

                        total_exposure = positions.values()
                            .flat_map(|p| p.legs.iter())
                            .map(|l| l.entry_price * Decimal::from(l.shares))
                            .sum();
                    }
                }

                // --- Heartbeat ---
                _ = heartbeat_tick.tick() => {
                    if let Err(e) = ctx.report_state(
                        &self.config.name,
                        status,
                        positions.len(),
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
                        None,
                    ).await {
                        warn!(agent = self.config.agent_id, error = %e, "failed to report heartbeat");
                    }
                }

                // --- Coordinator commands ---
                cmd = ctx.command_rx().recv() => {
                    match cmd {
                        Some(CoordinatorCommand::Pause) => {
                            info!(agent = self.config.agent_id, "pausing");
                            status = AgentStatus::Paused;
                        }
                        Some(CoordinatorCommand::Resume) => {
                            info!(agent = self.config.agent_id, "resuming");
                            status = AgentStatus::Running;
                        }
                        Some(CoordinatorCommand::Shutdown) => {
                            info!(agent = self.config.agent_id, "shutting down");
                            break;
                        }
                        Some(CoordinatorCommand::ForceClose) => {
                            warn!(agent = self.config.agent_id, "force close — submitting exit orders");
                            let quote_cache = self.pm_ws.quote_cache();
                            for (slug, pos) in &positions {
                                let deployment_id = deployment_id_for_symbol(&pos.symbol);
                                for leg in &pos.legs {
                                    let bid = quote_cache
                                        .get(&leg.token_id)
                                        .and_then(|q| q.best_bid)
                                        .unwrap_or(dec!(0.01));

                                    let intent = OrderIntent::new(
                                        &self.config.agent_id,
                                        Domain::Crypto,
                                        slug.as_str(),
                                        leg.token_id.as_str(),
                                        leg.side,
                                        false,
                                        leg.shares,
                                        bid,
                                    )
                                    .with_priority(OrderPriority::Critical)
                                    .with_metadata("strategy", "crypto_rl_policy")
                                    .with_deployment_id(deployment_id.as_str())
                                    .with_metadata("signal_type", "crypto_rl_policy")
                                    .with_metadata("action", "force_close")
                                    .with_metadata("config_hash", &config_hash);

                                    if let Err(e) = ctx.submit_order(intent).await {
                                        error!(
                                            agent = %self.config.agent_id,
                                            slug = %slug,
                                            error = %e,
                                            "CRITICAL: force-close exit order FAILED — position remains open"
                                        );
                                    }
                                }
                            }
                            positions.clear();
                            break;
                        }
                        Some(CoordinatorCommand::HealthCheck(tx)) => {
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: self.config.name.clone(),
                                domain: Domain::Crypto,
                                status,
                                position_count: positions.len(),
                                exposure: total_exposure,
                                daily_pnl,
                                unrealized_pnl: Decimal::ZERO,
                                metrics: HashMap::new(),
                                last_heartbeat: Utc::now(),
                                error_message: None,
                            };
                            let _ = tx.send(crate::coordinator::AgentHealthResponse {
                                snapshot,
                                is_healthy: matches!(status, AgentStatus::Running),
                                uptime_secs: 0,
                                orders_submitted: 0,
                                orders_filled: 0,
                            });
                        }
                        None => break,
                    }
                }
            }
        }

        Ok(())
    }
}
