//! CryptoLobMlAgent — pull-based agent that uses Binance LOB features to estimate
//! a short-horizon UP probability across crypto 5m/15m markets and trade Polymarket
//! UP/DOWN markets accordingly.
//!
//! This is intentionally lightweight: it provides a deployable baseline for
//! collecting training data and running a probabilistic strategy *without*
//! requiring the optional `rl` feature gate. RL integration can replace the
//! `estimate_p_up()` function later.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::adapters::{BinanceWebSocket, PolymarketWebSocket, PriceUpdate, QuoteUpdate, SpotPrice};
use crate::agents::{AgentContext, TradingAgent};
use crate::collector::{LobCache, LobSnapshot};
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::{PloyError, Result};
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

const TRADED_EVENT_RETENTION_HOURS: i64 = 24;
const STRATEGY_ID: &str = "crypto_lob_ml";

/// Standard normal CDF approximation (Abramowitz-Stegun), ~4dp accuracy.
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x / 2.0).exp();

    0.5 * (1.0 + sign * y)
}

fn default_exit_edge_floor() -> Decimal {
    dec!(0.02)
}

fn default_exit_price_band() -> Decimal {
    dec!(0.05)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoLobMlExitMode {
    /// Hold position until market settlement.
    SettleOnly,
    /// Exit when model side flips (after min hold time).
    SignalFlip,
    /// Exit on mark-to-market price thresholds.
    PriceExit,
}

fn default_exit_mode() -> CryptoLobMlExitMode {
    // Binary option architecture default: hold to settlement.
    CryptoLobMlExitMode::SettleOnly
}

/// Configuration for the CryptoLobMlAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoLobMlConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,

    /// Refresh interval for Gamma event discovery (seconds)
    pub event_refresh_secs: u64,

    /// Minimum time remaining for selected event (seconds)
    pub min_time_remaining_secs: u64,

    /// Maximum time remaining for selected event (seconds)
    pub max_time_remaining_secs: u64,

    /// If true, prefer events closest to end (confirmatory mode).
    /// If false, prefer events with more time remaining (predictive mode).
    pub prefer_close_to_end: bool,

    pub default_shares: u64,
    #[serde(default = "default_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    #[serde(default = "default_exit_price_band")]
    pub exit_price_band: Decimal,
    /// Exit policy:
    /// - settle_only: hold until settlement
    /// - signal_flip: exit on side flip
    /// - price_exit: exit on mark-to-market thresholds
    #[serde(default = "default_exit_mode")]
    pub exit_mode: CryptoLobMlExitMode,
    /// Minimum hold time before edge/price-band exits are allowed (seconds)
    pub min_hold_secs: u64,

    /// Minimum expected-value edge required to enter.
    /// This is measured on net EV after fee/slippage adjustments.
    pub min_edge: Decimal,

    /// Max ask price to pay for entry (YES/NO).
    pub max_entry_price: Decimal,

    /// Taker fee rate used in net EV calculation (e.g. 0.02 = 2%).
    #[serde(default = "default_taker_fee_rate")]
    pub taker_fee_rate: Decimal,

    /// Slippage buffer applied to entry ask in basis points (e.g. 10 = 0.10%).
    #[serde(default = "default_entry_slippage_bps")]
    pub entry_slippage_bps: Decimal,

    /// If true, incorporate event price-to-beat into settlement probability.
    #[serde(default = "default_use_price_to_beat")]
    pub use_price_to_beat: bool,

    /// If true, skip events that do not expose a parseable price-to-beat.
    #[serde(default = "default_require_price_to_beat")]
    pub require_price_to_beat: bool,

    /// Blend weight for threshold-anchored settlement probability.
    /// p_up = p_up_base * (1 - w_threshold) + p_up_threshold * w_threshold
    #[serde(default = "default_threshold_prob_weight")]
    pub threshold_prob_weight: Decimal,

    /// Minimum seconds between entries per symbol (avoid thrash).
    pub cooldown_secs: u64,

    /// Reject LOB snapshots older than this age (seconds).
    pub max_lob_snapshot_age_secs: u64,

    /// Prediction model type. Must be "onnx".
    #[serde(default = "default_lob_ml_model_type")]
    pub model_type: String,

    /// ONNX model path used for online inference.
    #[serde(default)]
    pub model_path: Option<String>,

    /// Optional model version label recorded in order metadata (helps audit).
    #[serde(default)]
    pub model_version: Option<String>,

    /// Safety fallback blend weight for window baseline probability.
    /// p_up = p_up_model * (1 - w_window) + p_up_window * w_window
    #[serde(default = "default_window_fallback_weight")]
    pub window_fallback_weight: Decimal,

    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

fn default_lob_ml_model_type() -> String {
    "onnx".to_string()
}

fn default_window_fallback_weight() -> Decimal {
    dec!(0.10)
}

fn default_taker_fee_rate() -> Decimal {
    dec!(0.02)
}

fn default_entry_slippage_bps() -> Decimal {
    dec!(10)
}

fn default_use_price_to_beat() -> bool {
    true
}

fn default_require_price_to_beat() -> bool {
    true
}

fn default_threshold_prob_weight() -> Decimal {
    dec!(0.35)
}

impl Default for CryptoLobMlConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto_lob_ml".into(),
            name: "Crypto LOB ML".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            event_refresh_secs: 15,
            // Cover both 5m + 15m windows by default.
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            prefer_close_to_end: true,
            default_shares: 50,
            exit_edge_floor: default_exit_edge_floor(),
            exit_price_band: default_exit_price_band(),
            exit_mode: default_exit_mode(),
            min_hold_secs: 20,
            min_edge: dec!(0.02),
            max_entry_price: dec!(0.70),
            taker_fee_rate: default_taker_fee_rate(),
            entry_slippage_bps: default_entry_slippage_bps(),
            use_price_to_beat: default_use_price_to_beat(),
            require_price_to_beat: default_require_price_to_beat(),
            threshold_prob_weight: default_threshold_prob_weight(),
            cooldown_secs: 30,
            max_lob_snapshot_age_secs: 2,
            model_type: default_lob_ml_model_type(),
            model_path: None,
            model_version: None,
            window_fallback_weight: default_window_fallback_weight(),
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone)]
struct TrackedPosition {
    market_slug: String,
    symbol: String,
    horizon: String,
    series_id: String,
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct WindowContext {
    now: DateTime<Utc>,
    start_price: Decimal,
    window_move: Decimal,
    elapsed_secs: i64,
    remaining_secs: i64,
    p_up_window: Decimal,
}

#[derive(Debug, Clone)]
struct EntrySignal {
    side: Side,
    token_id: String,
    limit_price: Decimal,
    edge: Decimal,
    gross_edge: Decimal,
    fair_value: Decimal,
    signal_confidence: Decimal,
    p_up_window: Decimal,
    p_up_threshold: Option<Decimal>,
    p_up_blended: Decimal,
    w_threshold: Decimal,
    w_model: Decimal,
    w_window: Decimal,
    up_edge_gross: Decimal,
    down_edge_gross: Decimal,
    up_edge_net: Decimal,
    down_edge_net: Decimal,
    up_ask: Decimal,
    down_ask: Decimal,
}

pub struct CryptoLobMlAgent {
    config: CryptoLobMlConfig,
    binance_ws: Arc<BinanceWebSocket>,
    pm_ws: Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
    lob_cache: LobCache,
    #[cfg(feature = "onnx")]
    onnx_model: Option<OnnxModel>,
}

fn should_skip_entry(
    event_slug: &str,
    entry_key: &str,
    now: DateTime<Utc>,
    positions: &HashMap<String, TrackedPosition>,
    traded_events: &HashMap<String, DateTime<Utc>>,
    last_trade_by_key: &HashMap<String, DateTime<Utc>>,
    cooldown_secs: u64,
) -> bool {
    if positions.contains_key(event_slug) {
        return true;
    }

    if traded_events.contains_key(event_slug) {
        return true;
    }

    if let Some(last) = last_trade_by_key.get(entry_key) {
        if now.signed_duration_since(*last).num_seconds() < cooldown_secs as i64 {
            return true;
        }
    }

    false
}

fn prune_stale_traded_events(
    traded_events: &mut HashMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    let retention = chrono::Duration::hours(TRADED_EVENT_RETENTION_HOURS);
    traded_events.retain(|_, entered_at| now.signed_duration_since(*entered_at) < retention);
}

fn normalize_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn normalize_timeframe(horizon: &str) -> String {
    let raw = horizon.trim().to_ascii_lowercase();
    if raw.contains("15m") || raw == "15" {
        "15m".to_string()
    } else if raw.contains("5m") || raw == "5" {
        "5m".to_string()
    } else if raw.is_empty() {
        "5m".to_string()
    } else {
        raw
    }
}

fn event_window_secs_for_horizon(horizon: &str) -> u64 {
    match normalize_timeframe(horizon).as_str() {
        "15m" => 15 * 60,
        "5m" => 5 * 60,
        _ => 5 * 60,
    }
}

fn deployment_id_for(strategy: &str, coin: &str, horizon: &str) -> String {
    format!(
        "crypto.pm.{}.{}.{}",
        normalize_component(coin),
        normalize_timeframe(horizon),
        normalize_component(strategy)
    )
}

fn infer_coin_from_market_slug(slug: &str) -> String {
    slug.split('-')
        .next()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

fn infer_horizon_from_market_slug(slug: &str) -> String {
    normalize_timeframe(slug)
}

fn tracked_position_from_global(position: &crate::platform::Position) -> TrackedPosition {
    let coin = infer_coin_from_market_slug(&position.market_slug);
    let symbol = position
        .metadata
        .get("symbol")
        .cloned()
        .unwrap_or_else(|| format!("{coin}USDT"));
    let horizon = position
        .metadata
        .get("horizon")
        .cloned()
        .unwrap_or_else(|| infer_horizon_from_market_slug(&position.market_slug));
    let series_id = position
        .metadata
        .get("series_id")
        .or_else(|| position.metadata.get("event_series_id"))
        .cloned()
        .unwrap_or_default();

    TrackedPosition {
        market_slug: position.market_slug.clone(),
        symbol,
        horizon,
        series_id,
        token_id: position.token_id.clone(),
        side: position.side,
        shares: position.shares,
        entry_price: position.entry_price,
        entry_time: position.entry_time,
    }
}

async fn sync_positions_from_global(
    ctx: &AgentContext,
    agent_id: &str,
    positions: &mut HashMap<String, TrackedPosition>,
) -> Decimal {
    let state = ctx.read_global_state().await;
    positions.clear();
    for position in state.positions {
        if position.agent_id != agent_id
            || position.domain != Domain::Crypto
            || position.shares == 0
        {
            continue;
        }
        positions.insert(
            position.market_slug.clone(),
            tracked_position_from_global(&position),
        );
    }

    positions
        .values()
        .map(|p| p.entry_price * Decimal::from(p.shares))
        .sum()
}

impl CryptoLobMlAgent {
    pub fn new(
        config: CryptoLobMlConfig,
        binance_ws: Arc<BinanceWebSocket>,
        pm_ws: Arc<PolymarketWebSocket>,
        event_matcher: Arc<EventMatcher>,
        lob_cache: LobCache,
    ) -> Result<Self> {
        let model_type = config.model_type.trim().to_ascii_lowercase();
        if model_type != "onnx" {
            return Err(PloyError::Validation(format!(
                "crypto_lob_ml only accepts model_type=onnx, got '{model_type}'"
            )));
        }

        #[cfg(feature = "onnx")]
        {
            let model_path = config
                .model_path
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    PloyError::Validation(
                        "crypto_lob_ml requires PLOY_CRYPTO_LOB_ML__MODEL_PATH for ONNX inference"
                            .to_string(),
                    )
                })?;

            let m = OnnxModel::load_for_vec_input(model_path, 7)?;
            info!(
                agent = config.agent_id,
                model_type = "onnx",
                model_path = %model_path,
                input_dim = m.input_dim(),
                output_dim = m.output_dim(),
                "loaded lob-ml onnx model"
            );

            return Ok(Self {
                config,
                binance_ws,
                pm_ws,
                event_matcher,
                lob_cache,
                onnx_model: Some(m),
            });
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = (binance_ws, pm_ws, event_matcher, lob_cache);
            Err(PloyError::Validation(
                "crypto_lob_ml requires building with --features onnx".to_string(),
            ))
        }
    }

