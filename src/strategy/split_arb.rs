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

use crate::adapters::{PolymarketClient, PolymarketWebSocket, QuoteUpdate};
use crate::domain::Side;
use crate::error::Result;
use crate::strategy::OrderExecutor;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

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
        self.prices.insert(token_id.to_string(), (bid, ask, Utc::now()));
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

    /// Series IDs to monitor
    pub series_ids: Vec<String>,
}

impl Default for SplitArbConfig {
    fn default() -> Self {
        Self {
            max_entry_price: dec!(0.35),      // Max 35¢ per side
            target_total_cost: dec!(0.70),    // Target 70¢ total (30¢ profit)
            min_profit_margin: dec!(0.05),    // Min 5¢ profit
            max_hedge_wait_secs: 900,         // 15 minutes max wait
            shares_per_trade: 100,            // ~$35 per leg
            max_unhedged_positions: 3,        // Max 3 unhedged at once
            unhedged_stop_loss: dec!(0.15),   // 15% stop loss on unhedged
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
                let end_time = details.end_date
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

        info!("Monitoring {} markets, {} tokens", markets.len(), all_token_ids.len());
        Ok(all_token_ids)
    }

    /// Main run loop
    pub async fn run(&self, mut quote_rx: broadcast::Receiver<QuoteUpdate>) -> Result<()> {
        info!("Split Arbitrage Engine started");
        info!(
            "Config: max_entry={}¢, target_total={}¢, min_profit={}¢",
            self.config.max_entry_price * dec!(100),
            self.config.target_total_cost * dec!(100),
            self.config.min_profit_margin * dec!(100)
        );

        let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            tokio::select! {
                // Process quote updates
                Ok(update) = quote_rx.recv() => {
                    self.on_quote_update(update).await;
                }

                // Periodic checks (timeout, stop loss, etc.)
                _ = check_interval.tick() => {
                    self.check_positions().await;
                }
            }
        }
    }

    /// Handle quote update
    async fn on_quote_update(&self, update: QuoteUpdate) {
        // Update cache with bid/ask from the quote
        {
            let mut cache = self.price_cache.write().await;
            cache.update(
                &update.token_id,
                update.quote.best_bid,
                update.quote.best_ask,
            );
        }

        // Check for opportunities on this token
        self.check_opportunity(&update.token_id).await;
    }

    /// Check for entry or hedge opportunity
    async fn check_opportunity(&self, token_id: &str) {
        let markets = self.monitored_markets.read().await;
        let cache = self.price_cache.read().await;

        // Find which market this token belongs to
        let market = markets.values().find(|m| {
            m.up_token_id == token_id || m.down_token_id == token_id
        });

        let market = match market {
            Some(m) => m.clone(),
            None => return,
        };

        drop(markets);

        // Get current prices
        let up_price = cache.get_ask(&market.up_token_id);
        let down_price = cache.get_ask(&market.down_token_id);

        let (up_ask, down_ask) = match (up_price, down_price) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };

        drop(cache);

        // Check if we have a partial position in this market
        let partial_positions = self.partial_positions.read().await;
        let has_partial = partial_positions.contains_key(&market.condition_id);
        drop(partial_positions);

