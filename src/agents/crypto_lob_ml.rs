//! CryptoLobMlAgent — pull-based agent that uses Binance LOB features to estimate
//! a short-horizon UP probability (BTC 5m focus by default) and trade Polymarket
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
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::momentum::{EventInfo, EventMatcher};

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
    pub take_profit: Decimal,
    pub stop_loss: Decimal,

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
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

impl Default for CryptoLobMlConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto_lob_ml".into(),
            name: "Crypto LOB ML".into(),
            coins: vec!["BTC".into()],
            event_refresh_secs: 15,
            // By default, focus on 5m markets and enter early (predictive mode).
            min_time_remaining_secs: 240,
            max_time_remaining_secs: 300,
            prefer_close_to_end: false,
            default_shares: 50,
            take_profit: dec!(0.02),
            stop_loss: dec!(0.05),
            min_edge: dec!(0.02),
            max_entry_price: dec!(0.70),
            cooldown_secs: 30,
            max_lob_snapshot_age_secs: 2,
            weights: LobMlWeights::default(),
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone)]
struct TrackedPosition {
    market_slug: String,
    symbol: String,
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
}

impl CryptoLobMlAgent {
    pub fn new(
        config: CryptoLobMlConfig,
        binance_ws: Arc<BinanceWebSocket>,
        pm_ws: Arc<PolymarketWebSocket>,
        event_matcher: Arc<EventMatcher>,
        lob_cache: LobCache,
    ) -> Self {
        Self {
            config,
            binance_ws,
            pm_ws,
            event_matcher,
            lob_cache,
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
    fn estimate_p_up(&self, lob: &LobSnapshot, momentum_1s: Decimal, momentum_5s: Decimal) -> f64 {
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
        let mut active_events: HashMap<String, EventInfo> = HashMap::new(); // symbol -> event
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();

        let daily_pnl = Decimal::ZERO;
        let mut total_exposure = Decimal::ZERO;

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

                    let event = match active_events.get(&update.symbol) {
                        Some(e) => e,
                        None => {
                            debug!(agent = self.config.agent_id, symbol = %update.symbol, "no active event yet");
                            continue;
                        }
                    };

                    // Do not re-enter the same event.
                    if positions.contains_key(&event.slug) {
                        continue;
                    }

                    // Cooldown per symbol.
                    if let Some(last) = last_trade_by_symbol.get(&update.symbol) {
                        if Utc::now().signed_duration_since(*last).num_seconds() < self.config.cooldown_secs as i64 {
                            continue;
                        }
                    }

                    // Pull latest PM quotes.
                    let quote_cache = self.pm_ws.quote_cache();
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

                    let p_up = self.estimate_p_up(&lob, momentum_1s, momentum_5s);
                    let p_up_dec = Decimal::from_f64_retain(p_up).unwrap_or(dec!(0.5));

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

                    let intent = OrderIntent::new(
                        &self.config.agent_id,
                        Domain::Crypto,
                        event.slug.as_str(),
                        &token_id,
                        side,
                        true,
                        self.config.default_shares.max(1),
                        limit_price,
                    )
                    .with_priority(OrderPriority::Normal)
                    .with_metadata("strategy", "crypto_lob_ml")
                    .with_metadata("signal_type", "crypto_lob_ml_entry")
                    .with_metadata("coin", &coin)
                    .with_metadata("symbol", &update.symbol)
                    .with_metadata("condition_id", &event.condition_id)
                    .with_metadata("event_end_time", &event.end_time.to_rfc3339())
                    .with_metadata("event_title", &event.title)
                    .with_metadata("p_up", &format!("{p_up:.6}"))
                    .with_metadata("signal_edge", &edge.to_string())
                    .with_metadata("signal_confidence", &confidence.to_string())
                    .with_metadata("pm_up_ask", &up_ask.to_string())
                    .with_metadata("pm_down_ask", &down_ask.to_string())
                    .with_metadata("lob_best_bid", &lob.best_bid.to_string())
                    .with_metadata("lob_best_ask", &lob.best_ask.to_string())
                    .with_metadata("lob_mid_price", &lob.mid_price.to_string())
                    .with_metadata("lob_spread_bps", &lob.spread_bps.to_string())
                    .with_metadata("lob_obi_5", &lob.obi_5.to_string())
                    .with_metadata("lob_obi_10", &lob.obi_10.to_string())
                    .with_metadata("lob_bid_volume_5", &lob.bid_volume_5.to_string())
                    .with_metadata("lob_ask_volume_5", &lob.ask_volume_5.to_string())
                    .with_metadata("signal_momentum_1s", &momentum_1s.to_string())
                    .with_metadata("signal_momentum_5s", &momentum_5s.to_string())
                    .with_metadata("config_hash", &config_hash);

                    info!(
                        agent = self.config.agent_id,
                        slug = %event.slug,
                        %side,
                        %limit_price,
                        %edge,
                        p_up = %p_up,
                        "lob-ml signal detected, submitting order"
                    );

                    if let Err(e) = ctx.submit_order(intent).await {
                        warn!(agent = self.config.agent_id, error = %e, "failed to submit order");
                        continue;
                    }

                    last_trade_by_symbol.insert(update.symbol.clone(), Utc::now());
                    positions.insert(event.slug.clone(), TrackedPosition {
                        market_slug: event.slug.clone(),
                        symbol: update.symbol.clone(),
                        token_id,
                        side,
                        shares: self.config.default_shares.max(1),
                        entry_price: limit_price,
                        entry_time: Utc::now(),
                    });

                    total_exposure = positions.values()
                        .map(|p| p.entry_price * Decimal::from(p.shares))
                        .sum();
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

                    let pnl_pct = (best_bid - pos.entry_price) / pos.entry_price;
                    let maybe_reason = if pnl_pct >= self.config.take_profit {
                        Some(("take_profit", OrderPriority::High))
                    } else if pnl_pct <= -self.config.stop_loss {
                        Some(("stop_loss", OrderPriority::Critical))
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
                    )
                    .with_priority(priority)
                    .with_metadata("strategy", "crypto_lob_ml")
                    .with_metadata("signal_type", "crypto_lob_ml_exit")
                    .with_metadata("symbol", &pos.symbol)
                    .with_metadata("exit_reason", exit_reason)
                    .with_metadata("entry_price", &pos.entry_price.to_string())
                    .with_metadata("exit_price", &best_bid.to_string())
                    .with_metadata("pnl_pct", &pnl_pct.to_string())
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
                            positions.remove(&slug);
                            total_exposure = positions.values()
                                .map(|p| p.entry_price * Decimal::from(p.shares))
                                .sum();
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
                                )
                                .with_priority(OrderPriority::Critical)
                                .with_metadata("strategy", "crypto_lob_ml")
                                .with_metadata("signal_type", "crypto_lob_ml_exit")
                                .with_metadata("symbol", &pos.symbol)
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
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: self.config.name.clone(),
                                domain: Domain::Crypto,
                                status,
                                position_count: positions.len(),
                                exposure: total_exposure,
                                daily_pnl,
                                unrealized_pnl: Decimal::ZERO,
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
        assert_eq!(cfg.coins, vec!["BTC".to_string()]);
        assert_eq!(cfg.max_time_remaining_secs, 300);
        assert!(!cfg.prefer_close_to_end);
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
        };
        let p = agent.estimate_p_up(&snap, Decimal::ZERO, Decimal::ZERO);
        assert!(p > 0.0 && p < 1.0);
    }
}