    fn config_hash(&self) -> String {
        let payload = serde_json::to_vec(&self.config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(payload);
        format!("{:x}", hasher.finalize())
    }

    #[cfg(feature = "onnx")]
    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    fn estimate_p_up_window(
        window_move: Decimal,
        sigma_1s: Option<Decimal>,
        remaining_secs: i64,
    ) -> Decimal {
        let remaining_secs = remaining_secs.max(0) as f64;
        let Some(sig_1s) = sigma_1s.and_then(|v| v.to_f64()) else {
            return dec!(0.50);
        };
        if !sig_1s.is_finite() || sig_1s <= 0.0 {
            return dec!(0.50);
        }

        let sigma_rem = sig_1s * remaining_secs.sqrt();
        if !sigma_rem.is_finite() || sigma_rem <= 0.0 {
            return dec!(0.50);
        }

        let w = window_move.to_f64().unwrap_or(0.0);
        if !w.is_finite() {
            return dec!(0.50);
        }

        let p = normal_cdf(w / sigma_rem).clamp(0.001, 0.999);
        Decimal::from_f64_retain(p).unwrap_or(dec!(0.50))
    }

    /// Estimate p(UP) using ONNX model. Returns (p_up, model_type_used).
    fn estimate_p_up(
        &self,
        lob: &LobSnapshot,
        momentum_1s: Decimal,
        momentum_5s: Decimal,
    ) -> Result<(f64, &'static str)> {
        #[cfg(feature = "onnx")]
        {
            // Shared feature order (must match training/export):
            // [obi5, obi10, spread_bps, bid_volume_5, ask_volume_5, momentum_1s, momentum_5s]
            let obi5 = lob.obi_5.to_f64().unwrap_or(0.0);
            let obi10 = lob.obi_10.to_f64().unwrap_or(0.0);
            let spread = lob.spread_bps.to_f64().unwrap_or(0.0);
            let bidv5 = lob.bid_volume_5.to_f64().unwrap_or(0.0);
            let askv5 = lob.ask_volume_5.to_f64().unwrap_or(0.0);
            let m1 = momentum_1s.to_f64().unwrap_or(0.0);
            let m5 = momentum_5s.to_f64().unwrap_or(0.0);

            let m = self
                .onnx_model
                .as_ref()
                .ok_or_else(|| PloyError::InvalidState("onnx model not initialized".to_string()))?;
            let features = [
                obi5 as f32,
                obi10 as f32,
                spread as f32,
                bidv5 as f32,
                askv5 as f32,
                m1 as f32,
                m5 as f32,
            ];
            let raw = m.predict_scalar(&features)?;
            if !raw.is_finite() {
                return Err(PloyError::Internal(
                    "lob-ml onnx returned non-finite output".to_string(),
                ));
            }
            // Prefer probability output, but tolerate logits.
            let p = if raw < -0.001 || raw > 1.001 {
                Self::sigmoid(raw as f64)
            } else {
                raw as f64
            };
            return Ok((p.clamp(0.001, 0.999), "onnx"));
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = (lob, momentum_1s, momentum_5s);
            Err(PloyError::Validation(
                "crypto_lob_ml requires --features onnx".to_string(),
            ))
        }
    }

    fn model_window_blend_weights(&self) -> (Decimal, Decimal) {
        // Keep window baseline as a small fallback, never dominant.
        let w_window = self
            .config
            .window_fallback_weight
            .max(Decimal::ZERO)
            .min(dec!(0.49));
        let w_model = (Decimal::ONE - w_window).max(Decimal::ZERO);
        (w_model, w_window)
    }

    fn threshold_blend_weight(&self) -> Decimal {
        self.config
            .threshold_prob_weight
            .max(Decimal::ZERO)
            .min(dec!(0.90))
    }

    fn allows_signal_flip_exit(&self) -> bool {
        matches!(self.config.exit_mode, CryptoLobMlExitMode::SignalFlip)
    }

    fn allows_price_exit(&self) -> bool {
        matches!(self.config.exit_mode, CryptoLobMlExitMode::PriceExit)
    }

    fn exit_mode_label(&self) -> &'static str {
        match self.config.exit_mode {
            CryptoLobMlExitMode::SettleOnly => "settle_only",
            CryptoLobMlExitMode::SignalFlip => "signal_flip",
            CryptoLobMlExitMode::PriceExit => "price_exit",
        }
    }

