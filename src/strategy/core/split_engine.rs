//! Core Split Arbitrage Engine
//!
//! Generic split arbitrage logic that works across market types.

use super::{
    ArbSide, ArbStats, BinaryMarket, HedgedPosition, PartialPosition, PositionStatus, PriceCache,
};
use crate::adapters::PolymarketClient;
use crate::strategy::OrderExecutor;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

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
}

impl Default for SplitArbConfig {
    fn default() -> Self {
        Self {
            max_entry_price: dec!(0.35),
            target_total_cost: dec!(0.70),
            min_profit_margin: dec!(0.05),
            max_hedge_wait_secs: 900,
            shares_per_trade: 100,
            max_unhedged_positions: 3,
            unhedged_stop_loss: dec!(0.15),
        }
    }
}

/// Generic Split Arbitrage Engine
pub struct SplitArbEngine {
    config: SplitArbConfig,
    client: PolymarketClient,
    executor: OrderExecutor,
    price_cache: Arc<RwLock<PriceCache>>,

    /// Binary markets being monitored
    markets: Arc<RwLock<HashMap<String, BinaryMarket>>>,

    /// Unhedged positions waiting for hedge
    partial_positions: Arc<RwLock<HashMap<String, PartialPosition>>>,

    /// Fully hedged positions
    hedged_positions: Arc<RwLock<Vec<HedgedPosition>>>,

    /// Dry run mode
    dry_run: bool,