        if has_partial {
            // Check for hedge opportunity
            self.check_hedge(&market.condition_id, up_ask, down_ask).await;
        } else {
            // Check for new entry opportunity
            self.check_new_entry(&market, up_ask, down_ask).await;
        }
    }

    /// Check for new entry opportunity
    async fn check_new_entry(&self, market: &MonitoredMarket, up_ask: Decimal, down_ask: Decimal) {
        // Check position limits
        let partial_count = self.partial_positions.read().await.len();
        if partial_count >= self.config.max_unhedged_positions {
            return;
        }

        // Check if either side is cheap enough
        let (side, entry_price, token_id, other_token_id) = if up_ask <= self.config.max_entry_price {
            (ArbSide::Up, up_ask, &market.up_token_id, &market.down_token_id)
        } else if down_ask <= self.config.max_entry_price {
            (ArbSide::Down, down_ask, &market.down_token_id, &market.up_token_id)
        } else {
            return; // Neither side is cheap enough
        };

        // Calculate max hedge price to hit target
        let max_hedge_price = self.config.target_total_cost - entry_price;

        // Check if hedge is even possible (other side not already too expensive)
        let other_ask = if side == ArbSide::Up { down_ask } else { up_ask };
        if other_ask > max_hedge_price + dec!(0.10) {
            // Other side is way too expensive, unlikely to get hedge
            debug!(
                "Skipping {} entry at {}¢ - other side at {}¢ (max hedge: {}¢)",
                side,
                entry_price * dec!(100),
                other_ask * dec!(100),
                max_hedge_price * dec!(100)
            );
            return;
        }

        // Signal detected!
        {
            let mut stats = self.stats.write().await;
            stats.signals_detected += 1;
        }

        info!(
            "🎯 ENTRY SIGNAL: {} @ {}¢ (market: {}, max hedge: {}¢)",
            side,
            entry_price * dec!(100),
            &market.condition_id[..8],
            max_hedge_price * dec!(100)
        );

        // Execute entry
        if self.dry_run {
            info!("  [DRY RUN] Would buy {} shares of {}", self.config.shares_per_trade, side);
        } else {
            // Place order
            match self.execute_buy(token_id, entry_price, self.config.shares_per_trade).await {
                Ok(_) => {
                    info!("  ✓ Order placed for {} @ {}¢", side, entry_price * dec!(100));
                }
                Err(e) => {
                    error!("  ✗ Order failed: {}", e);
                    return;
                }
            }
        }

        // Record partial position
        let position = PartialPosition {
            event_id: market.event_id.clone(),
            condition_id: market.condition_id.clone(),
            first_side: side,
            first_token_id: token_id.clone(),
            first_entry_price: entry_price,
            shares: self.config.shares_per_trade,
            entry_time: Utc::now(),
            event_end_time: market.event_end_time,
            other_token_id: other_token_id.clone(),
            status: PositionStatus::WaitingForHedge,
            max_hedge_price,
        };

        {
            let mut positions = self.partial_positions.write().await;
            positions.insert(market.condition_id.clone(), position);
        }

        {
            let mut stats = self.stats.write().await;
            stats.first_leg_entries += 1;
        }
    }

    /// Check for hedge opportunity on existing position
    async fn check_hedge(&self, condition_id: &str, up_ask: Decimal, down_ask: Decimal) {
        let mut positions = self.partial_positions.write().await;

        let position = match positions.get_mut(condition_id) {
            Some(p) if p.status == PositionStatus::WaitingForHedge => p,
            _ => return,
        };

        // Get the price of the side we need to hedge
        let hedge_price = match position.first_side {
            ArbSide::Up => down_ask,
            ArbSide::Down => up_ask,
        };

        // Check if hedge price is acceptable
        if hedge_price > position.max_hedge_price {
            return; // Too expensive
        }

        // Calculate locked profit
        let total_cost = position.first_entry_price + hedge_price;
        let locked_profit = Decimal::ONE - total_cost;

        if locked_profit < self.config.min_profit_margin {
            return; // Not enough profit
        }

        let hedge_side = match position.first_side {
            ArbSide::Up => ArbSide::Down,
            ArbSide::Down => ArbSide::Up,
        };

        info!(
            "🔒 HEDGE SIGNAL: {} @ {}¢ (total: {}¢, profit: {}¢)",
            hedge_side,
            hedge_price * dec!(100),
            total_cost * dec!(100),
            locked_profit * dec!(100)
        );

        // Execute hedge
        if self.dry_run {
            info!(
                "  [DRY RUN] Would buy {} shares of {} to hedge",
                position.shares, hedge_side
            );
        } else {
            match self.execute_buy(&position.other_token_id, hedge_price, position.shares).await {
                Ok(_) => {
                    info!("  ✓ Hedge order placed");
                }
                Err(e) => {
                    error!("  ✗ Hedge order failed: {}", e);
                    return;
                }
            }
        }

        // Create hedged position
        let hedged = HedgedPosition {
            event_id: position.event_id.clone(),
            condition_id: condition_id.to_string(),
            up_token_id: if position.first_side == ArbSide::Up {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            down_token_id: if position.first_side == ArbSide::Down {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            up_entry_price: if position.first_side == ArbSide::Up {
                position.first_entry_price
            } else {
                hedge_price
            },
            down_entry_price: if position.first_side == ArbSide::Down {
                position.first_entry_price
            } else {
                hedge_price
            },
            total_cost,
            locked_profit,
            shares: position.shares,
            entry_time: position.entry_time,
            hedge_time: Utc::now(),
            event_end_time: position.event_end_time,
        };

        // Move to hedged positions
        positions.remove(condition_id);
        drop(positions);

        {
            let mut hedged_positions = self.hedged_positions.write().await;
            hedged_positions.push(hedged.clone());
        }

        {
            let mut stats = self.stats.write().await;
            stats.hedges_completed += 1;
            stats.total_profit += locked_profit * Decimal::from(hedged.shares);
        }

        info!(
            "✅ POSITION HEDGED: Total cost {}¢, Locked profit {}¢/share (${:.2} total)",
            total_cost * dec!(100),
            locked_profit * dec!(100),
            locked_profit * Decimal::from(hedged.shares)
        );
    }

    /// Periodic position checks (timeout, stop loss)
    async fn check_positions(&self) {
        let now = Utc::now();
        let mut to_remove = Vec::new();

        {
            let positions = self.partial_positions.read().await;

            for (condition_id, position) in positions.iter() {
                // Check timeout
                let elapsed = now - position.entry_time;
                let max_wait = Duration::seconds(self.config.max_hedge_wait_secs as i64);

                if elapsed > max_wait {
                    warn!(
                        "⏱️ Position timed out: {} {} @ {}¢ (waited {}s)",
                        position.first_side,
                        &condition_id[..8],
                        position.first_entry_price * dec!(100),
                        elapsed.num_seconds()
                    );
                    to_remove.push((condition_id.clone(), "timeout".to_string()));
                    continue;
                }

                // Check if event is about to end
                let time_to_end = position.event_end_time - now;
                if time_to_end < Duration::seconds(30) {
                    warn!(
                        "⏰ Event ending soon, exiting unhedged: {} {}",
                        position.first_side,
                        &condition_id[..8]
                    );
                    to_remove.push((condition_id.clone(), "event_ending".to_string()));
                }
            }
        }

        // Exit unhedged positions
        for (condition_id, reason) in to_remove {
            self.exit_unhedged(&condition_id, &reason).await;
        }
    }

    /// Exit an unhedged position
    async fn exit_unhedged(&self, condition_id: &str, reason: &str) {
        let mut positions = self.partial_positions.write().await;

        let position = match positions.remove(condition_id) {
            Some(p) => p,
            None => return,
        };

        drop(positions);

        // Get current bid for our position
        let cache = self.price_cache.read().await;
        let current_bid = cache.get_bid(&position.first_token_id);
        drop(cache);

        let exit_price = current_bid.unwrap_or(position.first_entry_price);
        let pnl = exit_price - position.first_entry_price;
        let pnl_total = pnl * Decimal::from(position.shares);

        info!(
            "🚪 EXITING UNHEDGED: {} @ {}¢ → {}¢ ({}: {:.2}¢/share, ${:.2} total)",
            position.first_side,
            position.first_entry_price * dec!(100),
            exit_price * dec!(100),
            reason,
            pnl * dec!(100),
            pnl_total
        );

        if !self.dry_run {
            // Place sell order
            if let Err(e) = self.execute_sell(&position.first_token_id, exit_price, position.shares).await {
                error!("  ✗ Exit order failed: {}", e);
            }
        }

        {
            let mut stats = self.stats.write().await;
            stats.unhedged_exits += 1;
            if pnl_total > Decimal::ZERO {
                stats.total_profit += pnl_total;
            } else {
                stats.total_loss += pnl_total.abs();
            }
        }
    }

    /// Execute a buy order
    async fn execute_buy(&self, token_id: &str, price: Decimal, shares: u64) -> Result<()> {
        let order = crate::domain::OrderRequest::buy_limit(
            token_id.to_string(),
            Side::Up, // Side doesn't matter for token-based orders
            shares,
            price,
        );

        self.executor.execute(&order).await?;
        Ok(())
    }

    /// Execute a sell order
    async fn execute_sell(&self, token_id: &str, price: Decimal, shares: u64) -> Result<()> {
        let order = crate::domain::OrderRequest::sell_limit(
            token_id.to_string(),
            Side::Up,
            shares,
            price,
        );

        self.executor.execute(&order).await?;
        Ok(())
    }

    /// Get current stats
    pub async fn get_stats(&self) -> ArbStats {
        self.stats.read().await.clone()
    }

    /// Print status summary
    pub async fn print_status(&self) {
        let stats = self.stats.read().await;
        let partial = self.partial_positions.read().await;
        let hedged = self.hedged_positions.read().await;

        info!("═══════════════════════════════════════════");
        info!("Split Arbitrage Status");
        info!("───────────────────────────────────────────");
        info!("Signals detected:    {}", stats.signals_detected);
        info!("First leg entries:   {}", stats.first_leg_entries);
        info!("Hedges completed:    {}", stats.hedges_completed);
        info!("Unhedged exits:      {}", stats.unhedged_exits);
        info!("───────────────────────────────────────────");
        info!("Active unhedged:     {}", partial.len());
        info!("Active hedged:       {}", hedged.len());
        info!("───────────────────────────────────────────");
        info!("Total profit:        ${:.2}", stats.total_profit);
        info!("Total loss:          ${:.2}", stats.total_loss);
        info!("Net P&L:             ${:.2}", stats.total_profit - stats.total_loss);
        info!("═══════════════════════════════════════════");
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

    // Connect to WebSocket
    let pm_ws = PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");
    let quote_rx = pm_ws.subscribe_updates();

    // Spawn WebSocket task
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = pm_ws.run(token_ids).await {
            error!("WebSocket error: {}", e);
        }
    });

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

    ws_handle.abort();
    Ok(())
}