    fn net_ev_for_binary_side(&self, prob_win: Decimal, ask: Decimal) -> Decimal {
        if ask <= Decimal::ZERO || ask >= Decimal::ONE {
            return Decimal::MIN;
        }

        let slippage_rate = (self.config.entry_slippage_bps / dec!(10000))
            .max(Decimal::ZERO)
            .min(dec!(0.25));
        let effective_entry = (ask * (Decimal::ONE + slippage_rate))
            .max(Decimal::ZERO)
            .min(dec!(0.999));

        let fee_rate = self
            .config
            .taker_fee_rate
            .max(Decimal::ZERO)
            .min(dec!(0.25));
        let prob = prob_win.max(dec!(0.001)).min(dec!(0.999));
        let net_profit_on_win = (Decimal::ONE - effective_entry) * (Decimal::ONE - fee_rate);
        let loss_on_lose = effective_entry;

        prob * net_profit_on_win - (Decimal::ONE - prob) * loss_on_lose
    }

    fn estimate_p_up_threshold_anchor(
        &self,
        spot_price: Decimal,
        price_to_beat: Option<Decimal>,
        sigma_1s: Option<Decimal>,
        remaining_secs: i64,
    ) -> Option<Decimal> {
        if !self.config.use_price_to_beat {
            return None;
        }

        let threshold = price_to_beat?;
        if spot_price <= Decimal::ZERO || threshold <= Decimal::ZERO || remaining_secs <= 0 {
            return None;
        }

        let sigma_1s = sigma_1s.and_then(|v| v.to_f64())?;
        if !sigma_1s.is_finite() || sigma_1s <= 0.0 {
            return None;
        }

        let sigma_rem = sigma_1s * (remaining_secs as f64).sqrt();
        if !sigma_rem.is_finite() || sigma_rem <= 0.0 {
            return None;
        }

        let spot = spot_price.to_f64()?;
        let beat = threshold.to_f64()?;
        if !spot.is_finite() || !beat.is_finite() || spot <= 0.0 || beat <= 0.0 {
            return None;
        }

        let required_return = (beat - spot) / spot;
        if !required_return.is_finite() {
            return None;
        }

        let p = (1.0 - normal_cdf(required_return / sigma_rem)).clamp(0.001, 0.999);
        Decimal::from_f64_retain(p)
    }

    fn build_window_context(
        &self,
        spot: &SpotPrice,
        event: &EventInfo,
        rolling_volatility_opt: Option<Decimal>,
    ) -> Option<WindowContext> {
        let now = spot.timestamp;
        if now < event.start_time || now >= event.end_time {
            return None;
        }

        let elapsed_secs = now
            .signed_duration_since(event.start_time)
            .num_seconds()
            .max(0);
        let remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
        if remaining_secs <= 0 {
            return None;
        }

        let target_time = now - chrono::Duration::seconds(elapsed_secs);
        match spot.oldest_timestamp() {
            Some(oldest) if oldest > target_time => return None,
            Some(_) => {}
            None => return None,
        }

        let start_price = spot.price_secs_ago(elapsed_secs as u64)?;
        if start_price <= Decimal::ZERO {
            return None;
        }

        let window_move = (spot.price - start_price) / start_price;
        let p_up_window =
            Self::estimate_p_up_window(window_move, rolling_volatility_opt, remaining_secs);

        Some(WindowContext {
            now,
            start_price,
            window_move,
            elapsed_secs,
            remaining_secs,
            p_up_window,
        })
    }

