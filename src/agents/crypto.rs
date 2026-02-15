//! CryptoTradingAgent — pull-based agent for crypto 15-min UP/DOWN markets
//!
//! Owns Binance + Polymarket WebSocket feeds. Reuses signal logic from
//! the existing CryptoAgent (sum_of_asks threshold + momentum direction).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
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

/// Configuration for the CryptoTradingAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub sum_threshold: Decimal,
    pub min_momentum_1s: f64,
    /// Refresh interval for Gamma event discovery (seconds)
    pub event_refresh_secs: u64,
    /// Minimum time remaining for selected event (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining for selected event (seconds)
    pub max_time_remaining_secs: u64,
    /// Prefer events closest to end (confirmatory mode)
    pub prefer_close_to_end: bool,
    pub default_shares: u64,
    pub take_profit: Decimal,
    pub stop_loss: Decimal,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

impl Default for CryptoTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto".into(),
            name: "Crypto Momentum".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into()],
            sum_threshold: dec!(0.96),
            min_momentum_1s: 0.001,
            event_refresh_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            prefer_close_to_end: true,
            default_shares: 100,
            take_profit: dec!(0.02),
            stop_loss: dec!(0.05),
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}

/// Internal position tracking
#[derive(Debug, Clone)]
struct TrackedPosition {
    market_slug: String,
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    #[allow(dead_code)]
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

        let mut status = AgentStatus::Running;
        let mut positions: HashMap<String, TrackedPosition> = HashMap::new();
        let mut active_events: HashMap<String, EventInfo> = HashMap::new(); // symbol -> event
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let daily_pnl = Decimal::ZERO;
        let mut total_exposure = Decimal::ZERO;

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

                    // Find the active UP/DOWN event for this symbol
                    let event = match active_events.get(&update.symbol) {
                        Some(e) => e,
                        None => {
                            debug!(agent = self.config.agent_id, symbol = %update.symbol, "no active event yet");
                            continue;
                        }
                    };

                    // Get PM quotes for UP/DOWN token IDs
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

                    // Check momentum from binance price cache
                    let spot_cache = self.binance_ws.price_cache();
                    let momentum = spot_cache.momentum(&update.symbol, 1).await;

                    // Signal detection: sum_of_asks < threshold
                    let sum_of_asks = up_ask + down_ask;
                    if sum_of_asks >= self.config.sum_threshold {
                        continue;
                    }

                    // Already have a position in this market
                    if positions.contains_key(&event.slug) {
                        continue;
                    }

                    // Check momentum threshold
                    let mom_ok = momentum
                        .map(|m| m.abs() >= Decimal::try_from(self.config.min_momentum_1s).unwrap_or(dec!(0.001)))
                        .unwrap_or(true);

                    if !mom_ok {
                        continue;
                    }

                    // Determine side from momentum direction
                    let side = if momentum.map(|m| m > Decimal::ZERO).unwrap_or(true) {
                        Side::Up
                    } else {
                        Side::Down
                    };

                    let (token_id, limit_price) = match side {
                        Side::Up => (event.up_token_id.clone(), up_ask),
                        Side::Down => (event.down_token_id.clone(), down_ask),
                    };

                    let intent = OrderIntent::new(
                        &self.config.agent_id,
                        Domain::Crypto,
                        event.slug.as_str(),
                        &token_id,
                        side,
                        true,
                        self.config.default_shares,
                        limit_price,
                    )
                    .with_priority(OrderPriority::Normal)
                    .with_metadata("strategy", "crypto_momentum")
                    .with_metadata("coin", &coin)
                    .with_metadata("sum_of_asks", &sum_of_asks.to_string())
                    .with_metadata("event_title", &event.title);

                    info!(
                        agent = self.config.agent_id,
                        slug = %event.slug,
                        %sum_of_asks,
                        %side,
                        %limit_price,
                        "signal detected, submitting order"
                    );

                    if let Err(e) = ctx.submit_order(intent).await {
                        warn!(agent = self.config.agent_id, error = %e, "failed to submit order");
                    }

                    // Track position locally
                    positions.insert(event.slug.clone(), TrackedPosition {
                        market_slug: event.slug.clone(),
                        token_id,
                        side,
                        shares: self.config.default_shares,
                        entry_price: limit_price,
                        entry_time: Utc::now(),
                        is_hedged: false,
                    });

                    total_exposure = positions.values()
                        .map(|p| p.entry_price * Decimal::from(p.shares))
                        .sum();
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

                    // Update quote cache for exit checks
                    let _ = update; // Quote updates are consumed via quote_cache above
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
                                    bid, // best-effort sell
                                )
                                .with_priority(OrderPriority::Critical)
                                .with_metadata("exit_reason", "force_close");

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

        info!(agent = self.config.agent_id, "crypto agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = CryptoTradingConfig::default();
        assert_eq!(cfg.agent_id, "crypto");
        assert_eq!(cfg.coins.len(), 3);
        assert_eq!(cfg.sum_threshold, dec!(0.96));
    }
}