    /// Stats tracking
    stats: Arc<RwLock<ArbStats>>,
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
            markets: Arc::new(RwLock::new(HashMap::new())),
            partial_positions: Arc::new(RwLock::new(HashMap::new())),
            hedged_positions: Arc::new(RwLock::new(Vec::new())),
            dry_run,
            stats: Arc::new(RwLock::new(ArbStats::default())),
        }
    }

    /// Add markets to monitor
    pub async fn add_markets(&self, markets: Vec<BinaryMarket>) {
        let mut market_map = self.markets.write().await;
        for market in markets {
            market_map.insert(market.condition_id.clone(), market);
        }
    }

    /// Get all token IDs to subscribe to
    pub async fn get_token_ids(&self) -> Vec<String> {
        let markets = self.markets.read().await;
        let mut tokens = Vec::new();
        for market in markets.values() {
            tokens.push(market.yes_token_id.clone());
            tokens.push(market.no_token_id.clone());
        }
        tokens
    }

    /// Handle a price update
    pub async fn on_price_update(
        &self,
        token_id: &str,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
    ) {
        // Update cache
        {
            let mut cache = self.price_cache.write().await;
            cache.update(token_id, bid, ask);
        }

        // Find which market this token belongs to
        let markets = self.markets.read().await;
        for market in markets.values() {
            if market.yes_token_id == token_id || market.no_token_id == token_id {
                // Check for entry opportunity
                self.check_for_entry(market).await;

                // Check for hedge on existing positions
                self.check_for_hedge(&market.condition_id).await;
            }
        }
    }

    /// Check for entry opportunity on a market
    async fn check_for_entry(&self, market: &BinaryMarket) {
        // Skip if we already have a position in this market
        {
            let positions = self.partial_positions.read().await;
            if positions.contains_key(&market.condition_id) {
                return;
            }
        }

        // Check position limits
        {
            let positions = self.partial_positions.read().await;
            if positions.len() >= self.config.max_unhedged_positions {
                return;
            }
        }

        // Get current prices
        let cache = self.price_cache.read().await;
        let yes_ask = cache.get_ask(&market.yes_token_id);
        let no_ask = cache.get_ask(&market.no_token_id);
        drop(cache);

        let (yes_ask, no_ask) = match (yes_ask, no_ask) {
            (Some(y), Some(n)) => (y, n),
            _ => return, // Missing prices
        };

        // Determine which side to enter (if any)
        let (side, entry_price, token_id, other_token, label, other_label) =
            if yes_ask <= self.config.max_entry_price {
                (
                    ArbSide::Yes,
                    yes_ask,
                    &market.yes_token_id,
                    &market.no_token_id,
                    &market.yes_label,
                    &market.no_label,
                )
            } else if no_ask <= self.config.max_entry_price {
                (
                    ArbSide::No,
                    no_ask,
                    &market.no_token_id,
                    &market.yes_token_id,
                    &market.no_label,
                    &market.yes_label,
                )
            } else {
                return; // Neither side is cheap enough
            };

        // Calculate max hedge price
        let max_hedge_price = self.config.target_total_cost - entry_price;

        // Check if hedge is even possible
        let other_ask = if side == ArbSide::Yes {
            no_ask
        } else {
            yes_ask
        };
        if other_ask > max_hedge_price + dec!(0.10) {
            debug!(
                "Skipping {} entry at {}¢ - other side at {}¢ (max hedge: {}¢)",
                label,
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
            label,
            entry_price * dec!(100),
            &market.condition_id[..8.min(market.condition_id.len())],
            max_hedge_price * dec!(100)
        );

        // Execute entry
        if self.dry_run {
            info!(
                "  [DRY RUN] Would buy {} shares of {}",
                self.config.shares_per_trade, label
            );
        } else {
            // Place order (simplified - full implementation would use executor)
            info!(
                "  Placing order for {} shares of {}",
                self.config.shares_per_trade, label
            );
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
            event_end_time: market.end_time,
            other_token_id: other_token.clone(),
            status: PositionStatus::WaitingForHedge,
            max_hedge_price,
            first_side_label: label.clone(),
            other_side_label: other_label.clone(),
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
    async fn check_for_hedge(&self, condition_id: &str) {
        let mut positions = self.partial_positions.write().await;
        let position = match positions.get(condition_id) {
            Some(p) if p.status == PositionStatus::WaitingForHedge => p.clone(),
            _ => return,
        };

        // Get other side price
        let cache = self.price_cache.read().await;
        let hedge_ask = match cache.get_ask(&position.other_token_id) {
            Some(p) => p,
            None => return,
        };
        drop(cache);

        // Check if hedge price is acceptable
        if hedge_ask > position.max_hedge_price {
            return;
        }

        // Calculate locked profit
        let total_cost = position.first_entry_price + hedge_ask;
        let locked_profit = Decimal::ONE - total_cost;

        if locked_profit < self.config.min_profit_margin {
            return;
        }

        info!(
            "🔒 HEDGE SIGNAL: {} @ {}¢ (total: {}¢, profit: {}¢)",
            position.other_side_label,
            hedge_ask * dec!(100),
            total_cost * dec!(100),
            locked_profit * dec!(100)
        );

        // Execute hedge
        if self.dry_run {
            info!(
                "  [DRY RUN] Would buy {} shares of {} to hedge",
                position.shares, position.other_side_label
            );
        } else {
            info!("  Placing hedge order for {} shares", position.shares);
        }

        // Create hedged position
        let hedged = HedgedPosition {
            event_id: position.event_id.clone(),
            condition_id: condition_id.to_string(),
            yes_token_id: if position.first_side == ArbSide::Yes {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            no_token_id: if position.first_side == ArbSide::No {
                position.first_token_id.clone()
            } else {
                position.other_token_id.clone()
            },
            yes_entry_price: if position.first_side == ArbSide::Yes {
                position.first_entry_price
            } else {
                hedge_ask
            },
            no_entry_price: if position.first_side == ArbSide::No {
                position.first_entry_price
            } else {
                hedge_ask
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

    /// Print current stats
    pub async fn print_stats(&self) {
        let stats = self.stats.read().await;
        let _partial = self.partial_positions.read().await;
        let _hedged = self.hedged_positions.read().await;

        info!(
            "📊 Stats: {} signals, {} entries, {} hedged, {} exits, P&L: ${:.2}",
            stats.signals_detected,
            stats.first_leg_entries,
            stats.hedges_completed,
            stats.unhedged_exits,
            stats.net_pnl()
        );
    }

    /// Get config reference
    pub fn config(&self) -> &SplitArbConfig {
        &self.config
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}