    fn evaluate_entry_signal(
        &self,
        p_up_model_dec: Decimal,
        window: &WindowContext,
        p_up_threshold: Option<Decimal>,
        up_token_id: &str,
        down_token_id: &str,
        up_ask: Decimal,
        down_ask: Decimal,
    ) -> Option<EntrySignal> {
        if up_ask <= Decimal::ZERO || down_ask <= Decimal::ZERO {
            return None;
        }

        let (w_model, w_window) = self.model_window_blend_weights();
        let p_up_base = (window.p_up_window * w_window + p_up_model_dec * w_model)
            .max(dec!(0.001))
            .min(dec!(0.999));
        let w_threshold = self.threshold_blend_weight();
        let p_up_blended = if let Some(p_thr) = p_up_threshold {
            if w_threshold > Decimal::ZERO {
                (p_up_base * (Decimal::ONE - w_threshold) + p_thr * w_threshold)
                    .max(dec!(0.001))
                    .min(dec!(0.999))
            } else {
                p_up_base
            }
        } else {
            if self.config.use_price_to_beat && self.config.require_price_to_beat {
                return None;
            }
            p_up_base
        };

        let up_edge_gross = p_up_blended - up_ask;
        let down_edge_gross = (Decimal::ONE - p_up_blended) - down_ask;
        let up_edge_net = self.net_ev_for_binary_side(p_up_blended, up_ask);
        let down_edge_net = self.net_ev_for_binary_side(Decimal::ONE - p_up_blended, down_ask);

        let (side, token_id, limit_price, edge, gross_edge, fair_value) =
            if up_edge_net >= down_edge_net {
                (
                    Side::Up,
                    up_token_id.to_string(),
                    up_ask,
                    up_edge_net,
                    up_edge_gross,
                    p_up_blended,
                )
            } else {
                (
                    Side::Down,
                    down_token_id.to_string(),
                    down_ask,
                    down_edge_net,
                    down_edge_gross,
                    Decimal::ONE - p_up_blended,
                )
            };

        if edge < self.config.min_edge || limit_price > self.config.max_entry_price {
            return None;
        }

        let signal_confidence = (edge / dec!(0.10)).max(Decimal::ZERO).min(Decimal::ONE);

        Some(EntrySignal {
            side,
            token_id,
            limit_price,
            edge,
            gross_edge,
            fair_value,
            signal_confidence,
            p_up_window: window.p_up_window,
            p_up_threshold,
            p_up_blended,
            w_threshold,
            w_model,
            w_window,
            up_edge_gross,
            down_edge_gross,
            up_edge_net,
            down_edge_net,
            up_ask,
            down_ask,
        })
    }
}

#[async_trait]
impl TradingAgent for CryptoLobMlAgent {
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
        info!(agent = self.config.agent_id, "crypto lob-ml agent starting");
        let config_hash = self.config_hash();

        let mut status = AgentStatus::Running;
        let mut positions: HashMap<String, TrackedPosition> = HashMap::new(); // slug -> pos
        let mut active_events: HashMap<String, Vec<EventInfo>> = HashMap::new(); // symbol -> events
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut last_trade_by_key: HashMap<String, DateTime<Utc>> = HashMap::new(); // symbol|timeframe -> ts
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();

        let daily_pnl = Decimal::ZERO;
        sync_positions_from_global(&ctx, &self.config.agent_id, &mut positions).await;

        // Subscribe to data feeds
        let mut binance_rx: broadcast::Receiver<PriceUpdate> = self.binance_ws.subscribe();
        let mut pm_rx: broadcast::Receiver<QuoteUpdate> = self.pm_ws.subscribe_updates();

        let refresh_dur = tokio::time::Duration::from_secs(self.config.event_refresh_secs.max(1));
        let mut refresh_tick = tokio::time::interval(refresh_dur);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                    prune_stale_traded_events(&mut traded_events, Utc::now());

                    let mut refreshed_events: HashMap<String, Vec<EventInfo>> = HashMap::new();
                    for coin in &self.config.coins {
                        let symbol = format!("{}USDT", coin.to_uppercase());
                        let mut events = self
                            .event_matcher
                            .get_events_with_min_remaining(
                                &symbol,
                                self.config.min_time_remaining_secs as i64,
                            )
                            .await;
                        events.retain(|e| {
                            e.time_remaining().num_seconds()
                                <= self.config.max_time_remaining_secs as i64
                        });
                        events.sort_by_key(|e| e.time_remaining().num_seconds());
                        if !self.config.prefer_close_to_end {
                            events.reverse();
                        }
                        if !events.is_empty() {
                            refreshed_events.insert(symbol, events);
                        }
                    }

                    active_events = refreshed_events;

                    // Ensure we are subscribed to the latest token set.
                    let mut desired_tokens: HashSet<String> = HashSet::new();
                    for events in active_events.values() {
                        for event in events {
                            desired_tokens.insert(event.up_token_id.clone());
                            desired_tokens.insert(event.down_token_id.clone());
                        }
                    }

