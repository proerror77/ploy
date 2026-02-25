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
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::adapters::{BinanceWebSocket, PolymarketWebSocket, PriceUpdate, QuoteUpdate, SpotPrice};
use crate::agents::{AgentContext, TradingAgent};
use crate::collector::LobCache;
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::{PloyError, Result};
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

const TRADED_EVENT_RETENTION_HOURS: i64 = 24;
const STRATEGY_ID: &str = "crypto_lob_ml";
const SEQ_LEN_5M: usize = 60;
const SEQ_LEN_15M: usize = 180;
const SEQ_FEATURE_DIM: usize = 11;

/// Standard normal CDF approximation (Abramowitz-Stegun), ~4dp accuracy.
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;

    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

fn default_exit_edge_floor() -> Decimal {
    dec!(0.15)
}

fn default_exit_price_band() -> Decimal {
    dec!(0.25)
}

fn default_trailing_pullback_pct() -> Decimal {
    dec!(0.15)
}

fn default_trailing_time_decay() -> Decimal {
    dec!(0.50)
}

fn default_max_time_remaining_secs_5m() -> u64 {
    120
}

fn default_max_time_remaining_secs_15m() -> u64 {
    240
}

fn default_oracle_lag_buffer_secs() -> u64 {
    3
}

fn default_max_spread_pct() -> Decimal {
    dec!(0.10)
}

fn default_force_settle_only_5m() -> bool {
    true
}

/// Bias added to P(UP) for >= settlement rule (UP wins on ties).
const GEQ_SETTLEMENT_BIAS: f64 = 0.002;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoLobMlExitMode {
    /// Hold position until market settlement.
    SettleOnly,
    /// Exit when model says current market is overpriced (EV turns negative).
    EvExit,
    /// Exit when model side flips (after min hold time).
    SignalFlip,
    /// Trailing exit: track peak bid, exit on pullback. Tightens as settlement nears.
    TrailingExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoLobMlEntrySidePolicy {
    /// Choose the side with the highest net EV.
    BestEv,
    /// Only consider the cheaper side (lagging price) for entry.
    LaggingOnly,
}

fn default_exit_mode() -> CryptoLobMlExitMode {
    // Model-driven exit by default (pure ML policy).
    CryptoLobMlExitMode::EvExit
}

fn default_entry_side_policy() -> CryptoLobMlEntrySidePolicy {
    CryptoLobMlEntrySidePolicy::LaggingOnly
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

    /// Optional override: maximum time remaining for 5m events (seconds).
    #[serde(default = "default_max_time_remaining_secs_5m")]
    pub max_time_remaining_secs_5m: u64,

    /// Optional override: maximum time remaining for 15m events (seconds).
    #[serde(default = "default_max_time_remaining_secs_15m")]
    pub max_time_remaining_secs_15m: u64,

    /// If true, prefer events closest to end (confirmatory mode).
    /// If false, prefer events with more time remaining (predictive mode).
    pub prefer_close_to_end: bool,

    pub default_shares: u64,
    /// (Legacy, kept for config compat) Fixed take-profit PnL% threshold.
    #[serde(default = "default_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    /// (Legacy, kept for config compat) Fixed stop-loss PnL% threshold.
    #[serde(default = "default_exit_price_band")]
    pub exit_price_band: Decimal,

    /// Trailing exit: base pullback % from peak bid that triggers exit (e.g. 0.15 = 15%).
    #[serde(default = "default_trailing_pullback_pct")]
    pub trailing_pullback_pct: Decimal,
    /// Trailing exit: time-decay factor. As remaining time → 0, pullback tolerance shrinks
    /// by up to this fraction. 0.50 means at settlement the effective pullback threshold
    /// is halved (e.g. 15% → 7.5%).
    #[serde(default = "default_trailing_time_decay")]
    pub trailing_time_decay: Decimal,

    /// Exit policy:
    /// - settle_only: hold until settlement (pure binary payoff)
    /// - ev_exit: exit when model EV turns negative
    /// - signal_flip: exit on side flip
    /// - trailing_exit: track peak bid, exit on pullback (tightens near settlement)
    #[serde(default = "default_exit_mode")]
    pub exit_mode: CryptoLobMlExitMode,
    /// Minimum hold time before exits are allowed (seconds)
    pub min_hold_secs: u64,

    /// Minimum expected-value edge required to enter.
    /// This is measured on net EV after fee/slippage adjustments.
    pub min_edge: Decimal,

    /// Max ask price to pay for entry (YES/NO).
    pub max_entry_price: Decimal,

    /// Entry-side selection policy (best_ev or lagging_only).
    #[serde(default = "default_entry_side_policy")]
    pub entry_side_policy: CryptoLobMlEntrySidePolicy,

    /// For 5m markets, only allow entries in the last N seconds of the window.
    #[serde(default = "default_entry_late_window_secs_5m")]
    pub entry_late_window_secs_5m: u64,

    /// For 15m markets, only allow entries in the last N seconds of the window.
    #[serde(default = "default_entry_late_window_secs_15m")]
    pub entry_late_window_secs_15m: u64,

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

    /// Blend weight for the ONNX model vs GBM anchor probability.
    /// p_final = w_model × p_model + (1 - w_model) × p_gbm_anchor
    #[serde(
        default = "default_model_blend_weight",
        alias = "threshold_prob_weight"
    )]
    pub model_blend_weight: Decimal,

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

    /// Expected SHA256 hex digest of the model file. On startup, if the model file
    /// exists and this is set, the actual hash is compared and a warning logged on mismatch.
    #[serde(default)]
    pub model_sha256: Option<String>,

    /// ISO8601 timestamp of when the model was trained.
    #[serde(default)]
    pub model_trained_at: Option<String>,

    /// Validation AUC of the model (recorded in order metadata for post-hoc analysis).
    #[serde(default)]
    pub model_auc: Option<f64>,

    /// Per-feature offsets for normalization: normalized = (raw - offset) * scale.
    /// Length must equal SEQ_FEATURE_DIM (11). Empty → identity (no normalization).
    #[serde(default)]
    pub feature_offsets: Vec<f32>,

    /// Per-feature scales for normalization: normalized = (raw - offset) * scale.
    /// Length must equal SEQ_FEATURE_DIM (11). Empty → identity (no normalization).
    #[serde(default)]
    pub feature_scales: Vec<f32>,

    /// Safety fallback blend weight for window baseline probability (DEPRECATED).
    /// Kept for config backward-compat deserialization but unused in 2-layer blend.
    #[serde(default = "default_window_fallback_weight")]
    pub _window_fallback_weight_compat: Decimal,

    /// Minimum positive EV gap required to trigger EV exit.
    #[serde(default = "default_ev_exit_buffer")]
    pub ev_exit_buffer: Decimal,

    /// Volatility-scaled EV exit buffer component (uses window uncertainty).
    #[serde(default = "default_ev_exit_vol_scale")]
    pub ev_exit_vol_scale: Decimal,

    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,

    /// Oracle lag buffer (seconds) added to remaining-time uncertainty near settlement.
    #[serde(default = "default_oracle_lag_buffer_secs")]
    pub oracle_lag_buffer_secs: u64,

    /// Maximum bid-ask spread percentage per side to filter thin markets.
    #[serde(default = "default_max_spread_pct")]
    pub max_spread_pct: Decimal,

    /// When true, force SettleOnly exit mode for 5m events regardless of configured exit_mode.
    #[serde(default = "default_force_settle_only_5m")]
    pub force_settle_only_5m: bool,
}

fn default_lob_ml_model_type() -> String {
    "onnx".to_string()
}

fn default_window_fallback_weight() -> Decimal {
    dec!(0.10)
}

fn default_ev_exit_buffer() -> Decimal {
    dec!(0.005)
}

