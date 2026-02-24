//! CryptoTradingAgent — pull-based agent for crypto 5m/15m UP/DOWN markets
//!
//! Owns Binance + Polymarket WebSocket feeds. Reuses signal logic from
//! the existing CryptoAgent (sum_of_asks threshold + momentum direction).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
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
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::Result;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

const TRADED_EVENT_RETENTION_HOURS: i64 = 24;
const STRATEGY_ID: &str = "crypto_momentum";

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

/// Convert event threshold into required return from event start price.
///
/// For UP/DOWN markets:
/// final_return = (end_price - start_price) / start_price
/// UP wins when final_return > required_return
fn required_return_from_threshold(start_price: Decimal, price_to_beat: Decimal) -> Option<Decimal> {
    if start_price <= Decimal::ZERO || price_to_beat <= Decimal::ZERO {
        return None;
    }

    let rr = (price_to_beat - start_price) / start_price;

    // Guard against bad threshold parsing (e.g., timestamps misread as prices).
    // Real 5m/15m crypto UP/DOWN thresholds should be close to start price.
    if rr.abs() > dec!(0.20) {
        return None;
    }

    Some(rr)
}

/// Estimate P(UP wins) over the remaining event window.
///
/// Model assumption:
/// - Remaining return over the window is zero-mean normal:
///   R_rem ~ N(0, sigma_1s^2 * t_rem)
/// - Current realized return from start is `window_move`
/// - UP wins when `window_move + R_rem > required_return`
fn estimate_p_up_window(
    window_move: Decimal,
    required_return: Decimal,
    rolling_volatility_opt: Option<Decimal>,
    window_remaining_secs: i64,
) -> Decimal {
    if window_remaining_secs <= 0 {
        return dec!(0.5);
    }

    let sigma_1s = rolling_volatility_opt.unwrap_or(Decimal::ZERO);
    let sigma_1s_f = sigma_1s.to_f64().unwrap_or(0.0);
    let sigma_rem = sigma_1s_f * (window_remaining_secs as f64).sqrt();

    let z_num = (window_move - required_return).to_f64().unwrap_or(0.0);
    let p_up = if sigma_rem.is_finite() && sigma_rem > 0.0 && z_num.is_finite() {
        normal_cdf(z_num / sigma_rem)
    } else if z_num > 0.0 {
        1.0
    } else if z_num < 0.0 {
        0.0
    } else {
        0.5
    };

    Decimal::from_f64(p_up)
        .unwrap_or(dec!(0.5))
        .max(dec!(0.01))
        .min(dec!(0.99))
}

/// Configuration for the CryptoTradingAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub sum_threshold: Decimal,
    pub min_momentum_1s: f64,
    /// Minimum absolute move since the event start required to trade.
    ///
    /// For UP/DOWN markets, resolution depends on the net change over the full window, so this
    /// threshold helps avoid "coin-flip" windows near flat.
    #[serde(default)]
    pub min_window_move_pct: Decimal,
    /// Minimum edge required for entry:
    /// edge = fair_value - market_entry_price.
    #[serde(default = "default_exit_edge_floor")]
    pub min_edge: Decimal,
    /// Refresh interval for Gamma event discovery (seconds)
    pub event_refresh_secs: u64,
    /// Minimum time remaining for selected event (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining for selected event (seconds)
    pub max_time_remaining_secs: u64,
    /// Prefer events closest to end (confirmatory mode)
    pub prefer_close_to_end: bool,
    /// Optional entry cooldown per (symbol,timeframe), in seconds.
    ///
    /// This is a safety throttle to prevent bursty duplicate entries from noisy feeds.
    /// Use `0` to disable and rely on per-market idempotency + duplicate guards.
    #[serde(default)]
    pub entry_cooldown_secs: u64,
    /// Require multi-timeframe momentum agreement for entries.
    ///
    /// When enabled:
    /// - 5m entries require 1s and 5s momentum to agree (when 5s momentum is available)
    /// - 15m entries additionally require 30s momentum to agree (when available)
    #[serde(default)]
    pub require_mtf_agreement: bool,
    pub default_shares: u64,
    #[serde(default = "default_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    #[serde(default = "default_exit_price_band")]
    pub exit_price_band: Decimal,
    /// Optional mark-to-market binary exit thresholds (disabled by default)
    pub enable_price_exits: bool,
    /// Minimum hold time before edge/price-band exits are allowed (seconds)
    pub min_hold_secs: u64,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