                    if desired_tokens != subscribed_tokens {
                        for events in active_events.values() {
                            for event in events {
                                self.pm_ws
                                    .register_tokens(&event.up_token_id, &event.down_token_id)
                                    .await;
                            }
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

                // --- Binance price updates (entry decisions) ---
                result = binance_rx.recv() => {
                    let update = match result {
                        Ok(u) => u,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(agent = self.config.agent_id, lagged = n, "binance rx lagged");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            error!(agent = self.config.agent_id, "binance feed closed");
                            break;
                        }
                    };

                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }

                    let coin = update.symbol.replace("USDT", "");
                    if !self.config.coins.iter().any(|c| c == &coin) {
                        continue;
                    }

                    let events = match active_events.get(&update.symbol) {
                        Some(e) if !e.is_empty() => e.clone(),
                        None => {
                            debug!(agent = self.config.agent_id, symbol = %update.symbol, "no active event yet");
                            continue;
                        }
                        _ => continue,
                    };

                    // Pull LOB snapshot (feature vector).
                    let lob = match self.lob_cache.get_snapshot(&update.symbol).await {
                        Some(s) => s,
                        None => continue,
                    };
                    let age_secs = Utc::now().signed_duration_since(lob.timestamp).num_seconds();
                    if age_secs > self.config.max_lob_snapshot_age_secs as i64 {
                        continue;
                    }

                    // Momentum + volatility from trade-tick cache.
                    let spot_cache = self.binance_ws.price_cache();
                    let Some(spot) = spot_cache.get(&update.symbol).await else {
                        continue;
                    };
                    let momentum_1s = spot.momentum(1).unwrap_or(Decimal::ZERO);
                    let momentum_5s = spot.momentum(5).unwrap_or(Decimal::ZERO);
                    let rolling_volatility_opt = spot.volatility(60);

                    let (p_up_model, model_type_used) = match self
                        .estimate_p_up(&lob, momentum_1s, momentum_5s)
                    {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                agent = self.config.agent_id,
                                symbol = %update.symbol,
                                error = %e,
                                "onnx inference failed, skipping this tick"
                            );
                            continue;
                        }
                    };
                    let p_up_model_dec =
                        Decimal::from_f64_retain(p_up_model).unwrap_or(dec!(0.5));

                    let (obi_1, obi_2, obi_3, obi_20) = (
                        self.lob_cache
                            .get_obi(&update.symbol, 1)
                            .await
                            .unwrap_or(Decimal::ZERO),
                        self.lob_cache
                            .get_obi(&update.symbol, 2)
                            .await
                            .unwrap_or(Decimal::ZERO),
                        self.lob_cache
                            .get_obi(&update.symbol, 3)
                            .await
                            .unwrap_or(Decimal::ZERO),
                        self.lob_cache
                            .get_obi(&update.symbol, 20)
                            .await
                            .unwrap_or(Decimal::ZERO),
                    );
                    let obi_micro = obi_1 - lob.obi_5;
                    let obi_slope = lob.obi_5 - obi_20;

                    let quote_cache = self.pm_ws.quote_cache();
                    for event in events {
                        let timeframe = normalize_timeframe(&event.horizon);
                        let entry_key = format!("{}|{}", update.symbol, &timeframe);
                        let Some(window_ctx) =
                            self.build_window_context(&spot, &event, rolling_volatility_opt)
                        else {
                            continue;
                        };
                        let p_up_threshold = self.estimate_p_up_threshold_anchor(
                            spot.price,
                            event.price_to_beat,
                            rolling_volatility_opt,
                            window_ctx.remaining_secs,
                        );

                        let up = quote_cache.get(&event.up_token_id);
                        let down = quote_cache.get(&event.down_token_id);
                        let (_up_bid, up_ask, _down_bid, down_ask) = match (up, down) {
                            (Some(uq), Some(dq)) => (
                                uq.best_bid.unwrap_or(Decimal::ZERO),
                                uq.best_ask.unwrap_or(Decimal::ZERO),
                                dq.best_bid.unwrap_or(Decimal::ZERO),
                                dq.best_ask.unwrap_or(Decimal::ZERO),
                            ),
                            _ => continue,
                        };
                        let Some(signal) = self.evaluate_entry_signal(
                            p_up_model_dec,
                            &window_ctx,
                            p_up_threshold,
                            &event.up_token_id,
                            &event.down_token_id,
                            up_ask,
                            down_ask,
                        ) else {
                            continue;
                        };

                        if let Some(pos) = positions.get(&event.slug).cloned() {
                            if pos.side != signal.side && self.allows_signal_flip_exit() {
                                let held_secs = Utc::now().signed_duration_since(pos.entry_time).num_seconds();
                                if held_secs >= self.config.min_hold_secs as i64 {
                                    let exit_price = quote_cache
                                        .get(&pos.token_id)
                                        .and_then(|q| q.best_bid)
                                        .unwrap_or(Decimal::ZERO);

                                    if exit_price > Decimal::ZERO {
                                        let exit_intent = OrderIntent::new(
                                            &self.config.agent_id,
                                            Domain::Crypto,
                                            &pos.market_slug,
                                            &pos.token_id,
                                            pos.side,
                                            false,
                                            pos.shares,
                                            exit_price,
                                        );
                                        let position_coin = pos.symbol.replace("USDT", "");
                                        let deployment_id =
                                            deployment_id_for(STRATEGY_ID, &position_coin, &pos.horizon);
                                        let timeframe = normalize_timeframe(&pos.horizon);
                                        let event_window_secs =
                                            event_window_secs_for_horizon(&timeframe).to_string();
                                        let exit_intent = exit_intent
                                        .with_priority(OrderPriority::High)
                                        .with_metadata("strategy", STRATEGY_ID)
                                        .with_metadata("deployment_id", &deployment_id)
                                        .with_metadata("timeframe", &timeframe)
                                        .with_metadata("event_window_secs", &event_window_secs)
                                        .with_metadata("signal_type", "crypto_lob_ml_exit")
                                        .with_metadata("coin", &position_coin)
                                        .with_metadata("symbol", &pos.symbol)
                                        .with_metadata("series_id", &pos.series_id)
                                        .with_metadata("event_series_id", &pos.series_id)
                                        .with_metadata("horizon", &pos.horizon)
                                        .with_metadata("exit_mode", self.exit_mode_label())
                                        .with_metadata("exit_reason", "signal_flip")
                                        .with_metadata("entry_price", &pos.entry_price.to_string())
                                        .with_metadata("exit_price", &exit_price.to_string())
                                        .with_metadata("held_secs", &held_secs.to_string())
                                        .with_metadata("p_up_model", &format!("{p_up_model:.6}"))
                                        .with_metadata("p_up_window", &signal.p_up_window.to_string())
                                        .with_metadata(
                                            "p_up_threshold",
                                            &signal
                                                .p_up_threshold
                                                .unwrap_or(dec!(0.5))
                                                .to_string(),
                                        )
                                        .with_metadata("p_up_blended", &signal.p_up_blended.to_string())
                                        .with_metadata(
                                            "p_up_blend_w_threshold",
                                            &signal.w_threshold.to_string(),
                                        )
                                        .with_metadata("p_up_blend_w_model", &signal.w_model.to_string())
                                        .with_metadata(
                                            "p_up_blend_w_window",
                                            &signal.w_window.to_string(),
                                        )
                                        .with_metadata("signal_edge", &signal.edge.to_string())
                                        .with_metadata("signal_edge_gross", &signal.gross_edge.to_string())
                                        .with_metadata(
                                            "signal_up_edge_gross",
                                            &signal.up_edge_gross.to_string(),
                                        )
                                        .with_metadata(
                                            "signal_down_edge_gross",
                                            &signal.down_edge_gross.to_string(),
                                        )
                                        .with_metadata(
                                            "signal_up_edge_net",
                                            &signal.up_edge_net.to_string(),
                                        )
                                        .with_metadata(
                                            "signal_down_edge_net",
                                            &signal.down_edge_net.to_string(),
                                        )
                                        .with_metadata(
                                            "cost_taker_fee_rate",
                                            &self.config.taker_fee_rate.to_string(),
                                        )
                                        .with_metadata(
                                            "cost_entry_slippage_bps",
                                            &self.config.entry_slippage_bps.to_string(),
                                        )
                                        .with_metadata("config_hash", &config_hash);

                                        match ctx.submit_order(exit_intent).await {
                                            Ok(()) => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    slug = %event.slug,
                                                    old_side = %pos.side,
                                                    new_side = %signal.side,
                                                    held_secs,
                                                    p_up = %signal.p_up_blended,
                                                    "signal flip detected, submitting sell order"
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    agent = self.config.agent_id,
                                                    slug = %event.slug,
                                                    error = %e,
                                                    "failed to submit signal-flip exit order"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        if should_skip_entry(
                            &event.slug,
                            entry_key.as_str(),
                            window_ctx.now,
                            &positions,
                            &traded_events,
                            &last_trade_by_key,
                            self.config.cooldown_secs,
                        ) {
                            continue;
                        }

                        let intent = OrderIntent::new(
                            &self.config.agent_id,
                            Domain::Crypto,
                            event.slug.as_str(),
                            &signal.token_id,
                            signal.side,
                            true,
                            self.config.default_shares.max(1),
                            signal.limit_price,
                        );
                        let deployment_id = deployment_id_for(STRATEGY_ID, &coin, &event.horizon);
                        let event_window_secs =
                            event_window_secs_for_horizon(&timeframe).to_string();
                        let intent = intent
                        .with_priority(OrderPriority::Normal)
                        .with_metadata("strategy", STRATEGY_ID)
                        .with_metadata("deployment_id", &deployment_id)
                        .with_metadata("timeframe", &timeframe)
                        .with_metadata("event_window_secs", &event_window_secs)
                        .with_metadata("signal_type", "crypto_lob_ml_entry")
                        .with_metadata("coin", &coin)
                        .with_metadata("symbol", &update.symbol)
                        .with_metadata("condition_id", &event.condition_id)
                        .with_metadata("series_id", &event.series_id)
                        .with_metadata("event_series_id", &event.series_id)
                        .with_metadata("horizon", &event.horizon)
                        .with_metadata("exit_mode", self.exit_mode_label())
                        .with_metadata("event_end_time", &event.end_time.to_rfc3339())
                        .with_metadata("event_title", &event.title)
                        .with_metadata(
                            "price_to_beat",
                            &event
                                .price_to_beat
                                .unwrap_or(Decimal::ZERO)
                                .to_string(),
                        )
                        .with_metadata("p_up_model", &format!("{p_up_model:.6}"))
                        .with_metadata("p_up_window", &signal.p_up_window.to_string())
                        .with_metadata(
                            "p_up_threshold",
                            &signal
                                .p_up_threshold
                                .unwrap_or(dec!(0.5))
                                .to_string(),
                        )
                        .with_metadata("p_up_blended", &signal.p_up_blended.to_string())
                        .with_metadata("p_up_blend_w_threshold", &signal.w_threshold.to_string())
                        .with_metadata("p_up_blend_w_model", &signal.w_model.to_string())
                        .with_metadata("p_up_blend_w_window", &signal.w_window.to_string())
                        .with_metadata("model_type", model_type_used)
                        .with_metadata("model_version", self.config.model_version.as_deref().unwrap_or(""))
                        .with_metadata("signal_edge", &signal.edge.to_string())
                        .with_metadata("signal_edge_gross", &signal.gross_edge.to_string())
                        .with_metadata("signal_up_edge_gross", &signal.up_edge_gross.to_string())
                        .with_metadata("signal_down_edge_gross", &signal.down_edge_gross.to_string())
                        .with_metadata("signal_up_edge_net", &signal.up_edge_net.to_string())
                        .with_metadata("signal_down_edge_net", &signal.down_edge_net.to_string())
                        .with_metadata("signal_confidence", &signal.signal_confidence.to_string())
                        .with_metadata("signal_fair_value", &signal.fair_value.to_string())
                        .with_metadata("signal_market_price", &signal.limit_price.to_string())
                        .with_metadata("cost_taker_fee_rate", &self.config.taker_fee_rate.to_string())
                        .with_metadata(
                            "cost_entry_slippage_bps",
                            &self.config.entry_slippage_bps.to_string(),
                        )
                        .with_metadata("pm_up_ask", &signal.up_ask.to_string())
                        .with_metadata("pm_down_ask", &signal.down_ask.to_string())
                        .with_metadata("lob_best_bid", &lob.best_bid.to_string())
                        .with_metadata("lob_best_ask", &lob.best_ask.to_string())
                        .with_metadata("lob_mid_price", &lob.mid_price.to_string())
                        .with_metadata("lob_spread_bps", &lob.spread_bps.to_string())
                        .with_metadata("lob_obi_5", &lob.obi_5.to_string())
                        .with_metadata("lob_obi_10", &lob.obi_10.to_string())
                        .with_metadata("lob_obi_1", &obi_1.to_string())
                        .with_metadata("lob_obi_2", &obi_2.to_string())
                        .with_metadata("lob_obi_3", &obi_3.to_string())
                        .with_metadata("lob_obi_20", &obi_20.to_string())
                        .with_metadata("lob_obi_micro", &obi_micro.to_string())
                        .with_metadata("lob_obi_slope", &obi_slope.to_string())
                        .with_metadata("lob_bid_volume_5", &lob.bid_volume_5.to_string())
                        .with_metadata("lob_ask_volume_5", &lob.ask_volume_5.to_string())
                        .with_metadata("signal_momentum_1s", &momentum_1s.to_string())
                        .with_metadata("signal_momentum_5s", &momentum_5s.to_string())
                        .with_metadata("window_start_price", &window_ctx.start_price.to_string())
                        .with_metadata("window_move_pct", &window_ctx.window_move.to_string())
                        .with_metadata("window_elapsed_secs", &window_ctx.elapsed_secs.to_string())
                        .with_metadata("window_remaining_secs", &window_ctx.remaining_secs.to_string())
                        .with_metadata("config_hash", &config_hash);

                        info!(
                            agent = self.config.agent_id,
                            slug = %event.slug,
                            horizon = %event.horizon,
                            side = %signal.side,
                            limit_price = %signal.limit_price,
                            net_edge = %signal.edge,
                            gross_edge = %signal.gross_edge,
                            p_up = %signal.p_up_blended,
                            model = model_type_used,
                            "lob-ml signal detected, submitting order"
                        );

                        if let Err(e) = ctx.submit_order(intent).await {
                            warn!(agent = self.config.agent_id, error = %e, "failed to submit order");
                            continue;
                        }

                        let now = Utc::now();
                        last_trade_by_key.insert(entry_key, now);
                        traded_events.insert(event.slug.clone(), now);
                    }
                }

                // --- Polymarket quote updates (exit decisions) ---
                result = pm_rx.recv() => {
                    let update = match result {
                        Ok(u) => u,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(lagged = n, "pm rx lagged");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            error!(agent = self.config.agent_id, "pm feed closed");
                            break;
                        }
                    };

                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }

                    if !self.allows_price_exit() {
                        continue;
                    }

                    let Some(best_bid) = update.quote.best_bid else {
                        continue;
                    };

                    // Find position by token id.
                    let position_key = positions
                        .iter()
                        .find_map(|(slug, pos)| (pos.token_id == update.token_id).then(|| slug.clone()));

                    let Some(slug) = position_key else {
                        continue;
                    };
                    let Some(pos) = positions.get(&slug).cloned() else {
                        continue;
                    };

                    if pos.entry_price <= Decimal::ZERO {
                        continue;
                    }

                    let held_secs = Utc::now().signed_duration_since(pos.entry_time).num_seconds();
                    if held_secs < self.config.min_hold_secs as i64 {
                        continue;
                    }

                    let pnl_pct = (best_bid - pos.entry_price) / pos.entry_price;
                    let maybe_reason = if pnl_pct >= self.config.exit_edge_floor {
                        Some(("exit_edge_floor", OrderPriority::High))
                    } else if pnl_pct <= -self.config.exit_price_band {
                        Some(("exit_price_band", OrderPriority::Critical))
                    } else {
                        None
                    };

                    let Some((exit_reason, priority)) = maybe_reason else {
                        continue;
                    };

                    let intent = OrderIntent::new(
                        &self.config.agent_id,
                        Domain::Crypto,
                        &pos.market_slug,
                        &pos.token_id,
                        pos.side,
                        false,
                        pos.shares,
                        best_bid,
                    );
                    let position_coin = pos.symbol.replace("USDT", "");
                    let deployment_id = deployment_id_for(STRATEGY_ID, &position_coin, &pos.horizon);
                    let timeframe = normalize_timeframe(&pos.horizon);
                    let event_window_secs = event_window_secs_for_horizon(&timeframe).to_string();
                    let intent = intent
                    .with_priority(priority)
                    .with_metadata("strategy", STRATEGY_ID)
                    .with_metadata("deployment_id", &deployment_id)
                    .with_metadata("timeframe", &timeframe)
                    .with_metadata("event_window_secs", &event_window_secs)
                    .with_metadata("signal_type", "crypto_lob_ml_exit")
                    .with_metadata("coin", &position_coin)
                    .with_metadata("symbol", &pos.symbol)
                    .with_metadata("series_id", &pos.series_id)
                    .with_metadata("event_series_id", &pos.series_id)
                    .with_metadata("horizon", &pos.horizon)
                    .with_metadata("exit_mode", self.exit_mode_label())
                    .with_metadata("exit_reason", exit_reason)
                    .with_metadata("entry_price", &pos.entry_price.to_string())
                    .with_metadata("exit_price", &best_bid.to_string())
                    .with_metadata("pnl_pct", &pnl_pct.to_string())
                    .with_metadata("held_secs", &held_secs.to_string())
                    .with_metadata("config_hash", &config_hash);

                    match ctx.submit_order(intent).await {
                        Ok(()) => {
                            info!(
                                agent = self.config.agent_id,
                                slug = %slug,
                                reason = exit_reason,
                                pnl_pct = %pnl_pct,
                                "exit signal triggered, submitting sell order"
                            );
                        }
                        Err(e) => {
                            warn!(
                                agent = self.config.agent_id,
                                slug = %slug,
                                error = %e,
                                "failed to submit exit order"
                            );
                        }
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
                            sync_positions_from_global(
                                &ctx,
                                &self.config.agent_id,
                                &mut positions,
                            )
                            .await;
                            let quote_cache = self.pm_ws.quote_cache();
                            for (slug, pos) in &positions {
                                let bid = quote_cache
                                    .get(&pos.token_id)
                                    .and_then(|q| q.best_bid)
                                    .unwrap_or(dec!(0.01));
                                let intent = OrderIntent::new(
                                    &self.config.agent_id,
                                    Domain::Crypto,
                                    slug.as_str(),
                                    &pos.token_id,
                                    pos.side,
                                    false,
                                    pos.shares,
                                    bid,
                                );
                                let position_coin = pos.symbol.replace("USDT", "");
                                let deployment_id =
                                    deployment_id_for(STRATEGY_ID, &position_coin, &pos.horizon);
                                let timeframe = normalize_timeframe(&pos.horizon);
                                let event_window_secs =
                                    event_window_secs_for_horizon(&timeframe).to_string();
                                let intent = intent
                                .with_priority(OrderPriority::Critical)
                                .with_metadata("strategy", STRATEGY_ID)
                                .with_metadata("deployment_id", &deployment_id)
                                .with_metadata("timeframe", &timeframe)
                                .with_metadata("event_window_secs", &event_window_secs)
                                .with_metadata("signal_type", "crypto_lob_ml_exit")
                                .with_metadata("coin", &position_coin)
                                .with_metadata("symbol", &pos.symbol)
                                .with_metadata("series_id", &pos.series_id)
                                .with_metadata("event_series_id", &pos.series_id)
                                .with_metadata("horizon", &pos.horizon)
                                .with_metadata("exit_mode", self.exit_mode_label())
                                .with_metadata("exit_reason", "force_close")
                                .with_metadata("entry_price", &pos.entry_price.to_string())
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
                            positions.clear();
                            break;
                        }
                        Some(CoordinatorCommand::HealthCheck(tx)) => {
                            let total_exposure = sync_positions_from_global(
                                &ctx,
                                &self.config.agent_id,
                                &mut positions,
                            )
                            .await;
                            let mut metrics = HashMap::new();
                            metrics.insert(
                                "model_type_configured".to_string(),
                                self.config.model_type.clone(),
                            );
                            metrics.insert(
                                "exit_mode".to_string(),
                                self.exit_mode_label().to_string(),
                            );
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: self.config.name.clone(),
                                domain: Domain::Crypto,
                                status,
                                position_count: positions.len(),
                                exposure: total_exposure,
                                daily_pnl,
                                unrealized_pnl: Decimal::ZERO,
                                metrics,
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
                        None => {
                            warn!(agent = self.config.agent_id, "command channel closed");
                            break;
                        }
                    }
                }

                // --- Heartbeat ---
                _ = heartbeat_tick.tick() => {
                    let total_exposure = sync_positions_from_global(
                        &ctx,
                        &self.config.agent_id,
                        &mut positions,
                    )
                    .await;
                    let mut metrics = HashMap::new();
                    metrics.insert(
                        "model_type_configured".to_string(),
                        self.config.model_type.clone(),
                    );
                    metrics.insert(
                        "exit_mode".to_string(),
                        self.exit_mode_label().to_string(),
                    );
                    let _ = ctx.report_state_with_metrics(
                        &self.config.name,
                        status,
                        positions.len(),
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
                        metrics,
                        None,
                    ).await;
                }
            }
        }

        info!(agent = self.config.agent_id, "crypto lob-ml agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window_context() -> WindowContext {
        WindowContext {
            now: Utc::now(),
            start_price: dec!(100),
            window_move: dec!(0.01),
            elapsed_secs: 30,
            remaining_secs: 270,
            p_up_window: dec!(0.60),
        }
    }

    #[test]
    fn test_config_defaults() {
        let cfg = CryptoLobMlConfig::default();
        assert_eq!(cfg.agent_id, "crypto_lob_ml");
        assert_eq!(cfg.coins, vec!["BTC", "ETH", "SOL", "XRP"]);
        assert_eq!(cfg.max_time_remaining_secs, 900);
        assert_eq!(cfg.exit_mode, CryptoLobMlExitMode::SettleOnly);
        assert_eq!(cfg.min_hold_secs, 20);
        assert!(cfg.prefer_close_to_end);
    }

    #[test]
    fn test_exit_mode_gate_helpers() {
        let mk_agent = |exit_mode: CryptoLobMlExitMode| CryptoLobMlAgent {
            config: CryptoLobMlConfig {
                exit_mode,
                ..CryptoLobMlConfig::default()
            },
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };

        let settle = mk_agent(CryptoLobMlExitMode::SettleOnly);
        assert!(!settle.allows_signal_flip_exit());
        assert!(!settle.allows_price_exit());

        let flip = mk_agent(CryptoLobMlExitMode::SignalFlip);
        assert!(flip.allows_signal_flip_exit());
        assert!(!flip.allows_price_exit());

        let price = mk_agent(CryptoLobMlExitMode::PriceExit);
        assert!(!price.allows_signal_flip_exit());
        assert!(price.allows_price_exit());
    }

    #[test]
    fn test_new_requires_onnx_model_type() {
        let mut cfg = CryptoLobMlConfig::default();
        cfg.model_type = "mlp_json".to_string();
        cfg.model_path = Some("/tmp/does-not-matter.onnx".to_string());

        let result = CryptoLobMlAgent::new(
            cfg,
            Arc::new(BinanceWebSocket::new(vec![])),
            Arc::new(PolymarketWebSocket::new("wss://example.com")),
            Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            LobCache::new(),
        );
        assert!(result.is_err(), "non-onnx model_type must be rejected");
        let err = result.err().expect("result should be err");

        assert!(err.to_string().contains("model_type=onnx"));
    }

    #[test]
    fn test_new_requires_model_path() {
        let mut cfg = CryptoLobMlConfig::default();
        cfg.model_type = "onnx".to_string();
        cfg.model_path = None;

        let result = CryptoLobMlAgent::new(
            cfg,
            Arc::new(BinanceWebSocket::new(vec![])),
            Arc::new(PolymarketWebSocket::new("wss://example.com")),
            Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            LobCache::new(),
        );
        assert!(result.is_err(), "missing model_path must be rejected");
        let err = result.err().expect("result should be err");
        #[cfg(feature = "onnx")]
        assert!(err.to_string().contains("MODEL_PATH"));
        #[cfg(not(feature = "onnx"))]
        assert!(err.to_string().contains("--features onnx"));
    }

    #[test]
    fn test_model_first_blend_weights_default() {
        let agent = CryptoLobMlAgent {
            config: CryptoLobMlConfig::default(),
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };

        let (w_model, w_window) = agent.model_window_blend_weights();
        assert_eq!(w_model, dec!(0.90));
        assert_eq!(w_window, dec!(0.10));
    }

    #[test]
    fn test_evaluate_entry_signal_prefers_higher_edge() {
        let agent = CryptoLobMlAgent {
            config: CryptoLobMlConfig::default(),
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };

        let signal = agent
            .evaluate_entry_signal(
                dec!(0.62),
                &sample_window_context(),
                Some(dec!(0.58)),
                "up-token",
                "down-token",
                dec!(0.52),
                dec!(0.49),
            )
            .expect("signal should pass filters");

        assert_eq!(signal.side, Side::Up);
        assert_eq!(signal.token_id, "up-token");
        assert!(signal.edge >= dec!(0.02));
    }

    #[test]
    fn test_evaluate_entry_signal_rejects_expensive_entry() {
        let agent = CryptoLobMlAgent {
            config: CryptoLobMlConfig::default(),
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };

        let signal = agent.evaluate_entry_signal(
            dec!(0.99),
            &sample_window_context(),
            Some(dec!(0.50)),
            "up-token",
            "down-token",
            dec!(0.80),
            dec!(0.95),
        );
        assert!(signal.is_none());
    }

    #[test]
    fn test_evaluate_entry_signal_requires_price_to_beat_when_enabled() {
        let mut cfg = CryptoLobMlConfig::default();
        cfg.use_price_to_beat = true;
        cfg.require_price_to_beat = true;
        let agent = CryptoLobMlAgent {
            config: cfg,
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };

        let signal = agent.evaluate_entry_signal(
            dec!(0.60),
            &sample_window_context(),
            None,
            "up-token",
            "down-token",
            dec!(0.51),
            dec!(0.49),
        );
        assert!(signal.is_none());
    }

    #[test]
    fn test_should_skip_entry_when_event_already_traded() {
        let mut positions: HashMap<String, TrackedPosition> = HashMap::new();
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();
        let last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();
        let now = Utc::now();

        traded_events.insert("btc-updown-5m-1".to_string(), now);

        let skip = should_skip_entry(
            "btc-updown-5m-1",
            "BTCUSDT",
            now,
            &positions,
            &traded_events,
            &last_trade_by_symbol,
            30,
        );
        assert!(skip);

        positions.insert(
            "btc-updown-5m-2".to_string(),
            TrackedPosition {
                market_slug: "btc-updown-5m-2".to_string(),
                symbol: "BTCUSDT".to_string(),
                horizon: "5m".to_string(),
                series_id: "10684".to_string(),
                token_id: "token".to_string(),
                side: Side::Up,
                shares: 1,
                entry_price: dec!(0.5),
                entry_time: now,
            },
        );

        let skip_open_position = should_skip_entry(
            "btc-updown-5m-2",
            "BTCUSDT",
            now,
            &positions,
            &traded_events,
            &last_trade_by_symbol,
            30,
        );
        assert!(skip_open_position);
    }

    #[test]
    fn test_prune_stale_traded_events() {
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();
        let now = Utc::now();
        traded_events.insert(
            "stale".to_string(),
            now - chrono::Duration::hours(TRADED_EVENT_RETENTION_HOURS + 1),
        );
        traded_events.insert("fresh".to_string(), now);

        prune_stale_traded_events(&mut traded_events, now);

        assert!(!traded_events.contains_key("stale"));
        assert!(traded_events.contains_key("fresh"));
    }

    #[test]
    fn test_deployment_metadata_helpers() {
        assert_eq!(normalize_timeframe("15"), "15m");
        assert_eq!(normalize_timeframe("btc-5m"), "5m");
        assert_eq!(event_window_secs_for_horizon("15m"), 900);
        assert_eq!(event_window_secs_for_horizon("5m"), 300);
        assert_eq!(
            deployment_id_for("crypto_lob_ml", "ETH", "5m"),
            "crypto.pm.eth.5m.crypto_lob_ml"
        );
    }
}
