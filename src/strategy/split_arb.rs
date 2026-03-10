//! Time-Separated Split Arbitrage Strategy (gabagool22 style)
//!
//! Core strategy:
//! 1. Wait for UP to drop below threshold (e.g., 35¢), buy UP
//! 2. Wait for DOWN to drop below threshold (e.g., 35¢), buy DOWN
//! 3. If avg(UP) + avg(DOWN) < 99¢, profit is locked
//! 4. One side always settles at $1.00, guaranteed profit
//!
//! Key insight: Don't need to buy both sides simultaneously.
//! Retail panic creates mispricings at different times.

use crate::adapters::PolymarketClient;
use crate::domain::Side;
use crate::error::{PloyError, Result};
use crate::data_plane::{DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane};
use crate::strategy::OrderExecutor;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

mod runtime_flow;

/// Simple local price cache for split arbitrage
#[derive(Debug, Clone, Default)]
pub struct PriceCache {
    /// Map token_id -> (best_bid, best_ask, timestamp)
    prices: HashMap<String, (Option<Decimal>, Option<Decimal>, DateTime<Utc>)>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, token_id: &str, bid: Option<Decimal>, ask: Option<Decimal>) {
        self.prices
            .insert(token_id.to_string(), (bid, ask, Utc::now()));
    }

    pub fn get_ask(&self, token_id: &str) -> Option<Decimal> {
        self.prices.get(token_id).and_then(|(_, ask, _)| *ask)
    }

    pub fn get_bid(&self, token_id: &str) -> Option<Decimal> {
        self.prices.get(token_id).and_then(|(bid, _, _)| *bid)
    }
}

/// Configuration for split arbitrage strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitArbConfig {
    /// Maximum price to enter a position (e.g., 0.35 = 35¢)
    pub max_entry_price: Decimal,

    /// Target total cost for both sides (e.g., 0.70 = 70¢)
    /// Profit = $1.00 - total_cost
    pub target_total_cost: Decimal,

    /// Minimum profit margin required (e.g., 0.05 = 5¢ per pair)
    pub min_profit_margin: Decimal,

    /// Maximum time to wait for hedge (seconds)
    pub max_hedge_wait_secs: u64,

    /// Shares per trade
    pub shares_per_trade: u64,

    /// Maximum concurrent positions (unhedged)
    pub max_unhedged_positions: usize,

    /// Stop loss percentage for unhedged exit (e.g., 0.10 = 10%)
    pub unhedged_stop_loss: Decimal,

    /// Taker fee rate per leg (e.g., 0.02 = 2%)
    pub fee_rate: Decimal,

    /// Series IDs to monitor
    pub series_ids: Vec<String>,
}

impl Default for SplitArbConfig {
    fn default() -> Self {
        Self {
            max_entry_price: dec!(0.35),    // Max 35¢ per side
            target_total_cost: dec!(0.70),  // Target 70¢ total (30¢ profit)
            min_profit_margin: dec!(0.05),  // Min 5¢ profit
            max_hedge_wait_secs: 900,       // 15 minutes max wait
            shares_per_trade: 100,          // ~$35 per leg
            max_unhedged_positions: 3,      // Max 3 unhedged at once
            unhedged_stop_loss: dec!(0.15), // 15% stop loss on unhedged
            fee_rate: dec!(0.02),           // 2% taker fee per leg
            series_ids: vec![
                "10423".into(), // SOL 15m
                "10191".into(), // ETH 15m
                "41".into(),    // BTC daily
            ],
        }
    }
}

/// Tracks a partial position waiting for hedge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialPosition {
    /// Event/market identifier
    pub event_id: String,

    /// Market condition ID
    pub condition_id: String,

    /// Which side we bought first
    pub first_side: ArbSide,

    /// Token ID of first side
    pub first_token_id: String,

    /// Entry price of first side
    pub first_entry_price: Decimal,

    /// Shares bought
    pub shares: u64,

    /// When we entered
    pub entry_time: DateTime<Utc>,

    /// Event end time (for timeout)
    pub event_end_time: DateTime<Utc>,

    /// Token ID of the other side (for hedging)
    pub other_token_id: String,

    /// Current status
    pub status: PositionStatus,

    /// Maximum price we can pay for hedge to hit target profit
    pub max_hedge_price: Decimal,

    /// Whether the first leg fill has been confirmed
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArbSide {
    Up,
    Down,
}

impl std::fmt::Display for ArbSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArbSide::Up => write!(f, "UP"),
            ArbSide::Down => write!(f, "DOWN"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionStatus {
    /// Waiting for hedge opportunity
    WaitingForHedge,
    /// Hedge order placed
    HedgePending,
    /// Fully hedged, profit locked
    Hedged,
    /// Exited without hedge (stopped out or timed out)
    ExitedUnhedged,
}

/// A fully hedged position with locked profit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HedgedPosition {
    pub event_id: String,
    pub condition_id: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub up_entry_price: Decimal,
    pub down_entry_price: Decimal,
    pub total_cost: Decimal,
    pub locked_profit: Decimal,
    pub shares: u64,
    pub entry_time: DateTime<Utc>,
    pub hedge_time: DateTime<Utc>,
    pub event_end_time: DateTime<Utc>,
}

/// Market info for monitoring
#[derive(Debug, Clone)]
pub struct MonitoredMarket {
    pub event_id: String,
    pub condition_id: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub event_end_time: DateTime<Utc>,
    pub series_id: String,
}

/// Split Arbitrage Engine
pub struct SplitArbEngine {
    config: SplitArbConfig,
    client: PolymarketClient,
    executor: OrderExecutor,
    price_cache: Arc<RwLock<PriceCache>>,

