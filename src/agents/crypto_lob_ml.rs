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

use crate::adapters::{BinanceWebSocket, PolymarketWebSocket, PriceUpdate, QuoteUpdate};
use crate::agents::{AgentContext, TradingAgent};
use crate::collector::{LobCache, LobSnapshot};
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::Result;
use crate::ml::DenseNetwork;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

const TRADED_EVENT_RETENTION_HOURS: i64 = 24;
const STRATEGY_ID: &str = "crypto_lob_ml";

fn default_exit_edge_floor() -> Decimal {
    dec!(0.02)
}

fn default_exit_price_band() -> Decimal {
    dec!(0.05)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobMlWeights {
    pub bias: f64,
    pub w_obi_5: f64,
    pub w_obi_10: f64,
    pub w_momentum_1s: f64,
    pub w_momentum_5s: f64,
    pub w_spread_bps: f64,
}

impl Default for LobMlWeights {
    fn default() -> Self {
        // Reasonable starting point; tune via env-config + backtests.
        // Typical ranges:
        // - OBI: [-1, 1]
        // - momentum_1s: ~[-0.002, 0.002] in calm markets
        // - spread_bps: single digits
        Self {
            bias: 0.0,
            w_obi_5: 1.5,
            w_obi_10: 0.5,
            w_momentum_1s: 150.0,
            w_momentum_5s: 50.0,
            w_spread_bps: -0.01,
        }
    }
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
    /// Optional mark-to-market binary exit thresholds (disabled by default)
    pub enable_price_exits: bool,
    /// Minimum hold time before edge/price-band exits are allowed (seconds)
    pub min_hold_secs: u64,

    /// Minimum expected-value edge required to enter.
    /// UP edge = p_up - up_ask; DOWN edge = (1 - p_up) - down_ask.
    pub min_edge: Decimal,

    /// Max ask price to pay for entry (YES/NO).
    pub max_entry_price: Decimal,

    /// Minimum seconds between entries per symbol (avoid thrash).
    pub cooldown_secs: u64,

    /// Reject LOB snapshots older than this age (seconds).
    pub max_lob_snapshot_age_secs: u64,

    pub weights: LobMlWeights,

    /// Prediction model type: "logistic" (default) or "mlp".
    #[serde(default = "default_lob_ml_model_type")]
    pub model_type: String,

    /// Optional JSON model path used when `model_type = "mlp"`.
    #[serde(default)]
    pub model_path: Option<String>,

    /// Optional model version label recorded in order metadata (helps audit).
    #[serde(default)]
    pub model_version: Option<String>,

    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

fn default_lob_ml_model_type() -> String {
    "logistic".to_string()
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
            enable_price_exits: false,
            min_hold_secs: 20,
            min_edge: dec!(0.02),
            max_entry_price: dec!(0.70),
            cooldown_secs: 30,
            max_lob_snapshot_age_secs: 2,
            weights: LobMlWeights::default(),
            model_type: default_lob_ml_model_type(),
            model_path: None,
            model_version: None,
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

pub struct CryptoLobMlAgent {
    config: CryptoLobMlConfig,
    binance_ws: Arc<BinanceWebSocket>,
    pm_ws: Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
    lob_cache: LobCache,
    nn_model: Option<DenseNetwork>,
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
    ) -> Self {
        let model_type = config.model_type.trim().to_ascii_lowercase();
        let nn_model = if model_type == "mlp" || model_type == "mlp_json" {
            match config.model_path.as_deref() {
                Some(path) if !path.trim().is_empty() => match DenseNetwork::from_file(path) {
                    Ok(m) => {
                        info!(
                            agent = config.agent_id,
                            model_type = "mlp_json",
                            model_path = %path,
                            input_dim = m.input_dim,
                            output_dim = m.output_dim(),
                            "loaded lob-ml neural model"
                        );
                        Some(m)
                    }
                    Err(e) => {
                        warn!(
                            agent = config.agent_id,
                            model_type = "mlp_json",
                            model_path = %path,
                            error = %e,
                            "failed to load lob-ml neural model; falling back to logistic"
                        );
                        None
                    }
                },
                _ => {
                    warn!(
                        agent = config.agent_id,
                        model_type = "mlp_json",
                        "model_type=mlp but model_path is not set; falling back to logistic"
                    );
                    None
                }
            }
        } else {
            None
        };

        #[cfg(feature = "onnx")]
        let onnx_model: Option<OnnxModel> = if model_type == "onnx" {
            match config.model_path.as_deref() {
                Some(path) if !path.trim().is_empty() => {
                    match OnnxModel::load_for_vec_input(path, 7) {
                        Ok(m) => {
                            info!(
                                agent = config.agent_id,
                                model_type = "onnx",
                                model_path = %path,
                                input_dim = m.input_dim(),
                                output_dim = m.output_dim(),
                                "loaded lob-ml onnx model"
                            );
                            Some(m)
                        }
                        Err(e) => {
                            warn!(
                                agent = config.agent_id,
                                model_type = "onnx",
                                model_path = %path,
                                error = %e,
                                "failed to load lob-ml onnx model; falling back to logistic"
                            );
                            None
                        }
                    }
                }
                _ => {
                    warn!(
                        agent = config.agent_id,
                        model_type = "onnx",
                        "model_type=onnx but model_path is not set; falling back to logistic"
                    );
                    None
                }
            }
        } else {
            None
        };

        #[cfg(not(feature = "onnx"))]
        if model_type == "onnx" {
            warn!(
                agent = config.agent_id,
                "model_type=onnx requested but binary is built without --features onnx; falling back to logistic"
            );
        }

        Self {
            config,
            binance_ws,
            pm_ws,
            event_matcher,
            lob_cache,
            nn_model,
            #[cfg(feature = "onnx")]
            onnx_model,
        }
    }

    fn config_hash(&self) -> String {
        let payload = serde_json::to_vec(&self.config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(payload);
        format!("{:x}", hasher.finalize())
    }

    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    /// Estimate p(UP) from LOB snapshot + short-horizon momentum.
    fn estimate_p_up_logistic(
        &self,
        lob: &LobSnapshot,
        momentum_1s: Decimal,
        momentum_5s: Decimal,
    ) -> f64 {
        let w = &self.config.weights;

        let obi5 = lob.obi_5.to_f64().unwrap_or(0.0);
        let obi10 = lob.obi_10.to_f64().unwrap_or(0.0);
        let spread = lob.spread_bps.to_f64().unwrap_or(0.0);
        let m1 = momentum_1s.to_f64().unwrap_or(0.0);
        let m5 = momentum_5s.to_f64().unwrap_or(0.0);

        let z = w.bias
            + w.w_obi_5 * obi5
            + w.w_obi_10 * obi10
            + w.w_momentum_1s * m1
            + w.w_momentum_5s * m5
            + w.w_spread_bps * spread;

        // Avoid exact 0/1 probabilities.
        Self::sigmoid(z).clamp(0.001, 0.999)
    }

    /// Estimate p(UP) using configured model. Returns (p_up, model_type_used).
    fn estimate_p_up(
        &self,
        lob: &LobSnapshot,
        momentum_1s: Decimal,
        momentum_5s: Decimal,
    ) -> (f64, &'static str) {
        let model_type = self.config.model_type.trim().to_ascii_lowercase();

        // Shared feature order (must match training/export):
        // [obi5, obi10, spread_bps, bid_volume_5, ask_volume_5, momentum_1s, momentum_5s]
        let obi5 = lob.obi_5.to_f64().unwrap_or(0.0);
        let obi10 = lob.obi_10.to_f64().unwrap_or(0.0);
        let spread = lob.spread_bps.to_f64().unwrap_or(0.0);
        let bidv5 = lob.bid_volume_5.to_f64().unwrap_or(0.0);
        let askv5 = lob.ask_volume_5.to_f64().unwrap_or(0.0);
        let m1 = momentum_1s.to_f64().unwrap_or(0.0);
        let m5 = momentum_5s.to_f64().unwrap_or(0.0);

        if model_type == "onnx" {
            #[cfg(feature = "onnx")]
            {
                if let Some(m) = &self.onnx_model {
                    let features = [
                        obi5 as f32,
                        obi10 as f32,
                        spread as f32,
                        bidv5 as f32,
                        askv5 as f32,
                        m1 as f32,
                        m5 as f32,
                    ];
                    match m.predict_scalar(&features) {
                        Ok(raw) if raw.is_finite() => {
                            // Prefer probability output, but tolerate logits.
                            let p = if raw < -0.001 || raw > 1.001 {
                                Self::sigmoid(raw as f64)
                            } else {
                                raw as f64
                            };
                            return (p.clamp(0.001, 0.999), "onnx");
                        }
                        Ok(_) => {
                            warn!(
                                agent = self.config.agent_id,
                                "lob-ml onnx returned non-finite output; falling back to logistic"
                            );
                        }
                        Err(e) => {
                            warn!(
                                agent = self.config.agent_id,
                                error = %e,
                                "lob-ml onnx inference failed; falling back to logistic"
                            );
                        }
                    }
                }
            }
        }

        if model_type == "mlp" || model_type == "mlp_json" {
            if let Some(nn) = &self.nn_model {
                // Feature order must match training/export.
                let features = [obi5, obi10, spread, bidv5, askv5, m1, m5];

                match nn.forward_scalar(&features) {
                    Ok(p) if p.is_finite() => return (p.clamp(0.001, 0.999), "mlp_json"),
                    Ok(_) => {
                        warn!(
                            agent = self.config.agent_id,
                            "lob-ml nn returned non-finite p_up; falling back to logistic"
                        );
                    }
                    Err(e) => {
                        warn!(
                            agent = self.config.agent_id,
                            error = %e,
                            "lob-ml nn inference failed; falling back to logistic"
                        );
                    }
                }
            }
        }

        (
            self.estimate_p_up_logistic(lob, momentum_1s, momentum_5s),
            "logistic",
        )
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

                    // Momentum from trade-tick cache.
                    let spot_cache = self.binance_ws.price_cache();
                    let momentum_1s = spot_cache.momentum(&update.symbol, 1).await.unwrap_or(Decimal::ZERO);
                    let momentum_5s = spot_cache.momentum(&update.symbol, 5).await.unwrap_or(Decimal::ZERO);

                    let (p_up, model_type_used) = self.estimate_p_up(&lob, momentum_1s, momentum_5s);
                    let p_up_dec = Decimal::from_f64_retain(p_up).unwrap_or(dec!(0.5));

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

                        // Only trade events that have actually started.
                        // Gamma can surface future windows early; avoid "pre-trading" them.
                        let now = update.timestamp;
                        if now < event.start_time || now >= event.end_time {
                            continue;
                        }

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
                        if up_ask <= Decimal::ZERO || down_ask <= Decimal::ZERO {
                            continue;
                        }

                        let up_edge = p_up_dec - up_ask;
                        let down_edge = (Decimal::ONE - p_up_dec) - down_ask;
                        let (side, token_id, limit_price, edge, confidence) = if up_edge >= down_edge {
                            (Side::Up, event.up_token_id.clone(), up_ask, up_edge, p_up_dec)
                        } else {
                            (Side::Down, event.down_token_id.clone(), down_ask, down_edge, Decimal::ONE - p_up_dec)
                        };

                        if edge < self.config.min_edge {
                            continue;
                        }
                        if limit_price > self.config.max_entry_price {
                            continue;
                        }

                        if let Some(pos) = positions.get(&event.slug).cloned() {
                            if pos.side != side {
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
                                        .with_metadata("exit_reason", "signal_flip")
                                        .with_metadata("entry_price", &pos.entry_price.to_string())
                                        .with_metadata("exit_price", &exit_price.to_string())
                                        .with_metadata("held_secs", &held_secs.to_string())
                                        .with_metadata("p_up", &format!("{p_up:.6}"))
                                        .with_metadata("signal_edge", &edge.to_string())
                                        .with_metadata("config_hash", &config_hash);

                                        match ctx.submit_order(exit_intent).await {
                                            Ok(()) => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    slug = %event.slug,
                                                    old_side = %pos.side,
                                                    new_side = %side,
                                                    held_secs,
                                                    p_up = %p_up,
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
                            Utc::now(),
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
                            &token_id,
                            side,
                            true,
                            self.config.default_shares.max(1),
                            limit_price,
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
                        .with_metadata("event_end_time", &event.end_time.to_rfc3339())
                        .with_metadata("event_title", &event.title)
                        .with_metadata("p_up", &format!("{p_up:.6}"))
                        .with_metadata("model_type", model_type_used)
                        .with_metadata("model_version", self.config.model_version.as_deref().unwrap_or(""))
                        .with_metadata("signal_edge", &edge.to_string())
                        .with_metadata("signal_confidence", &confidence.to_string())
                        .with_metadata("signal_fair_value", &confidence.to_string())
                        .with_metadata("signal_market_price", &limit_price.to_string())
                        .with_metadata("pm_up_ask", &up_ask.to_string())
                        .with_metadata("pm_down_ask", &down_ask.to_string())
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
                        .with_metadata("config_hash", &config_hash);

                        info!(
                            agent = self.config.agent_id,
                            slug = %event.slug,
                            horizon = %event.horizon,
                            %side,
                            %limit_price,
                            %edge,
                            p_up = %p_up,
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

                    if !self.config.enable_price_exits {
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
                    let _ = ctx.report_state(
                        &self.config.name,
                        status,
                        positions.len(),
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
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

    #[test]
    fn test_config_defaults() {
        let cfg = CryptoLobMlConfig::default();
        assert_eq!(cfg.agent_id, "crypto_lob_ml");
        assert_eq!(cfg.coins, vec!["BTC", "ETH", "SOL", "XRP"]);
        assert_eq!(cfg.max_time_remaining_secs, 900);
        assert!(!cfg.enable_price_exits);
        assert_eq!(cfg.min_hold_secs, 20);
        assert!(cfg.prefer_close_to_end);
    }

    #[test]
    fn test_probability_clamps() {
        // Minimal snapshot just for the estimator.
        let snap = LobSnapshot {
            timestamp: Utc::now(),
            symbol: "BTCUSDT".into(),
            best_bid: dec!(1),
            best_ask: dec!(1),
            mid_price: dec!(1),
            spread_bps: dec!(1),
            obi_5: dec!(0),
            obi_10: dec!(0),
            bid_volume_5: dec!(1),
            ask_volume_5: dec!(1),
            update_id: 1,
        };
        let agent = CryptoLobMlAgent {
            config: CryptoLobMlConfig::default(),
            binance_ws: Arc::new(BinanceWebSocket::new(vec![])),
            pm_ws: Arc::new(PolymarketWebSocket::new("wss://example.com")),
            event_matcher: Arc::new(EventMatcher::new(
                crate::adapters::PolymarketClient::new("https://example.com", true).unwrap(),
            )),
            lob_cache: LobCache::new(),
            nn_model: None,
            #[cfg(feature = "onnx")]
            onnx_model: None,
        };
        let (p, _model) = agent.estimate_p_up(&snap, Decimal::ZERO, Decimal::ZERO);
        assert!(p > 0.0 && p < 1.0);
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