fn default_ev_exit_vol_scale() -> Decimal {
    dec!(0.02)
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


fn default_model_blend_weight() -> Decimal {
    dec!(0.80)
}

fn default_entry_late_window_secs_5m() -> u64 {
    180
}

fn default_entry_late_window_secs_15m() -> u64 {
    180
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
            max_time_remaining_secs_5m: default_max_time_remaining_secs_5m(),
            max_time_remaining_secs_15m: default_max_time_remaining_secs_15m(),
            prefer_close_to_end: true,
            default_shares: 50,
            exit_edge_floor: default_exit_edge_floor(),
            exit_price_band: default_exit_price_band(),
            trailing_pullback_pct: default_trailing_pullback_pct(),
            trailing_time_decay: default_trailing_time_decay(),
            exit_mode: default_exit_mode(),
            min_hold_secs: 20,
            min_edge: dec!(0.02),
            max_entry_price: dec!(0.70),
            entry_side_policy: default_entry_side_policy(),
            entry_late_window_secs_5m: default_entry_late_window_secs_5m(),
            entry_late_window_secs_15m: default_entry_late_window_secs_15m(),
            taker_fee_rate: default_taker_fee_rate(),
            entry_slippage_bps: default_entry_slippage_bps(),
            use_price_to_beat: default_use_price_to_beat(),
            require_price_to_beat: default_require_price_to_beat(),
            model_blend_weight: default_model_blend_weight(),
            cooldown_secs: 30,
            max_lob_snapshot_age_secs: 2,
            model_type: default_lob_ml_model_type(),
            model_path: None,
            model_version: None,
            model_sha256: None,
            model_trained_at: None,
            model_auc: None,
            feature_offsets: vec![],
            feature_scales: vec![],
            _window_fallback_weight_compat: default_window_fallback_weight(),
            ev_exit_buffer: default_ev_exit_buffer(),
            ev_exit_vol_scale: default_ev_exit_vol_scale(),
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
            oracle_lag_buffer_secs: default_oracle_lag_buffer_secs(),
            max_spread_pct: default_max_spread_pct(),
            force_settle_only_5m: default_force_settle_only_5m(),
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
    /// Highest bid seen since entry (for trailing exit).
    peak_bid: Decimal,
}

#[derive(Debug, Clone)]
struct WindowContext {
    now: DateTime<Utc>,
    start_price: Decimal,
    window_move: Decimal,
    elapsed_secs: i64,
    remaining_secs: i64,
}

#[derive(Debug, Clone)]
struct BlendedProb {
    p_up_blended: Decimal,
    p_gbm_anchor: Decimal,
    w_model: Decimal,
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
    p_gbm_anchor: Decimal,
    p_up_blended: Decimal,
    w_model: Decimal,
    up_edge_gross: Decimal,
    down_edge_gross: Decimal,
    up_edge_net: Decimal,
    down_edge_net: Decimal,
    up_ask: Decimal,
    down_ask: Decimal,
}

#[derive(Debug, Clone)]
struct SequenceSnapshot {
    ts: DateTime<Utc>,
    obi_5: Decimal,
    obi_10: Decimal,
    spread_bps: Decimal,
    bid_volume_5: Decimal,
    ask_volume_5: Decimal,
    momentum_1s: Decimal,
    momentum_5s: Decimal,
    spot_price: Decimal,
    remaining_secs: Decimal,
    price_to_beat: Decimal,
    distance_to_beat: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceAlignMode {
    Exact,
    TruncateOldest,
    LeftPadZero,
}

fn sequence_len_for_horizon(horizon: &str) -> usize {
    match normalize_timeframe(horizon).as_str() {
        "15m" => SEQ_LEN_15M,
        _ => SEQ_LEN_5M,
    }
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
        peak_bid: position.entry_price,
    }
}

async fn sync_positions_from_global(
    ctx: &AgentContext,
    agent_id: &str,
    positions: &mut HashMap<String, TrackedPosition>,
) -> Decimal {
    let state = ctx.read_global_state().await;

    // Snapshot existing peak_bid values so trailing exits survive sync.
    let prev_peaks: HashMap<String, Decimal> = positions
        .iter()
        .map(|(slug, tp)| (slug.clone(), tp.peak_bid))
        .collect();

    positions.clear();
    for position in state.positions {
        if position.agent_id != agent_id
            || position.domain != Domain::Crypto
            || position.shares == 0
        {
            continue;
        }
        let mut tp = tracked_position_from_global(&position);
        // Restore the peak_bid we tracked locally (higher of old peak and entry).
        if let Some(&prev_peak) = prev_peaks.get(&position.market_slug) {
            if prev_peak > tp.peak_bid {
                tp.peak_bid = prev_peak;
            }
        }
        positions.insert(position.market_slug.clone(), tp);
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

            let configured_input_dim = std::env::var("PLOY_CRYPTO_LOB_ML__MODEL_INPUT_DIM")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(SEQ_LEN_5M * SEQ_FEATURE_DIM);

            let m = OnnxModel::load_for_vec_input(model_path, configured_input_dim)?;
            info!(
                agent = config.agent_id,
                model_type = "onnx",
                model_path = %model_path,
                input_dim = m.input_dim(),
                output_dim = m.output_dim(),
                "loaded lob-ml onnx model"
            );

            // Model SHA256 integrity check (warn-only, non-blocking)
            if let Some(ref expected_hash) = config.model_sha256 {
                match std::fs::read(model_path) {
                    Ok(bytes) => {
                        use sha2::{Digest, Sha256};
                        let actual = format!("{:x}", Sha256::digest(&bytes));
                        if actual != expected_hash.to_lowercase() {
                            warn!(
                                agent = config.agent_id,
                                expected = %expected_hash,
                                actual = %actual,
                                "model SHA256 mismatch — config may be stale"
                            );
                        } else {
                            info!(agent = config.agent_id, "model SHA256 verified");
                        }
                    }
                    Err(e) => {
                        warn!(agent = config.agent_id, error = %e, "could not read model for SHA256 check");
                    }
                }
            }

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

    fn push_sequence_snapshot(
        sequence_cache: &mut HashMap<String, VecDeque<SequenceSnapshot>>,
        key: &str,
        snapshot: SequenceSnapshot,
    ) {
        let window = sequence_cache.entry(key.to_string()).or_default();
        if let Some(last) = window.back_mut() {
            // Keep one row per second key to avoid dense duplicate ticks.
            if last.ts == snapshot.ts {
                *last = snapshot;
                return;
            }
        }

        window.push_back(snapshot);
        while window.len() > SEQ_LEN_15M {
            let _ = window.pop_front();
        }
    }

    fn build_sequence(
        sequence_cache: &HashMap<String, VecDeque<SequenceSnapshot>>,
        key: &str,
        horizon: &str,
        feature_offsets: &[f32],
        feature_scales: &[f32],
    ) -> Option<Vec<f32>> {
        let seq_len = sequence_len_for_horizon(horizon);
        let window = sequence_cache.get(key)?;
        if window.len() < seq_len {
            return None;
        }

        let normalize = feature_offsets.len() == SEQ_FEATURE_DIM
            && feature_scales.len() == SEQ_FEATURE_DIM;

        let mut flat: Vec<f32> = Vec::with_capacity(seq_len * SEQ_FEATURE_DIM);
        let start_idx = window.len().saturating_sub(seq_len);
        for snap in window.iter().skip(start_idx) {
            let raw = [
                snap.obi_5.to_f64().unwrap_or(0.0) as f32,
                snap.obi_10.to_f64().unwrap_or(0.0) as f32,
                snap.spread_bps.to_f64().unwrap_or(0.0) as f32,
                snap.bid_volume_5.to_f64().unwrap_or(0.0) as f32,
                snap.ask_volume_5.to_f64().unwrap_or(0.0) as f32,
                snap.momentum_1s.to_f64().unwrap_or(0.0) as f32,
                snap.momentum_5s.to_f64().unwrap_or(0.0) as f32,
                snap.spot_price.to_f64().unwrap_or(0.0) as f32,
                snap.remaining_secs.to_f64().unwrap_or(0.0) as f32,
                snap.price_to_beat.to_f64().unwrap_or(0.0) as f32,
                snap.distance_to_beat.to_f64().unwrap_or(0.0) as f32,
            ];
            if normalize {
                for (i, v) in raw.iter().enumerate() {
                    flat.push((v - feature_offsets[i]) * feature_scales[i]);
                }
            } else {
                flat.extend_from_slice(&raw);
            }
        }

        Some(flat)
    }

    #[cfg(feature = "onnx")]
    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    fn estimate_p_up_gbm_anchor(
        spot_price: Decimal,
        start_price: Decimal,
        price_to_beat: Option<Decimal>,
        sigma_1s: Option<Decimal>,
        remaining_secs: i64,
        oracle_lag_buffer_secs: u64,
    ) -> Decimal {
        let remaining_secs = remaining_secs.max(0) as f64;
        let Some(sig_1s) = sigma_1s.and_then(|v| v.to_f64()) else {
            return dec!(0.50);
        };
        if !sig_1s.is_finite() || sig_1s <= 0.0 {
            return dec!(0.50);
        }

        // Oracle lag buffer: near settlement, inflate remaining-time uncertainty
        // to account for Chainlink vs Binance price delay.
        let effective_remaining = if remaining_secs < 30.0 {
            remaining_secs + (oracle_lag_buffer_secs as f64)
        } else {
            remaining_secs
        };
        let sigma_rem = sig_1s * effective_remaining.sqrt();
        if !sigma_rem.is_finite() || sigma_rem <= 0.0 {
            return dec!(0.50);
        }

        let spot = spot_price.to_f64().unwrap_or(0.0);
        if !spot.is_finite() || spot <= 0.0 {
            return dec!(0.50);
        }

        // With price_to_beat: P(spot_at_end > price_to_beat) = 1 - Φ(required_return / σ)
        // Fallback (no price_to_beat): P(UP) = Φ(window_move / σ)
        if let Some(beat) = price_to_beat {
            let beat_f = beat.to_f64().unwrap_or(0.0);
            if beat_f.is_finite() && beat_f > 0.0 {
                let required_return = (beat_f - spot) / spot;
                if required_return.is_finite() {
                    let p = (1.0 - normal_cdf(required_return / sigma_rem)).clamp(0.001, 0.999);
                    return Decimal::from_f64_retain(p).unwrap_or(dec!(0.50));
                }
            }
        }

        // Fallback: use window_move = (spot - start) / start
        let start_f = start_price.to_f64().unwrap_or(0.0);
        if !start_f.is_finite() || start_f <= 0.0 {
            return dec!(0.50);
        }
        let window_move = (spot - start_f) / start_f;
        if !window_move.is_finite() {
            return dec!(0.50);
        }

        let mut p = normal_cdf(window_move / sigma_rem).clamp(0.001, 0.999);

        // >= settlement bias: UP wins on ties (close >= open).
        // Only significant for pure UP/DOWN events (no price_to_beat).
        p += GEQ_SETTLEMENT_BIAS;

        Decimal::from_f64_retain(p.clamp(0.001, 0.999)).unwrap_or(dec!(0.50))
    }

    fn align_sequence_to_model_input(
        sequence: &[f32],
        model_input_dim: usize,
    ) -> Result<(Vec<f32>, SequenceAlignMode)> {
        if model_input_dim == 0 {
            return Err(PloyError::Validation(
                "onnx model input_dim must be > 0".to_string(),
            ));
        }
        if model_input_dim % SEQ_FEATURE_DIM != 0 {
            return Err(PloyError::Validation(format!(
                "onnx model input_dim {} must be a multiple of sequence feature dim {}",
                model_input_dim, SEQ_FEATURE_DIM
            )));
        }
        if sequence.len() % SEQ_FEATURE_DIM != 0 {
            return Err(PloyError::Validation(format!(
                "sequence input dim {} must be a multiple of sequence feature dim {}",
                sequence.len(),
                SEQ_FEATURE_DIM
            )));
        }

        let model_snapshots = model_input_dim / SEQ_FEATURE_DIM;
        let sequence_snapshots = sequence.len() / SEQ_FEATURE_DIM;

        if sequence_snapshots == model_snapshots {
            return Ok((sequence.to_vec(), SequenceAlignMode::Exact));
        }

        if sequence_snapshots > model_snapshots {
            let start_snapshot = sequence_snapshots - model_snapshots;
            let start = start_snapshot * SEQ_FEATURE_DIM;
            return Ok((
                sequence[start..].to_vec(),
                SequenceAlignMode::TruncateOldest,
            ));
        }

        let pad_snapshots = model_snapshots - sequence_snapshots;
        let mut aligned = vec![0.0f32; pad_snapshots * SEQ_FEATURE_DIM];
        aligned.extend_from_slice(sequence);
        Ok((aligned, SequenceAlignMode::LeftPadZero))
    }

    /// Estimate p(UP) using ONNX model from a flattened sequence input.
    fn estimate_p_up(&self, horizon: &str, sequence: &[f32]) -> Result<(f64, &'static str)> {
        #[cfg(feature = "onnx")]
        {
            let expected_len = sequence_len_for_horizon(horizon) * SEQ_FEATURE_DIM;
            if sequence.len() != expected_len {
                return Err(PloyError::Validation(format!(
                    "sequence input dim mismatch: got {}, expected {} for horizon {}",
                    sequence.len(),
                    expected_len,
                    normalize_timeframe(horizon),
                )));
            }

            let m = self
                .onnx_model
                .as_ref()
                .ok_or_else(|| PloyError::InvalidState("onnx model not initialized".to_string()))?;

            let (aligned, align_mode) =
                Self::align_sequence_to_model_input(sequence, m.input_dim())?;
            if align_mode != SequenceAlignMode::Exact {
                debug!(
                    agent = self.config.agent_id,
                    horizon = normalize_timeframe(horizon),
                    sequence_len = sequence.len(),
                    model_input_dim = m.input_dim(),
                    align_mode = ?align_mode,
                    "aligned sequence input to onnx model input dim"
                );
            }

            let raw = m.predict_scalar(&aligned)?;
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
            let _ = (horizon, sequence);
            Err(PloyError::Validation(
                "crypto_lob_ml requires --features onnx".to_string(),
            ))
        }
    }

    fn model_blend_weight_clamped(&self) -> Decimal {
        self.config
            .model_blend_weight
            .max(dec!(0.01))
            .min(dec!(0.99))
    }

    fn entry_late_window_secs(&self, horizon: &str) -> u64 {
        match normalize_timeframe(horizon).as_str() {
            "15m" => self.config.entry_late_window_secs_15m,
            _ => self.config.entry_late_window_secs_5m,
        }
    }

    fn is_within_entry_late_window(&self, horizon: &str, remaining_secs: i64) -> bool {
        if remaining_secs <= 0 {
            return false;
        }

        let window_secs = self.entry_late_window_secs(horizon);
        window_secs == 0 || remaining_secs as u64 <= window_secs
    }

    fn allows_signal_flip_exit(&self) -> bool {
        matches!(self.config.exit_mode, CryptoLobMlExitMode::SignalFlip)
    }

    fn allows_ev_exit(&self) -> bool {
        matches!(self.config.exit_mode, CryptoLobMlExitMode::EvExit)
    }

    fn allows_trailing_exit(&self) -> bool {
        matches!(self.config.exit_mode, CryptoLobMlExitMode::TrailingExit)
    }

    fn exit_mode_label(&self) -> &'static str {
        match self.config.exit_mode {
            CryptoLobMlExitMode::SettleOnly => "settle_only",
            CryptoLobMlExitMode::EvExit => "ev_exit",
            CryptoLobMlExitMode::SignalFlip => "signal_flip",
            CryptoLobMlExitMode::TrailingExit => "trailing_exit",
        }
    }

    fn ev_exit_buffer(&self, blended: &BlendedProb) -> Decimal {
        let base = self
            .config
            .ev_exit_buffer
            .max(Decimal::ZERO)
            .min(dec!(0.50));
        let scale = self
            .config
            .ev_exit_vol_scale
            .max(Decimal::ZERO)
            .min(dec!(0.50));
        let uncertainty = (dec!(0.5) - (blended.p_gbm_anchor - dec!(0.5)).abs()).max(Decimal::ZERO);
        (base + scale * uncertainty).min(dec!(0.50))
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

        // Binary option EV: fee is part of cost basis.
        // Win  → receive $1.00, invested effective_cost is lost profit.
        // Lose → receive $0.00, lose entire effective_cost.
        // EV = prob × (1 - effective_cost) - (1-prob) × effective_cost
        //    = prob - effective_cost
        let effective_cost = (effective_entry * (Decimal::ONE + fee_rate))
            .max(Decimal::ZERO)
            .min(dec!(0.999));
        let net_profit_on_win = Decimal::ONE - effective_cost;
        let loss_on_lose = effective_cost;

        prob * net_profit_on_win - (Decimal::ONE - prob) * loss_on_lose
    }

    fn compute_blended_probability(
        &self,
        p_up_model_dec: Decimal,
        p_gbm_anchor: Decimal,
    ) -> BlendedProb {
        let w_model = self.model_blend_weight_clamped();
        let w_anchor = Decimal::ONE - w_model;
        let p_up_blended = (p_up_model_dec * w_model + p_gbm_anchor * w_anchor)
            .max(dec!(0.001))
            .min(dec!(0.999));

        BlendedProb {
            p_up_blended,
            p_gbm_anchor,
            w_model,
        }
    }

    fn build_window_context(
        &self,
        spot: &SpotPrice,
        event: &EventInfo,
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

        Some(WindowContext {
            now,
            start_price,
            window_move,
            elapsed_secs,
            remaining_secs,
        })
    }

    fn evaluate_entry_signal(
        &self,
        blended: &BlendedProb,
        up_token_id: &str,
        down_token_id: &str,
        up_ask: Decimal,
        down_ask: Decimal,
    ) -> Option<EntrySignal> {
        if up_ask <= Decimal::ZERO || down_ask <= Decimal::ZERO {
            return None;
        }

        let p_up_blended = blended.p_up_blended;
        let up_edge_gross = p_up_blended - up_ask;
        let down_edge_gross = (Decimal::ONE - p_up_blended) - down_ask;
        let up_edge_net = self.net_ev_for_binary_side(p_up_blended, up_ask);
        let down_edge_net = self.net_ev_for_binary_side(Decimal::ONE - p_up_blended, down_ask);

        let (side, token_id, limit_price, edge, gross_edge, fair_value) =
            match self.config.entry_side_policy {
                CryptoLobMlEntrySidePolicy::BestEv => {
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
                    }
                }
                CryptoLobMlEntrySidePolicy::LaggingOnly => {
                    // Follow model direction, only enter when that side's ask is cheap (<= 0.50)
                    let model_dir = if p_up_blended >= dec!(0.50) { Side::Up } else { Side::Down };
                    let (dir_token, dir_ask, dir_edge, dir_gross, dir_fair) = match model_dir {
                        Side::Up => (up_token_id, up_ask, up_edge_net, up_edge_gross, p_up_blended),
                        Side::Down => (down_token_id, down_ask, down_edge_net, down_edge_gross,
                                       Decimal::ONE - p_up_blended),
                    };
                    if dir_ask > dec!(0.50) {
                        return None;
                    }
                    (model_dir, dir_token.to_string(), dir_ask, dir_edge, dir_gross, dir_fair)
                }
            };

        if limit_price > self.config.max_entry_price {
            return None;
        }
        if edge < self.config.min_edge {
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
            p_gbm_anchor: blended.p_gbm_anchor,
            p_up_blended,
            w_model: blended.w_model,
            up_edge_gross,
            down_edge_gross,
            up_edge_net,
            down_edge_net,
            up_ask,
            down_ask,
        })
    }

    #[cfg(feature = "tcn_db")]
    async fn estimate_p_up_tcn(
        &self,
        tcn: &mut TcnBuffers,
        event: &EventInfo,
        now: DateTime<Utc>,
    ) -> Option<f64> {
        let bucket_start = tcn.bucket_start(now);
        if tcn
            .last_bucket_by_condition
            .get(&event.condition_id)
            .is_some_and(|v| *v == bucket_start)
            && tcn.seq_by_condition.contains_key(&event.condition_id)
        {
            let seq = tcn.sequence_flat(&event.condition_id)?;
            return self.predict_tcn_sequence(&seq);
        }

        let Some(pool) = self.pool.as_ref() else {
            warn!(
                agent = self.config.agent_id,
                "onnx_tcn requested but no PgPool was provided; skipping prediction"
            );
            return None;
        };

        let bucket_end = bucket_start + chrono::Duration::seconds(tcn.sample_secs.max(1) as i64);
        let binance_symbol = Self::tcn_binance_symbol_from_market_slug(&event.slug);
        let market_start_ts = Self::tcn_market_start_ts_from_slug(&event.slug);

        let row_opt = sqlx::query(
            r#"
            WITH y AS (
              SELECT
                received_at AS ts,
                (bids->0->>'price')::double precision AS best_bid,
                (asks->0->>'price')::double precision AS best_ask
              FROM clob_orderbook_snapshots
              WHERE LOWER(COALESCE(domain, '')) = 'crypto'
                AND token_id = $1
                AND received_at >= $2
                AND received_at < $3
              ORDER BY received_at ASC
              LIMIT 1
            ),
            n AS (
              SELECT
                (s2.bids->0->>'price')::double precision AS best_bid,
                (s2.asks->0->>'price')::double precision AS best_ask
              FROM clob_orderbook_snapshots s2
              JOIN y ON TRUE
              WHERE LOWER(COALESCE(s2.domain, '')) = 'crypto'
                AND s2.token_id = $4
                AND s2.received_at BETWEEN y.ts - ($5::bigint * INTERVAL '1 second')
                                      AND y.ts + ($5::bigint * INTERVAL '1 second')
              ORDER BY ABS(EXTRACT(EPOCH FROM (s2.received_at - y.ts))) ASC
              LIMIT 1
            ),
            t AS (
              SELECT
                token_id,
                COUNT(*)::double precision AS cnt,
                COALESCE(SUM(size), 0)::double precision AS vol,
                COALESCE(SUM(price * size) / NULLIF(SUM(size), 0), NULL)::double precision AS vwap,
                (ARRAY_AGG(price ORDER BY trade_ts DESC))[1]::double precision AS last_price
              FROM clob_trade_ticks
              JOIN y ON TRUE
              WHERE token_id IN ($1, $4)
                AND trade_ts <= y.ts
                AND trade_ts > y.ts - ($6::bigint * INTERVAL '1 second')
              GROUP BY token_id
            ),
            v AS (
              SELECT
                token_id,
                received_at AS sample_ts,
                COALESCE(
                  0.5 * (
                    (bids->0->>'price')::double precision +
                    (asks->0->>'price')::double precision
                  ),
                  (bids->0->>'price')::double precision,
                  (asks->0->>'price')::double precision
                ) AS mid_price
              FROM clob_orderbook_snapshots
              JOIN y ON TRUE
              WHERE LOWER(COALESCE(domain, '')) = 'crypto'
                AND token_id IN ($1, $4)
                AND received_at <= y.ts
                AND received_at > y.ts - ($7::bigint * INTERVAL '1 second')
            ),
            vol AS (
              SELECT
                token_id,
                COALESCE(
                  (
                    STDDEV_SAMP(mid_price) FILTER (
                      WHERE sample_ts > (SELECT ts FROM y) - ($8::bigint * INTERVAL '1 second')
                    ) / NULLIF(
                      AVG(mid_price) FILTER (
                        WHERE sample_ts > (SELECT ts FROM y) - ($8::bigint * INTERVAL '1 second')
                      ),
                      0
                    )
                  ) * 10000.0,
                  0.0
                )::double precision AS vol_short_bps,
                COALESCE(
                  (STDDEV_SAMP(mid_price) / NULLIF(AVG(mid_price), 0)) * 10000.0,
                  0.0
                )::double precision AS vol_long_bps
              FROM v
              WHERE mid_price IS NOT NULL
                AND mid_price > 0.0
              GROUP BY token_id
            ),
            bn AS (
              SELECT b.price::double precision AS price
              FROM binance_price_ticks b
              JOIN y ON TRUE
              WHERE $9::text IS NOT NULL
                AND b.symbol = $9
                AND b.trade_time <= y.ts
              ORDER BY b.trade_time DESC
              LIMIT 1
            ),
            bs AS (
              SELECT b.price::double precision AS price
              FROM binance_price_ticks b
              WHERE $9::text IS NOT NULL
                AND $10::timestamptz IS NOT NULL
                AND b.symbol = $9
                AND b.trade_time <= $10
              ORDER BY b.trade_time DESC
              LIMIT 1
            )
            SELECT
              y.ts,
              y.best_bid AS yes_best_bid,
              y.best_ask AS yes_best_ask,
              n.best_bid AS no_best_bid,
              n.best_ask AS no_best_ask,
              (SELECT last_price FROM t WHERE token_id = $1) AS yes_last_trade,
              (SELECT cnt FROM t WHERE token_id = $1) AS yes_trade_count,
              (SELECT vol FROM t WHERE token_id = $1) AS yes_trade_volume,
              (SELECT vwap FROM t WHERE token_id = $1) AS yes_trade_vwap,
              (SELECT last_price FROM t WHERE token_id = $4) AS no_last_trade,
              (SELECT cnt FROM t WHERE token_id = $4) AS no_trade_count,
              (SELECT vol FROM t WHERE token_id = $4) AS no_trade_volume,
              (SELECT vwap FROM t WHERE token_id = $4) AS no_trade_vwap,
              (SELECT vol_short_bps FROM vol WHERE token_id = $1) AS yes_mid_vol_short_bps,
              (SELECT vol_long_bps FROM vol WHERE token_id = $1) AS yes_mid_vol_long_bps,
              (SELECT vol_short_bps FROM vol WHERE token_id = $4) AS no_mid_vol_short_bps,
              (SELECT vol_long_bps FROM vol WHERE token_id = $4) AS no_mid_vol_long_bps,
              bn.price AS spot_now,
              bs.price AS spot_start
            FROM y
            CROSS JOIN n
            "#,
        )
        .bind(&event.up_token_id)
        .bind(bucket_start)
        .bind(bucket_end)
        .bind(&event.down_token_id)
        .bind(tcn.pair_window_secs as i64)
        .bind(tcn.trade_lookback_secs as i64)
        .bind(tcn.vol_long_window_secs as i64)
        .bind(tcn.vol_short_window_secs as i64)
        .bind(binance_symbol)
        .bind(market_start_ts)
        .fetch_optional(pool)
        .await;

        let row = match row_opt {
            Ok(Some(r)) => r,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    agent = self.config.agent_id,
                    error = %e,
                    "failed to fetch onnx_tcn feature row from db"
                );
                return None;
            }
        };

        let ts: DateTime<Utc> = row.try_get(0).ok()?;
        let yes_best_bid: Option<f64> = row.try_get(1).ok()?;
        let yes_best_ask: Option<f64> = row.try_get(2).ok()?;
        let no_best_bid: Option<f64> = row.try_get(3).ok()?;
        let no_best_ask: Option<f64> = row.try_get(4).ok()?;

        let yes_last_trade: Option<f64> = row.try_get(5).ok()?;
        let yes_trade_count: f64 = row.try_get::<Option<f64>, _>(6).ok()?.unwrap_or(0.0);
        let yes_trade_volume: f64 = row.try_get::<Option<f64>, _>(7).ok()?.unwrap_or(0.0);
        let yes_trade_vwap: Option<f64> = row.try_get(8).ok()?;

        let no_last_trade: Option<f64> = row.try_get(9).ok()?;
        let no_trade_count: f64 = row.try_get::<Option<f64>, _>(10).ok()?.unwrap_or(0.0);
        let no_trade_volume: f64 = row.try_get::<Option<f64>, _>(11).ok()?.unwrap_or(0.0);
        let no_trade_vwap: Option<f64> = row.try_get(12).ok()?;

        let yes_vol_short_bps: f64 = row.try_get::<Option<f64>, _>(13).ok()?.unwrap_or(0.0);
        let yes_vol_long_bps: f64 = row.try_get::<Option<f64>, _>(14).ok()?.unwrap_or(0.0);
        let no_vol_short_bps: f64 = row.try_get::<Option<f64>, _>(15).ok()?.unwrap_or(0.0);
        let no_vol_long_bps: f64 = row.try_get::<Option<f64>, _>(16).ok()?.unwrap_or(0.0);

        let spot_now: Option<f64> = row.try_get(17).ok()?;
        let spot_start: Option<f64> = row.try_get(18).ok()?;
        let spot_now_f = spot_now.unwrap_or(0.0);
        let spot_start_f = spot_start.unwrap_or(0.0);
        let spot_vs_start_ret_bps = match (spot_now, spot_start) {
            (Some(now), Some(start)) if start > 0.0 && now.is_finite() && start.is_finite() => {
                ((now - start) / start) * 10_000.0
            }
            _ => 0.0,
        };
        let secs_to_anchor_f = event
            .end_time
            .signed_duration_since(ts)
            .num_seconds()
            .max(0) as f64;

        let yes_mid = Self::tcn_mid_price(yes_best_bid, yes_best_ask)?;
        let no_mid = Self::tcn_mid_price(no_best_bid, no_best_ask)?;
        let yes_spread_bps = Self::tcn_spread_bps(yes_best_bid, yes_best_ask, yes_mid);
        let no_spread_bps = Self::tcn_spread_bps(no_best_bid, no_best_ask, no_mid);
        let yes_no_mid_gap = (yes_mid + no_mid) - 1.0;

        let yes_last = yes_last_trade.unwrap_or(yes_mid);
        let no_last = no_last_trade.unwrap_or(no_mid);
        let yes_vwap = yes_trade_vwap.unwrap_or(yes_mid);
        let no_vwap = no_trade_vwap.unwrap_or(no_mid);

        let feats = [
            yes_best_bid.unwrap_or(yes_mid),
            yes_best_ask.unwrap_or(yes_mid),
            yes_mid,
            no_best_bid.unwrap_or(no_mid),
            no_best_ask.unwrap_or(no_mid),
            no_mid,
            yes_spread_bps,
            no_spread_bps,
            yes_no_mid_gap,
            yes_last,
            no_last,
            yes_trade_count,
            no_trade_count,
            yes_trade_volume,
            no_trade_volume,
            yes_vwap,
            no_vwap,
            yes_vol_short_bps,
            yes_vol_long_bps,
            no_vol_short_bps,
            no_vol_long_bps,
            spot_now_f,
            spot_start_f,
            spot_vs_start_ret_bps,
            secs_to_anchor_f,
        ];

        if feats.iter().any(|v| !v.is_finite()) {
            return None;
        }

        let mut row_f32 = [0.0f32; TCN_FEATURE_DIM];
        for (i, v) in feats.iter().enumerate() {
            row_f32[i] = *v as f32;
        }
        tcn.ingest_point(&event.condition_id, bucket_start, row_f32);

        let seq = tcn.sequence_flat(&event.condition_id)?;
        self.predict_tcn_sequence(&seq)
    }

    #[cfg(feature = "tcn_db")]
    fn predict_tcn_sequence(&self, seq: &[f32]) -> Option<f64> {
        #[cfg(feature = "onnx")]
        {
            let Some(m) = &self.onnx_model else {
                return None;
            };
            match m.predict_scalar(seq) {
                Ok(raw) if raw.is_finite() => {
                    let p = if raw < -0.001 || raw > 1.001 {
                        Self::sigmoid(raw as f64)
                    } else {
                        raw as f64
                    };
                    Some(p.clamp(0.001, 0.999))
                }
                Ok(_) => None,
                Err(e) => {
                    warn!(
                        agent = self.config.agent_id,
                        error = %e,
                        "lob-ml onnx_tcn inference failed"
                    );
                    None
                }
            }
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = seq;
            None
        }
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
        let mut sequence_cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();

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

                    // Momentum + volatility from trade-tick cache.
                    let spot_cache = self.binance_ws.price_cache();
                    let Some(spot) = spot_cache.get(&update.symbol).await else {
                        continue;
                    };
                    let momentum_1s = spot.momentum(1).unwrap_or(Decimal::ZERO);
                    let momentum_5s = spot.momentum(5).unwrap_or(Decimal::ZERO);
                    let rolling_volatility_opt = spot.volatility(60);

                    let Some(lob) = self.lob_cache.get_snapshot(&update.symbol).await else {
                        continue;
                    };
                    let age_secs = Utc::now().signed_duration_since(lob.timestamp).num_seconds();
                    if age_secs > self.config.max_lob_snapshot_age_secs as i64 {
                        continue;
                    }

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
                            self.build_window_context(&spot, &event)
                        else {
                            continue;
                        };
                        if !self
                            .is_within_entry_late_window(&timeframe, window_ctx.remaining_secs)
                        {
                            continue;
                        }
                        // Skip events without price_to_beat when required.
                        if self.config.use_price_to_beat
                            && self.config.require_price_to_beat
                            && event.price_to_beat.is_none()
                        {
                            continue;
                        }
                        let p_gbm_anchor = Self::estimate_p_up_gbm_anchor(
                            spot.price,
                            window_ctx.start_price,
                            event.price_to_beat,
                            rolling_volatility_opt,
                            window_ctx.remaining_secs,
                            self.config.oracle_lag_buffer_secs,
                        );

                        let price_to_beat = event.price_to_beat.unwrap_or(spot.price);
                        let distance_to_beat = if spot.price > Decimal::ZERO {
                            (price_to_beat - spot.price) / spot.price
                        } else {
                            Decimal::ZERO
                        };
                        let second_bucket =
                            chrono::DateTime::<Utc>::from_timestamp(spot.timestamp.timestamp(), 0)
                                .unwrap_or(spot.timestamp);
                        Self::push_sequence_snapshot(
                            &mut sequence_cache,
                            entry_key.as_str(),
                            SequenceSnapshot {
                                ts: second_bucket,
                                obi_5: lob.obi_5,
                                obi_10: lob.obi_10,
                                spread_bps: lob.spread_bps,
                                bid_volume_5: lob.bid_volume_5,
                                ask_volume_5: lob.ask_volume_5,
                                momentum_1s,
                                momentum_5s,
                                spot_price: spot.price,
                                remaining_secs: Decimal::from(window_ctx.remaining_secs.max(0)),
                                price_to_beat,
                                distance_to_beat,
                            },
                        );

                        let Some(sequence_input) =
                            Self::build_sequence(&sequence_cache, entry_key.as_str(), &timeframe,
                                &self.config.feature_offsets, &self.config.feature_scales)
                        else {
                            continue;
                        };
                        let (p_up_model, model_type_used) =
                            match self.estimate_p_up(&timeframe, &sequence_input) {
                                Ok(v) => v,
                                Err(e) => {
                                    warn!(
                                        agent = self.config.agent_id,
                                        symbol = %update.symbol,
                                        timeframe = %timeframe,
                                        error = %e,
                                        "onnx inference failed for sequence input"
                                    );
                                    continue;
                                }
                            };
                        let p_up_model_dec =
                            Decimal::from_f64_retain(p_up_model).unwrap_or(dec!(0.5));

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

                        // Spread check: reject thin markets where bid-ask is too wide.
                        {
                            let up_spread = if up_bid > Decimal::ZERO {
                                (up_ask - up_bid) / up_ask
                            } else {
                                Decimal::ONE
                            };
                            let down_spread = if down_bid > Decimal::ZERO {
                                (down_ask - down_bid) / down_ask
                            } else {
                                Decimal::ONE
                            };
                            if up_spread > self.config.max_spread_pct
                                || down_spread > self.config.max_spread_pct
                            {
                                continue;
                            }
                        }

                        let blended = self.compute_blended_probability(
                            p_up_model_dec,
                            p_gbm_anchor,
                        );

                        if let Some(pos) = positions.get(&event.slug).cloned() {
                            // 5m events: force hold-to-settlement (skip all exit logic).
                            if self.config.force_settle_only_5m
                                && normalize_timeframe(&pos.horizon) == "5m"
                            {
                                // Only update peak_bid for tracking.
                                if let Some(tracked) = positions.get_mut(&event.slug) {
                                    let held_bid = match pos.side {
                                        Side::Up => up_bid,
                                        Side::Down => down_bid,
                                    };
                                    if held_bid > tracked.peak_bid {
                                        tracked.peak_bid = held_bid;
                                    }
                                }
                                continue;
                            }
                            if self.allows_ev_exit() {
                                let held_secs = Utc::now()
                                    .signed_duration_since(pos.entry_time)
                                    .num_seconds();
                                if held_secs >= self.config.min_hold_secs as i64 {

                                    let held_bid = match pos.side {
                                        Side::Up => up_bid,
                                        Side::Down => down_bid,
                                    };
                                    if held_bid > Decimal::ZERO {
                                        let fee_rate = self
                                            .config
                                            .taker_fee_rate
                                            .max(Decimal::ZERO)
                                            .min(dec!(0.25));
                                        let bid_net =
                                            (held_bid * (Decimal::ONE - fee_rate))
                                                .max(Decimal::ZERO)
                                                .min(dec!(0.999));
                                        let fair_value = match pos.side {
                                            Side::Up => blended.p_up_blended,
                                            Side::Down => Decimal::ONE - blended.p_up_blended,
                                        };
                                        let ev_buffer = self.ev_exit_buffer(&blended);
                                        let ev_edge = bid_net - fair_value;
                                        if ev_edge >= ev_buffer {
                                            let exit_intent = OrderIntent::new(
                                                &self.config.agent_id,
                                                Domain::Crypto,
                                                &pos.market_slug,
                                                &pos.token_id,
                                                pos.side,
                                                false,
                                                pos.shares,
                                                held_bid,
                                            );
                                            let position_coin = pos.symbol.replace("USDT", "");
                                            let deployment_id = deployment_id_for(
                                                STRATEGY_ID,
                                                &position_coin,
                                                &pos.horizon,
                                            );
                                            let timeframe = normalize_timeframe(&pos.horizon);
                                            let event_window_secs =
                                                event_window_secs_for_horizon(&timeframe)
                                                    .to_string();
                                            let exit_intent = exit_intent
                                                .with_priority(OrderPriority::High)
                                                .with_metadata("strategy", STRATEGY_ID)
                                                .with_metadata("deployment_id", &deployment_id)
                                                .with_metadata("timeframe", &timeframe)
                                                .with_metadata(
                                                    "event_window_secs",
                                                    &event_window_secs,
                                                )
                                                .with_metadata(
                                                    "signal_type",
                                                    "crypto_lob_ml_exit",
                                                )
                                                .with_metadata("coin", &position_coin)
                                                .with_metadata("symbol", &pos.symbol)
                                                .with_metadata("series_id", &pos.series_id)
                                                .with_metadata("event_series_id", &pos.series_id)
                                                .with_metadata("horizon", &pos.horizon)
                                                .with_metadata("exit_mode", self.exit_mode_label())
                                                .with_metadata("exit_reason", "model_ev")
                                                .with_metadata(
                                                    "entry_price",
                                                    &pos.entry_price.to_string(),
                                                )
                                                .with_metadata(
                                                    "exit_price",
                                                    &held_bid.to_string(),
                                                )
                                                .with_metadata(
                                                    "exit_bid_net",
                                                    &bid_net.to_string(),
                                                )
                                                .with_metadata(
                                                    "exit_fair_value",
                                                    &fair_value.to_string(),
                                                )
                                                .with_metadata(
                                                    "exit_ev_edge",
                                                    &ev_edge.to_string(),
                                                )
                                                .with_metadata(
                                                    "exit_ev_buffer",
                                                    &ev_buffer.to_string(),
                                                )
                                                .with_metadata(
                                                    "held_secs",
                                                    &held_secs.to_string(),
                                                )
                                                .with_metadata(
                                                    "p_up_model",
                                                    &format!("{p_up_model:.6}"),
                                                )
                                                .with_metadata(
                                                    "p_gbm_anchor",
                                                    &p_gbm_anchor.to_string(),
                                                )
                                                .with_metadata(
                                                    "p_up_blended",
                                                    &blended.p_up_blended.to_string(),
                                                )
                                                .with_metadata(
                                                    "p_up_blend_w_model",
                                                    &blended.w_model.to_string(),
                                                )
                                                .with_metadata(
                                                    "cost_taker_fee_rate",
                                                    &self.config.taker_fee_rate.to_string(),
                                                )
                                                .with_metadata(
                                                    "config_hash",
                                                    &config_hash,
                                                );

                                            match ctx.submit_order(exit_intent).await {
                                                Ok(()) => {
                                                    info!(
                                                        agent = self.config.agent_id,
                                                        slug = %event.slug,
                                                        side = %pos.side,
                                                        held_secs,
                                                        fair_value = %fair_value,
                                                        bid_net = %bid_net,
                                                        "model EV exit triggered, submitting sell order"
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        agent = self.config.agent_id,
                                                        slug = %event.slug,
                                                        error = %e,
                                                        "failed to submit model-EV exit order"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if self.allows_signal_flip_exit() {
                                let Some(signal) = self.evaluate_entry_signal(
                                    &blended,
                                    &event.up_token_id,
                                    &event.down_token_id,
                                    up_ask,
                                    down_ask,
                                ) else {
                                    continue;
                                };
                                if pos.side != signal.side {
                                    let held_secs =
                                        Utc::now().signed_duration_since(pos.entry_time).num_seconds();
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
                                            let deployment_id = deployment_id_for(
                                                STRATEGY_ID,
                                                &position_coin,
                                                &pos.horizon,
                                            );
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
                                            .with_metadata("p_gbm_anchor", &signal.p_gbm_anchor.to_string())
                                            .with_metadata("p_up_blended", &signal.p_up_blended.to_string())
                                            .with_metadata("p_up_blend_w_model", &signal.w_model.to_string())
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
                            }
                            continue;
                        }

                        let Some(signal) = self.evaluate_entry_signal(
                            &blended,
                            &event.up_token_id,
                            &event.down_token_id,
                            up_ask,
                            down_ask,
                        ) else {
                            continue;
                        };

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

                        let max_order_value = self.config.risk_params.max_order_value;
                        let shares = if max_order_value > Decimal::ZERO {
                            // Size by USD notional: shares ~= max_order_value / signal.limit_price.
                            // Truncation ensures we don't exceed the configured budget.
                            let raw = (max_order_value / signal.limit_price).trunc();
                            let shares = raw.to_u64().unwrap_or(0);
                            if shares < 1 {
                                continue;
                            }
                            shares
                        } else {
                            self.config.default_shares.max(1)
                        };

                        let intent = OrderIntent::new(
                            &self.config.agent_id,
                            Domain::Crypto,
                            event.slug.as_str(),
                            &signal.token_id,
                            signal.side,
                            true,
                            shares,
                            signal.limit_price,
                        );
                        let deployment_id = deployment_id_for(STRATEGY_ID, &coin, &event.horizon);
                        let event_window_secs =
                            event_window_secs_for_horizon(&timeframe).to_string();
                        let intent = intent
                        .with_priority(OrderPriority::Normal)
                        .with_metadata("strategy", STRATEGY_ID)
                        .with_deployment_id(&deployment_id)
                        .with_metadata("timeframe", &timeframe)
                        .with_metadata("event_window_secs", &event_window_secs)
                        .with_metadata("signal_type", "crypto_lob_ml_entry")
                        .with_metadata(
                            "entry_side_policy",
                            match self.config.entry_side_policy {
                                CryptoLobMlEntrySidePolicy::BestEv => "best_ev",
                                CryptoLobMlEntrySidePolicy::LaggingOnly => "lagging_only",
                            },
                        )
                        .with_metadata("coin", &coin)
                        .with_metadata("symbol", &update.symbol)
                        .with_condition_id(&event.condition_id)
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
                        .with_metadata("p_gbm_anchor", &signal.p_gbm_anchor.to_string())
                        .with_metadata("p_up_blended", &signal.p_up_blended.to_string())
                        .with_metadata("p_up_blend_w_model", &signal.w_model.to_string())
                        .with_metadata("model_type", model_type_used)
                        .with_metadata("model_version", self.config.model_version.as_deref().unwrap_or(""))
                        .with_metadata("model_trained_at", self.config.model_trained_at.as_deref().unwrap_or(""))
                        .with_metadata("model_auc", &self.config.model_auc.map(|v| format!("{v:.4}")).unwrap_or_default())
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

                // --- Polymarket quote updates (trailing exit decisions) ---
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

                    if !self.allows_trailing_exit() {
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
                    let Some(pos) = positions.get_mut(&slug) else {
                        continue;
                    };

                    if pos.entry_price <= Decimal::ZERO {
                        continue;
                    }

                    // Update peak bid (track high-water mark).
                    if best_bid > pos.peak_bid {
                        pos.peak_bid = best_bid;
                    }

                    let held_secs = Utc::now().signed_duration_since(pos.entry_time).num_seconds();
                    if held_secs < self.config.min_hold_secs as i64 {
                        continue;
                    }

                    // --- Time-aware trailing pullback ---
                    // Base pullback threshold (e.g. 15%).
                    let base_pullback = self.config.trailing_pullback_pct
                        .max(dec!(0.01))
                        .min(dec!(0.50));
                    // Time decay: as remaining_secs → 0, shrink tolerance.
                    // time_fraction = remaining / total (1.0 at start, 0.0 at settlement)
                    let event_total_secs = event_window_secs_for_horizon(&pos.horizon);
                    let remaining_secs = (event_total_secs as i64 - held_secs).max(0);
                    let time_fraction = if event_total_secs > 0 {
                        Decimal::from(remaining_secs) / Decimal::from(event_total_secs)
                    } else {
                        Decimal::ZERO
                    };
                    // effective_pullback = base × (1 - decay × (1 - time_fraction))
                    // At start (tf=1.0): effective = base × 1.0 (full tolerance)
                    // At settlement (tf=0): effective = base × (1 - decay) (tightened)
                    let decay = self.config.trailing_time_decay
                        .max(Decimal::ZERO)
                        .min(dec!(0.90));
                    let effective_pullback = base_pullback
                        * (Decimal::ONE - decay * (Decimal::ONE - time_fraction));

                    // Compute pullback from peak.
                    let pullback_pct = if pos.peak_bid > Decimal::ZERO {
                        (pos.peak_bid - best_bid) / pos.peak_bid
                    } else {
                        Decimal::ZERO
                    };

                    if pullback_pct < effective_pullback {
                        continue;
                    }

                    let pnl_pct = (best_bid - pos.entry_price) / pos.entry_price;
                    let pos = pos.clone();
                    let exit_reason = "trailing_pullback";
                    let priority = if pnl_pct >= Decimal::ZERO {
                        OrderPriority::High
                    } else {
                        OrderPriority::Critical
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
                    .with_deployment_id(&deployment_id)
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
                                .with_deployment_id(&deployment_id)
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
        }
    }

    fn sample_blended_prob(agent: &CryptoLobMlAgent) -> BlendedProb {
        agent
            .compute_blended_probability(dec!(0.62), dec!(0.58))
    }

    fn sample_sequence_snapshot(ts: DateTime<Utc>) -> SequenceSnapshot {
        SequenceSnapshot {
            ts,
            obi_5: dec!(0.10),
            obi_10: dec!(0.12),
            spread_bps: dec!(2.0),
            bid_volume_5: dec!(1000),
            ask_volume_5: dec!(980),
            momentum_1s: dec!(0.001),
            momentum_5s: dec!(0.003),
            spot_price: dec!(102000),
            remaining_secs: dec!(90),
            price_to_beat: dec!(102500),
            distance_to_beat: dec!(0.0049),
        }
    }

    #[test]
    fn test_build_sequence_lengths_for_5m_and_15m() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|15m";
        let now = Utc::now();
        for i in 0..SEQ_LEN_15M {
            CryptoLobMlAgent::push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        let seq_5m = CryptoLobMlAgent::build_sequence(&cache, key, "5m", &[], &[])
            .expect("5m sequence should be available");
        assert_eq!(seq_5m.len(), SEQ_LEN_5M * SEQ_FEATURE_DIM);

        let seq_15m = CryptoLobMlAgent::build_sequence(&cache, key, "15m", &[], &[])
            .expect("15m sequence should be available");
        assert_eq!(seq_15m.len(), SEQ_LEN_15M * SEQ_FEATURE_DIM);
    }

    #[test]
    fn test_build_sequence_returns_none_when_insufficient_history() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|5m";
        let now = Utc::now();
        for i in 0..(SEQ_LEN_5M - 1) {
            CryptoLobMlAgent::push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        assert!(CryptoLobMlAgent::build_sequence(&cache, key, "5m", &[], &[]).is_none());
    }

    #[test]
    fn test_build_sequence_applies_normalization() {
        let mut cache: HashMap<String, VecDeque<SequenceSnapshot>> = HashMap::new();
        let key = "BTCUSDT|5m";
        let now = Utc::now();
        // Push exactly SEQ_LEN_5M snapshots (one per second)
        for i in 0..SEQ_LEN_5M {
            CryptoLobMlAgent::push_sequence_snapshot(
                &mut cache,
                key,
                sample_sequence_snapshot(now + chrono::Duration::seconds(i as i64)),
            );
        }

        // Without normalization
        let raw = CryptoLobMlAgent::build_sequence(&cache, key, "5m", &[], &[])
            .expect("raw sequence");

        // With identity normalization (offset=0, scale=1)
        let offsets = vec![0.0f32; SEQ_FEATURE_DIM];
        let scales = vec![1.0f32; SEQ_FEATURE_DIM];
        let identity = CryptoLobMlAgent::build_sequence(&cache, key, "5m", &offsets, &scales)
            .expect("identity normalized");
        assert_eq!(raw.len(), identity.len());
        for (a, b) in raw.iter().zip(identity.iter()) {
            assert!((a - b).abs() < 1e-6, "identity transform should match raw");
        }

        // With real normalization: offset=1, scale=2 for first feature
        let mut offsets2 = vec![0.0f32; SEQ_FEATURE_DIM];
        let mut scales2 = vec![1.0f32; SEQ_FEATURE_DIM];
        offsets2[0] = 1.0;
        scales2[0] = 2.0;
        let normed = CryptoLobMlAgent::build_sequence(&cache, key, "5m", &offsets2, &scales2)
            .expect("normalized");
        // First feature of first timestep: (raw[0] - 1.0) * 2.0
        let expected_first = (raw[0] - 1.0) * 2.0;
        assert!(
            (normed[0] - expected_first).abs() < 1e-6,
            "expected {expected_first}, got {}",
            normed[0]
        );
        // Second feature should be unchanged (offset=0, scale=1)
        assert!(
            (normed[1] - raw[1]).abs() < 1e-6,
            "second feature should be unchanged"
        );
    }

    #[test]
    fn test_align_sequence_to_model_input_handles_boundary_cases() {
        let exact = vec![1.0f32; SEQ_FEATURE_DIM * 2];
        let (exact_aligned, exact_mode) =
            CryptoLobMlAgent::align_sequence_to_model_input(&exact, SEQ_FEATURE_DIM * 2).unwrap();
        assert_eq!(exact_mode, SequenceAlignMode::Exact);
        assert_eq!(exact_aligned, exact);
    }

    #[test]
    fn test_align_sequence_to_model_input_rejects_non_snapshot_aligned_model_dim() {
        let sequence = vec![1.0f32; SEQ_FEATURE_DIM * 2];
        let err = CryptoLobMlAgent::align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM + 1)
            .err()
            .expect("non-snapshot input_dim must fail fast");
        assert!(
            err.to_string()
                .contains("must be a multiple of sequence feature dim")
        );
    }

    #[test]
    fn test_align_sequence_to_model_input_truncate_and_pad_keep_snapshot_boundaries() {
        let mut sequence = Vec::new();
        for snapshot_value in [1.0f32, 2.0, 3.0] {
            sequence.extend(std::iter::repeat(snapshot_value).take(SEQ_FEATURE_DIM));
        }

        let (truncated, truncate_mode) =
            CryptoLobMlAgent::align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM * 2)
                .unwrap();
        assert_eq!(truncate_mode, SequenceAlignMode::TruncateOldest);
        assert_eq!(truncated.len(), SEQ_FEATURE_DIM * 2);
        assert!(truncated[..SEQ_FEATURE_DIM].iter().all(|v| *v == 2.0));
        assert!(truncated[SEQ_FEATURE_DIM..].iter().all(|v| *v == 3.0));

        let (padded, pad_mode) =
            CryptoLobMlAgent::align_sequence_to_model_input(&sequence, SEQ_FEATURE_DIM * 4)
                .unwrap();
        assert_eq!(pad_mode, SequenceAlignMode::LeftPadZero);
        assert_eq!(padded.len(), SEQ_FEATURE_DIM * 4);
        assert!(padded[..SEQ_FEATURE_DIM].iter().all(|v| *v == 0.0));
        assert_eq!(&padded[SEQ_FEATURE_DIM..], sequence.as_slice());
    }

    #[test]
    fn test_align_sequence_to_model_input_allows_15m_sequence_with_5m_model_dim() {
        let seq_15m_len = SEQ_LEN_15M * SEQ_FEATURE_DIM;
        let seq_5m_len = SEQ_LEN_5M * SEQ_FEATURE_DIM;
        let seq_15m: Vec<f32> = (0..seq_15m_len).map(|i| i as f32).collect();

        let (aligned, mode) =
            CryptoLobMlAgent::align_sequence_to_model_input(&seq_15m, seq_5m_len).unwrap();

        assert_eq!(mode, SequenceAlignMode::TruncateOldest);
        assert_eq!(aligned.len(), seq_5m_len);
        assert_eq!(aligned, seq_15m[(seq_15m_len - seq_5m_len)..].to_vec());
    }

    #[test]
    fn test_estimate_p_up_validates_sequence_input_dim() {
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
        let bad = vec![0.0f32; (SEQ_LEN_5M * SEQ_FEATURE_DIM).saturating_sub(1)];
        let err = agent
            .estimate_p_up("5m", &bad)
            .err()
            .expect("bad input dim should fail");
        #[cfg(feature = "onnx")]
        assert!(err.to_string().contains("sequence input dim mismatch"));
        #[cfg(not(feature = "onnx"))]
        assert!(err.to_string().contains("--features onnx"));
    }

    #[test]
    fn test_config_defaults() {
        let cfg = CryptoLobMlConfig::default();
        assert_eq!(cfg.agent_id, "crypto_lob_ml");
        assert_eq!(cfg.coins, vec!["BTC", "ETH", "SOL", "XRP"]);
        assert_eq!(cfg.max_time_remaining_secs, 900);
        assert_eq!(cfg.exit_mode, CryptoLobMlExitMode::EvExit);
        assert_eq!(cfg.max_entry_price, dec!(0.70));
        assert_eq!(cfg.model_blend_weight, dec!(0.80));
        assert_eq!(
            cfg.entry_side_policy,
            CryptoLobMlEntrySidePolicy::LaggingOnly
        );
        assert_eq!(cfg.entry_late_window_secs_5m, 180);
        assert_eq!(cfg.ev_exit_buffer, dec!(0.005));
        assert_eq!(cfg.entry_late_window_secs_15m, 180);
        assert_eq!(cfg.ev_exit_vol_scale, dec!(0.02));
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
        assert!(!settle.allows_ev_exit());
        assert!(!settle.allows_trailing_exit());

        let ev_exit = mk_agent(CryptoLobMlExitMode::EvExit);
        assert!(!ev_exit.allows_signal_flip_exit());
        assert!(ev_exit.allows_ev_exit());
        assert!(!ev_exit.allows_trailing_exit());

        let flip = mk_agent(CryptoLobMlExitMode::SignalFlip);
        assert!(flip.allows_signal_flip_exit());
        assert!(!flip.allows_ev_exit());
        assert!(!flip.allows_trailing_exit());

        let trailing = mk_agent(CryptoLobMlExitMode::TrailingExit);
        assert!(!trailing.allows_signal_flip_exit());
        assert!(!trailing.allows_ev_exit());
        assert!(trailing.allows_trailing_exit());
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
    fn test_model_blend_weight_default() {
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

        let w_model = agent.model_blend_weight_clamped();
        assert_eq!(w_model, dec!(0.80));
    }

    #[test]
    fn test_entry_window_enforces_late_windows_for_5m_and_15m() {
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

        assert!(agent.is_within_entry_late_window("5m", 180));
        assert!(!agent.is_within_entry_late_window("5m", 181));
        assert!(agent.is_within_entry_late_window("15m", 180));
        assert!(!agent.is_within_entry_late_window("15m", 181));
    }

    #[test]
    fn test_evaluate_entry_signal_uses_lagging_side_default() {
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

        let blended = agent
            .compute_blended_probability(dec!(0.40), dec!(0.42));
        let signal = agent
            .evaluate_entry_signal(
                &blended,
                "up-token",
                "down-token",
                dec!(0.28),
                dec!(0.25),
            )
            .expect("signal should pass filters");

        assert_eq!(signal.side, Side::Down);
        assert_eq!(signal.token_id, "down-token");
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

        let blended = sample_blended_prob(&agent);
        let signal = agent.evaluate_entry_signal(
            &blended,
            "up-token",
            "down-token",
            dec!(0.80),
            dec!(0.95),
        );
        assert!(signal.is_none());
    }

    #[test]
    fn test_evaluate_entry_signal_rejects_price_above_strict_cap() {
        let mut cfg = CryptoLobMlConfig::default();
        cfg.max_entry_price = dec!(0.30);
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

        let blended = sample_blended_prob(&agent);
        let signal = agent.evaluate_entry_signal(
            &blended,
            "up-token",
            "down-token",
            dec!(0.31),
            dec!(0.32),
        );
        assert!(signal.is_none(), "ask > 0.30 must be rejected");
    }

    #[test]
    fn test_gbm_anchor_falls_back_to_window_move_when_no_price_to_beat() {
        // Without price_to_beat, GBM anchor uses window_move fallback
        let p = CryptoLobMlAgent::estimate_p_up_gbm_anchor(
            dec!(101),      // spot_price
            dec!(100),      // start_price
            None,           // no price_to_beat
            Some(dec!(0.001)), // sigma_1s
            270,            // remaining_secs
            0,              // oracle_lag_buffer_secs
        );
        // window_move = +1%, with small volatility over 270s → P(UP) > 0.50
        assert!(p > dec!(0.50), "p_up with positive momentum should be > 0.50, got {p}");
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
                peak_bid: dec!(0.5),
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