    /// Unhedged positions waiting for hedge
    partial_positions: Arc<RwLock<HashMap<String, PartialPosition>>>,

    /// Fully hedged positions
    hedged_positions: Arc<RwLock<Vec<HedgedPosition>>>,

    /// Markets we're monitoring
    monitored_markets: Arc<RwLock<HashMap<String, MonitoredMarket>>>,

    /// Dry run mode
    dry_run: bool,

    /// Stats tracking
    stats: Arc<RwLock<ArbStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct ArbStats {
    pub signals_detected: u64,
    pub first_leg_entries: u64,
    pub hedges_completed: u64,
    pub unhedged_exits: u64,
    pub total_profit: Decimal,
    pub total_loss: Decimal,
}

impl SplitArbEngine {
    pub fn new(
        config: SplitArbConfig,
        client: PolymarketClient,
        executor: OrderExecutor,
        dry_run: bool,
    ) -> Self {
        Self {
            config,
            client,
            executor,
            price_cache: Arc::new(RwLock::new(PriceCache::new())),
            partial_positions: Arc::new(RwLock::new(HashMap::new())),
            hedged_positions: Arc::new(RwLock::new(Vec::new())),
            monitored_markets: Arc::new(RwLock::new(HashMap::new())),
            dry_run,
            stats: Arc::new(RwLock::new(ArbStats::default())),
        }
    }

    /// Initialize markets to monitor
    pub async fn initialize(&self) -> Result<Vec<String>> {
        let mut all_token_ids = Vec::new();
        let mut markets = self.monitored_markets.write().await;

        for series_id in &self.config.series_ids {
            info!("Fetching events for series {}", series_id);

            let events = match self.client.get_all_active_events(series_id).await {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to fetch series {}: {}", series_id, e);
                    continue;
                }
            };

            info!("Found {} events in series {}", events.len(), series_id);

            // Process up to 5 events per series
            for event in events.into_iter().take(5) {
                // Get event details
                let details = match self.client.get_event_details(&event.id).await {
                    Ok(d) => d,
                    Err(e) => {
                        debug!("Failed to get event details for {}: {}", event.id, e);
                        continue;
                    }
                };

                let market = match details.markets.first() {
                    Some(m) => m,
                    None => continue,
                };

                let condition_id = match &market.condition_id {
                    Some(cid) => cid.clone(),
                    None => continue,
                };

                // Get CLOB market for token IDs
                let clob_market = match self.client.get_market(&condition_id).await {
                    Ok(m) => m,
                    Err(e) => {
                        debug!("Failed to get CLOB market {}: {}", condition_id, e);
                        continue;
                    }
                };

                // Find UP and DOWN tokens
                let up_token = clob_market.tokens.iter().find(|t| {
                    let outcome = t.outcome.to_lowercase();
                    outcome.contains("up") || outcome == "yes"
                });

                let down_token = clob_market.tokens.iter().find(|t| {
                    let outcome = t.outcome.to_lowercase();
                    outcome.contains("down") || outcome == "no"
                });

                let (up_token, down_token) = match (up_token, down_token) {
                    (Some(u), Some(d)) => (u, d),
                    _ => continue,
                };

                // Parse end time
                let end_time = details
                    .end_date
                    .as_ref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|| Utc::now() + Duration::hours(24));

                let market_info = MonitoredMarket {
                    event_id: event.id.clone(),
                    condition_id: condition_id.clone(),
                    up_token_id: up_token.token_id.clone(),
                    down_token_id: down_token.token_id.clone(),
                    event_end_time: end_time,
                    series_id: series_id.clone(),
                };

                all_token_ids.push(up_token.token_id.clone());
                all_token_ids.push(down_token.token_id.clone());

                markets.insert(condition_id.clone(), market_info);
            }
        }

        info!(
            "Monitoring {} markets, {} tokens",
            markets.len(),
            all_token_ids.len()
        );
        Ok(all_token_ids)
    }
}

/// Run the split arbitrage strategy
pub async fn run_split_arb(
    config: SplitArbConfig,
    client: PolymarketClient,
    executor: OrderExecutor,
    dry_run: bool,
) -> Result<()> {
    let engine = SplitArbEngine::new(config, client, executor, dry_run);

    // Initialize markets
    let token_ids = engine.initialize().await?;

    if token_ids.is_empty() {
        warn!("No markets to monitor!");
        return Ok(());
    }

    info!("Found {} tokens to monitor", token_ids.len());

    // Build a local data plane instead of creating standalone WS adapters.
    let data_plane = Arc::new(PlatformDataPlane::new(
        DataPlaneConfig {
            polymarket_ws_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            ..Default::default()
        },
        Arc::new(DataPlaneFreshness::new()),
    ));
    let pm_ws = data_plane.polymarket_ws().ok_or_else(|| {
        PloyError::Validation("split_arb data plane missing polymarket websocket".to_string())
    })?;

    // Token list is generated as [up, down, up, down, ...].
    for chunk in token_ids.chunks(2) {
        pm_ws.register_token(&chunk[0], Side::Up).await;
        if let Some(token_id) = chunk.get(1) {
            pm_ws.register_token(token_id, Side::Down).await;
        }
    }
    data_plane.start(Vec::new()).await?;

    let quote_rx = pm_ws.subscribe_updates();

    // Spawn status printer
    let engine_clone = engine.stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let stats = engine_clone.read().await;
            info!(
                "📊 Stats: {} signals, {} entries, {} hedged, {} exits, P&L: ${:.2}",
                stats.signals_detected,
                stats.first_leg_entries,
                stats.hedges_completed,
                stats.unhedged_exits,
                stats.total_profit - stats.total_loss
            );
        }
    });

    // Run engine
    engine.run(quote_rx).await?;

    Ok(())
}