impl Default for CryptoTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto".into(),
            name: "Crypto Momentum".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            sum_threshold: dec!(0.96),
            min_momentum_1s: 0.001,
            min_window_move_pct: dec!(0.0001), // 0.01%
            min_edge: dec!(0.02),
            event_refresh_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            prefer_close_to_end: true,
            entry_cooldown_secs: 0,
            require_mtf_agreement: true,
            default_shares: 100,
            exit_edge_floor: default_exit_edge_floor(),
            exit_price_band: default_exit_price_band(),
            enable_price_exits: false,
            min_hold_secs: 20,
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}

/// Internal position tracking
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
    is_hedged: bool,
}

/// Pull-based crypto trading agent
pub struct CryptoTradingAgent {
    config: CryptoTradingConfig,
    binance_ws: Arc<BinanceWebSocket>,
    pm_ws: Arc<PolymarketWebSocket>,
    event_matcher: Arc<EventMatcher>,
}

fn should_skip_entry(
    event_slug: &str,
    positions: &HashMap<String, TrackedPosition>,
    traded_events: &HashMap<String, DateTime<Utc>>,
) -> bool {
    positions.contains_key(event_slug) || traded_events.contains_key(event_slug)
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
    let strategy_slug = normalize_component(strategy).replace('_', "-");
    let strategy_slug = strategy_slug
        .strip_prefix("crypto-")
        .unwrap_or(strategy_slug.as_str())
        .to_string();
    // Deployment matrix is strategy+timeframe scoped; coin routing stays in metadata.
    let _ = coin;
    format!("crypto-{}-{}", strategy_slug, normalize_timeframe(horizon))
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
        is_hedged: position.is_hedged,
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

impl CryptoTradingAgent {
    pub fn new(
        config: CryptoTradingConfig,
        binance_ws: Arc<BinanceWebSocket>,
        pm_ws: Arc<PolymarketWebSocket>,
        event_matcher: Arc<EventMatcher>,
    ) -> Self {
        Self {
            config,
            binance_ws,
            pm_ws,
            event_matcher,
        }
    }

    fn config_hash(&self) -> String {
        let payload = serde_json::to_vec(&self.config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(payload);
        format!("{:x}", hasher.finalize())
    }

    fn entry_cooldown_secs(&self) -> u64 {
        self.config.entry_cooldown_secs
    }

    fn estimate_fair_value(momentum: Decimal) -> Decimal {
        (dec!(0.50) + momentum.abs() * dec!(10))
            .max(dec!(0.05))
            .min(dec!(0.95))
    }

    fn signal_confidence(
        sum_of_asks: Decimal,
        sum_threshold: Decimal,
        momentum_1s: Decimal,
        short_momentum: Decimal,
        long_momentum: Decimal,
        min_momentum: Decimal,
    ) -> Decimal {
        let min_mom = min_momentum.max(dec!(0.0001));
        let momentum_strength = (momentum_1s.abs() / min_mom).min(dec!(3));
        let momentum_score = (momentum_strength / dec!(3)) * dec!(0.50);

        let dislocation = if sum_threshold > Decimal::ZERO {
            ((sum_threshold - sum_of_asks).max(Decimal::ZERO) / sum_threshold).min(Decimal::ONE)
        } else {
            Decimal::ZERO
        };
        let dislocation_score = dislocation * dec!(0.30);

        let trend_aligned = (short_momentum >= Decimal::ZERO && long_momentum >= Decimal::ZERO)
            || (short_momentum <= Decimal::ZERO && long_momentum <= Decimal::ZERO);
        let trend_score = if trend_aligned {
            dec!(0.20)
        } else {
            dec!(0.10)
        };

        (momentum_score + dislocation_score + trend_score).min(Decimal::ONE)
    }
}

#[async_trait]
impl TradingAgent for CryptoTradingAgent {
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
        info!(agent = self.config.agent_id, "crypto agent starting");
        let config_hash = self.config_hash();

        let mut status = AgentStatus::Running;
        let mut positions: HashMap<String, TrackedPosition> = HashMap::new();
        let mut active_events: HashMap<String, Vec<EventInfo>> = HashMap::new(); // symbol -> events
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut last_entry_at: HashMap<String, DateTime<Utc>> = HashMap::new(); // symbol|timeframe -> ts (optional throttle)
        let daily_pnl = Decimal::ZERO;
        sync_positions_from_global(&ctx, &self.config.agent_id, &mut positions).await;

        // Subscribe to data feeds
        let mut binance_rx: broadcast::Receiver<PriceUpdate> = self.binance_ws.subscribe();
        let mut pm_rx: broadcast::Receiver<QuoteUpdate> = self.pm_ws.subscribe_updates();

        // Periodic refresh of active events
        let refresh_dur = tokio::time::Duration::from_secs(self.config.event_refresh_secs);
        let mut refresh_tick = tokio::time::interval(refresh_dur);
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let heartbeat_dur = tokio::time::Duration::from_secs(self.config.heartbeat_interval_secs);
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

                // --- Binance price updates ---
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

                    // Check if this coin is in our watchlist
                    let coin = update.symbol.replace("USDT", "");
                    if !self.config.coins.iter().any(|c| c == &coin) {
                        continue;
                    }

                    // Find active UP/DOWN events for this symbol (5m/15m can both be present).
                    let events = match active_events.get(&update.symbol) {
                        Some(e) if !e.is_empty() => e.clone(),
                        None => {
                            debug!(agent = self.config.agent_id, symbol = %update.symbol, "no active event yet");
                            continue;
                        }
                        _ => continue,
                    };

                    // Spot price + derived signals from the Binance tick cache.
                    let spot_cache = self.binance_ws.price_cache();
                    let Some(spot) = spot_cache.get(&update.symbol).await else {
                        continue;
                    };

                    let momentum_1s = spot.momentum(1).unwrap_or(Decimal::ZERO);
                    let short_momentum_opt = spot.momentum(5);
                    let long_momentum_opt = spot.momentum(30);
                    let short_momentum = short_momentum_opt.unwrap_or(Decimal::ZERO);
                    let long_momentum = long_momentum_opt.unwrap_or(Decimal::ZERO);
                    let rolling_volatility_opt = spot.volatility(60);
                    let rolling_volatility = rolling_volatility_opt.unwrap_or(Decimal::ZERO);

                    // Helper: infer the correct direction from the net move since the event window start.
                    // For UP/DOWN markets, resolution is based on end_price >= start_price, not micro-momentum.
                    let window_signal = |event: &EventInfo| -> Option<(Side, Decimal, i64, i64, Decimal)> {
                        let now = spot.timestamp;
                        if now < event.start_time || now >= event.end_time {
                            return None;
                        }

                        let elapsed_secs = now.signed_duration_since(event.start_time).num_seconds();
                        if elapsed_secs < 0 {
                            return None;
                        }

                        let remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
                        if remaining_secs <= 0 {
                            return None;
                        }

                        let target_time = now - chrono::Duration::seconds(elapsed_secs);
                        if let Some(oldest) = spot.oldest_timestamp() {
                            if oldest > target_time {
                                return None;
                            }
                        } else {
                            return None;
                        }

                        let start_price = spot.price_secs_ago(elapsed_secs as u64)?;
                        if start_price <= Decimal::ZERO {
                            return None;
                        }
                        let window_move = (spot.price - start_price) / start_price;
                        let side = if window_move >= Decimal::ZERO {
                            Side::Up
                        } else {
                            Side::Down
                        };
                        Some((side, window_move, elapsed_secs, remaining_secs, start_price))
                    };

                    let quote_cache = self.pm_ws.quote_cache();

                    // Binary options default: exit on signal flip instead of TP/SL.
                    for (slug, pos) in &positions {
                        if pos.symbol != update.symbol {
                            continue;
                        }

                        let Some(event) = events.iter().find(|e| e.slug == pos.market_slug) else {
                            continue;
                        };
                        let Some((side, window_move, window_elapsed_secs, window_remaining_secs, window_start_price)) = window_signal(event) else {
                            continue;
                        };

                        if pos.side == side {
                            continue;
                        }

                        let held_secs = Utc::now().signed_duration_since(pos.entry_time).num_seconds();
                        if held_secs < self.config.min_hold_secs as i64 {
                            continue;
                        }

                        let Some(best_bid) = quote_cache.get(&pos.token_id).and_then(|q| q.best_bid) else {
                            continue;
                        };
                        if best_bid <= Decimal::ZERO {
                            continue;
                        }

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
                        let deployment_id =
                            deployment_id_for(STRATEGY_ID, &position_coin, &pos.horizon);
                        let timeframe = normalize_timeframe(&pos.horizon);
                        let event_window_secs =
                            event_window_secs_for_horizon(&timeframe).to_string();
                        let intent = intent
                        .with_priority(OrderPriority::High)
                        .with_metadata("strategy", STRATEGY_ID)
                        .with_deployment_id(&deployment_id)
                        .with_metadata("timeframe", &timeframe)
                        .with_metadata("event_window_secs", &event_window_secs)
                        .with_metadata("signal_type", "crypto_momentum_exit")
                        .with_metadata("coin", &position_coin)
                        .with_metadata("symbol", &pos.symbol)
                        .with_metadata("series_id", &pos.series_id)
                        .with_metadata("event_series_id", &pos.series_id)
                        .with_metadata("horizon", &pos.horizon)
                        .with_metadata("exit_reason", "signal_flip")
                        .with_metadata("entry_price", &pos.entry_price.to_string())
                        .with_metadata("exit_price", &best_bid.to_string())
                        .with_metadata("held_secs", &held_secs.to_string())
                        .with_metadata("signal_momentum_value", &momentum_1s.to_string())
                        .with_metadata("event_start_time", &event.start_time.to_rfc3339())
                        .with_metadata("window_start_price", &window_start_price.to_string())
                        .with_metadata("window_move_pct", &window_move.to_string())
                        .with_metadata("window_elapsed_secs", &window_elapsed_secs.to_string())
                        .with_metadata("window_remaining_secs", &window_remaining_secs.to_string())
                        .with_metadata("config_hash", &config_hash);

                        match ctx.submit_order(intent).await {
                            Ok(()) => {
                                info!(
                                    agent = self.config.agent_id,
                                    slug = %slug,
                                    old_side = %pos.side,
                                    new_side = %side,
                                    held_secs,
                                    "signal flip detected, submitting sell order"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    agent = self.config.agent_id,
                                    slug = %slug,
                                    error = %e,
                                    "failed to submit signal-flip exit order"
                                );
                            }
                        }
                    }

                    let mut entered_timeframes: HashSet<String> = HashSet::new();
                    let now = Utc::now();
                    for event in events {
                        if should_skip_entry(&event.slug, &positions, &traded_events) {
                            continue;
                        }

                        let timeframe = normalize_timeframe(&event.horizon);
                        if entered_timeframes.contains(&timeframe) {
                            continue;
                        }

                        let Some((side, window_move, window_elapsed_secs, window_remaining_secs, window_start_price)) = window_signal(&event) else {
                            continue;
                        };

                        // Only trade events that are within their own active window.
                        // Gamma may surface upcoming windows early; without this guard, we'd "pre-trade"
                        // multiple future markets and then look idle later.
                        if window_remaining_secs < self.config.min_time_remaining_secs as i64 {
                            continue;
                        }
                        if window_remaining_secs > self.config.max_time_remaining_secs as i64 {
                            continue;
                        }
                        if window_move.abs() < self.config.min_window_move_pct {
                            continue;
                        }

                        // Optional throttle: avoid burst entries from noisy feeds.
                        let cooldown_secs = self.entry_cooldown_secs();
                        if cooldown_secs > 0 {
                            let entry_key = format!("{}|{}", update.symbol, &timeframe);
                            if let Some(prev) = last_entry_at.get(&entry_key) {
                                let elapsed = now
                                    .signed_duration_since(*prev)
                                    .num_seconds()
                                    .max(0) as u64;
                                if elapsed < cooldown_secs {
                                    continue;
                                }
                            }
                        }

                        if self.config.require_mtf_agreement {
                            let dir_sign = match side {
                                Side::Up => 1,
                                Side::Down => -1,
                            };
                            let mom_sign = if momentum_1s > Decimal::ZERO {
                                1
                            } else if momentum_1s < Decimal::ZERO {
                                -1
                            } else {
                                0
                            };

                            let sign = |v: Decimal| {
                                if v > Decimal::ZERO {
                                    1
                                } else if v < Decimal::ZERO {
                                    -1
                                } else {
                                    0
                                }
                            };

                            let short_sign = short_momentum_opt.map(sign).unwrap_or(0);
                            let long_sign = long_momentum_opt.map(sign).unwrap_or(0);

                            if mom_sign != 0 && mom_sign != dir_sign {
                                continue;
                            }
                            if timeframe == "15m" {
                                if (short_sign != 0 && short_sign != dir_sign)
                                    || (long_sign != 0 && long_sign != dir_sign)
                                {
                                    continue;
                                }
                            } else if short_sign != 0 && short_sign != dir_sign {
                                continue;
                            }
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

                        let sum_of_asks = up_ask + down_ask;
                        if sum_of_asks >= self.config.sum_threshold {
                            continue;
                        }

                        let (token_id, limit_price) = match side {
                            Side::Up => (event.up_token_id.clone(), up_ask),
                            Side::Down => (event.down_token_id.clone(), down_ask),
                        };

                        let required_return = event
                            .price_to_beat
                            .and_then(|thr| required_return_from_threshold(window_start_price, thr))
                            .unwrap_or(Decimal::ZERO);

                        // Best-effort fair value estimate with threshold awareness:
                        // P(UP) = P(window_move + remaining_return > required_return).
                        let p_up = estimate_p_up_window(
                            window_move,
                            required_return,
                            rolling_volatility_opt,
                            window_remaining_secs,
                        );
                        let fair_value = match side {
                            Side::Up => p_up,
                            Side::Down => Decimal::ONE - p_up,
                        };
                        let signal_edge = fair_value - limit_price;
                        if signal_edge < self.config.min_edge {
                            continue;
                        }
                        let confidence = Self::signal_confidence(
                            sum_of_asks,
                            self.config.sum_threshold,
                            momentum_1s,
                            short_momentum,
                            long_momentum,
                            Decimal::try_from(self.config.min_momentum_1s).unwrap_or(dec!(0.001)),
                        );

                        let intent = OrderIntent::new(
                            &self.config.agent_id,
                            Domain::Crypto,
                            event.slug.as_str(),
                            &token_id,
                            side,
                            true,
                            self.config.default_shares,
                            limit_price,
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
                        .with_metadata("coin", &coin)
                        .with_condition_id(&event.condition_id)
                        .with_metadata("series_id", &event.series_id)
                        .with_metadata("event_series_id", &event.series_id)
                        .with_metadata("horizon", &event.horizon)
                        .with_metadata("event_start_time", &event.start_time.to_rfc3339())
                        .with_metadata("event_end_time", &event.end_time.to_rfc3339())
                        .with_metadata(
                            "price_to_beat",
                            &event
                                .price_to_beat
                                .map(|v| v.to_string())
                                .unwrap_or_default(),
                        )
                        .with_metadata("required_return", &required_return.to_string())
                        .with_metadata("sum_of_asks", &sum_of_asks.to_string())
                        .with_metadata("event_title", &event.title)
                        .with_metadata("signal_type", "crypto_momentum_entry")
                        .with_metadata("signal_confidence", &confidence.to_string())
                        .with_metadata("signal_momentum_value", &momentum_1s.to_string())
                        .with_metadata("signal_short_ma", &short_momentum.to_string())
                        .with_metadata("signal_long_ma", &long_momentum.to_string())
                        .with_metadata("signal_rolling_volatility", &rolling_volatility.to_string())
                        .with_metadata("p_up", &p_up.to_string())
                        .with_metadata("signal_fair_value", &fair_value.to_string())
                        .with_metadata("signal_market_price", &limit_price.to_string())
                        .with_metadata("signal_edge", &signal_edge.to_string())
                        .with_metadata("signal_min_edge", &self.config.min_edge.to_string())
                        .with_metadata("window_start_price", &window_start_price.to_string())
                        .with_metadata("window_move_pct", &window_move.to_string())
                        .with_metadata("window_elapsed_secs", &window_elapsed_secs.to_string())
                        .with_metadata("window_remaining_secs", &window_remaining_secs.to_string())
                        .with_metadata("config_hash", &config_hash);

                        info!(
                            agent = self.config.agent_id,
                            slug = %event.slug,
                            horizon = %event.horizon,
                            %sum_of_asks,
                            %side,
                            %limit_price,
                            window_move = %window_move,
                            "signal detected, submitting order"
                        );

                        if let Err(e) = ctx.submit_order(intent).await {
                            warn!(agent = self.config.agent_id, error = %e, "failed to submit order");
                            continue;
                        }

                        // Track position locally
                        traded_events.insert(event.slug.clone(), now);
                        if self.config.entry_cooldown_secs > 0 {
                            last_entry_at
                                .insert(format!("{}|{}", update.symbol, &timeframe), now);
                        }
                        entered_timeframes.insert(timeframe);
                    }
                }

                // --- Polymarket quote updates ---
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

                    // Check TP/SL on any tracked position matching this token.
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
                    let deployment_id =
                        deployment_id_for(STRATEGY_ID, &position_coin, &pos.horizon);
                    let timeframe = normalize_timeframe(&pos.horizon);
                    let event_window_secs = event_window_secs_for_horizon(&timeframe).to_string();
                    let intent = intent
                    .with_priority(priority)
                    .with_metadata("strategy", STRATEGY_ID)
                    .with_deployment_id(&deployment_id)
                    .with_metadata("timeframe", &timeframe)
                    .with_metadata("event_window_secs", &event_window_secs)
                    .with_metadata("signal_type", "crypto_momentum_exit")
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
                                    bid, // best-effort sell
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
                                .with_metadata("signal_type", "crypto_momentum_exit")
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

        info!(agent = self.config.agent_id, "crypto agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_config_defaults() {
        let cfg = CryptoTradingConfig::default();
        assert_eq!(cfg.agent_id, "crypto");
        assert_eq!(cfg.coins.len(), 4);
        assert_eq!(cfg.sum_threshold, dec!(0.96));
        assert_eq!(cfg.min_edge, dec!(0.02));
        assert!(!cfg.enable_price_exits);
        assert_eq!(cfg.min_hold_secs, 20);
    }

    #[test]
    fn test_required_return_from_threshold_sanity() {
        let start = dec!(100);
        let threshold = dec!(101);
        let rr = required_return_from_threshold(start, threshold).expect("required return");
        assert_eq!(rr, dec!(0.01));

        let impossible = required_return_from_threshold(start, dec!(200));
        assert!(
            impossible.is_none(),
            "implausible threshold should be ignored"
        );
    }

    #[test]
    fn test_estimate_p_up_window_respects_required_return() {
        let window_move = dec!(0.01);
        let vol = Some(dec!(0.002));
        let rem = 300;

        let base = estimate_p_up_window(window_move, dec!(0), vol, rem);
        let harder = estimate_p_up_window(window_move, dec!(0.01), vol, rem);
        let easier = estimate_p_up_window(window_move, dec!(-0.01), vol, rem);

        assert!(harder < base, "higher threshold should reduce p_up");
        assert!(easier > base, "lower threshold should increase p_up");
    }

    #[test]
    fn test_should_skip_entry_when_slug_already_traded() {
        let positions: HashMap<String, TrackedPosition> = HashMap::new();
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();
        traded_events.insert("btc-updown-5m-1".to_string(), Utc::now());

        assert!(should_skip_entry(
            "btc-updown-5m-1",
            &positions,
            &traded_events
        ));
    }

    #[test]
    fn test_prune_stale_traded_events() {
        let now = Utc::now();
        let mut traded_events: HashMap<String, DateTime<Utc>> = HashMap::new();
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
            deployment_id_for("momentum", "ETH", "5m"),
            "crypto-momentum-5m"
        );
        assert_eq!(
            deployment_id_for("crypto_momentum", "BTC", "15m"),
            "crypto-momentum-15m"
        );
    }
}
