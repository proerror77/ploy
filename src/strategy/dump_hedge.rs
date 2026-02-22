//! Enhanced Dump & Hedge Strategy for Polymarket
//!
//! Optimizations:
//! 1. Dynamic sum_target based on time remaining
//! 2. Progressive/partial hedge execution
//! 3. Enhanced dump detection (price + volume + depth)
//! 4. Failed hedge timeout protection with stop-loss

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::domain::Side;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for Dump & Hedge strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpHedgeConfig {
    /// Base target sum for full hedge (e.g., 0.95 means 5% profit locked)
    pub base_sum_target: Decimal,
    /// Aggressive sum target when time is running out
    pub urgent_sum_target: Decimal,
    /// Minimum price drop to trigger Leg 1 (e.g., 0.15 = 15%)
    pub move_pct: Decimal,
    /// Reduced threshold for enhanced signals (volume + depth)
    pub enhanced_move_pct: Decimal,
    /// Detection window in seconds
    pub window_secs: u64,
    /// Shares per trade
    pub shares: u64,
    /// Maximum entry price for Leg 1
    pub max_leg1_price: Decimal,
    /// Minimum time remaining before event end (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time to wait for hedge before stop-loss (seconds)
    pub max_hedge_wait_secs: u64,
    /// Enable progressive hedging (multiple partial fills)
    pub progressive_hedge: bool,
    /// Minimum shares per progressive hedge leg
    pub min_progressive_shares: u64,
    /// Time threshold for urgent mode (seconds)
    pub urgent_time_threshold_secs: u64,
    /// Stop-loss percentage if hedge fails
    pub hedge_fail_stop_loss_pct: Decimal,
    /// Coin priority weights (higher = prefer)
    pub coin_priorities: HashMap<String, u8>,
}

impl Default for DumpHedgeConfig {
    fn default() -> Self {
        let mut coin_priorities = HashMap::new();
        coin_priorities.insert("BTCUSDT".to_string(), 1);
        coin_priorities.insert("ETHUSDT".to_string(), 2);
        coin_priorities.insert("SOLUSDT".to_string(), 3);
        coin_priorities.insert("XRPUSDT".to_string(), 4);

        Self {
            base_sum_target: dec!(0.95),          // 5% profit
            urgent_sum_target: dec!(0.98),        // 2% profit when urgent
            move_pct: dec!(0.15),                 // 15% drop triggers
            enhanced_move_pct: dec!(0.10),        // 10% with volume/depth confirmation
            window_secs: 5,                       // Detection window
            shares: 20,                           // Shares per leg
            max_leg1_price: dec!(0.40),           // Max 40¢ for Leg 1
            min_time_remaining_secs: 120,         // At least 2 min for hedge
            max_hedge_wait_secs: 180,             // 3 min max wait for hedge
            progressive_hedge: true,              // Enable partial hedging
            min_progressive_shares: 5,            // Min 5 shares per partial
            urgent_time_threshold_secs: 60,       // Last minute = urgent
            hedge_fail_stop_loss_pct: dec!(0.20), // 20% stop-loss if no hedge
            coin_priorities,
        }
    }
}

impl DumpHedgeConfig {
    /// Calculate dynamic sum target based on time remaining
    pub fn dynamic_sum_target(&self, time_remaining_secs: i64) -> Decimal {
        if time_remaining_secs < self.urgent_time_threshold_secs as i64 {
            // Urgent: accept smaller profit to complete hedge
            self.urgent_sum_target
        } else if time_remaining_secs < 180 {
            // Moderate urgency: interpolate
            let urgency = Decimal::from(180 - time_remaining_secs) / dec!(120);
            self.base_sum_target + (self.urgent_sum_target - self.base_sum_target) * urgency
        } else {
            // Normal: require full profit margin
            self.base_sum_target
        }
    }

    /// Get coin priority (lower = higher priority)
    pub fn get_priority(&self, symbol: &str) -> u8 {
        *self.coin_priorities.get(symbol).unwrap_or(&10)
    }
}

// ============================================================================
// Price Tracking with Volume/Depth
// ============================================================================

/// Enhanced price snapshot with market depth info
#[derive(Debug, Clone)]
pub struct EnhancedSnapshot {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    pub bid_depth: Option<Decimal>, // Total bid volume in top levels
    pub ask_depth: Option<Decimal>, // Total ask volume in top levels
    pub volume_spike: bool,         // Unusual volume detected
}

/// Tracks PM token price movements with enhanced signals
#[derive(Debug, Clone)]
pub struct TokenPriceTracker {
    /// Recent prices for dump detection
    recent_prices: HashMap<String, Vec<EnhancedSnapshot>>,
    /// Baseline bid depth per token
    baseline_depth: HashMap<String, Decimal>,
    /// Window size for detection
    window_secs: i64,
}

impl TokenPriceTracker {
    pub fn new(window_secs: u64) -> Self {
        Self {
            recent_prices: HashMap::new(),
            baseline_depth: HashMap::new(),
            window_secs: window_secs as i64,
        }
    }

    /// Record a new price update with optional depth info
    pub fn update(
        &mut self,
        token_id: &str,
        price: Decimal,
        bid_depth: Option<Decimal>,
        ask_depth: Option<Decimal>,
    ) {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(self.window_secs * 3);

        // Detect volume spike BEFORE getting mutable entry (to avoid borrow conflict)
        let volume_spike = {
            let snapshots = self.recent_prices.get(token_id);
            match snapshots {
                Some(s) => self.detect_volume_spike(s, bid_depth),
                None => false,
            }
        };

        let snapshots = self.recent_prices.entry(token_id.to_string()).or_default();

        // Update baseline depth
        if let Some(depth) = bid_depth {
            let baseline = self
                .baseline_depth
                .entry(token_id.to_string())
                .or_insert(depth);
            // Exponential moving average for baseline
            *baseline = *baseline * dec!(0.95) + depth * dec!(0.05);
        }

        // Add new snapshot
        snapshots.push(EnhancedSnapshot {
            price,
            timestamp: now,
            bid_depth,
            ask_depth,
            volume_spike,
        });

        // Prune old snapshots
        snapshots.retain(|s| s.timestamp > cutoff);
    }

    /// Detect if current volume is unusual
    fn detect_volume_spike(
        &self,
        snapshots: &[EnhancedSnapshot],
        current_depth: Option<Decimal>,
    ) -> bool {
        if snapshots.len() < 5 {
            return false;
        }

        let current = match current_depth {
            Some(d) => d,
            None => return false,
        };

        // Calculate average depth from recent snapshots
        let depths: Vec<Decimal> = snapshots.iter().filter_map(|s| s.bid_depth).collect();

        if depths.is_empty() {
            return false;
        }

        let avg_depth: Decimal = depths.iter().sum::<Decimal>() / Decimal::from(depths.len());

        // Volume spike if current depth is 50% below average (depth collapse)
        current < avg_depth * dec!(0.5)
    }

    /// Check if price dropped by at least `move_pct` within the window
    pub fn detect_dump(
        &self,
        token_id: &str,
        config: &DumpHedgeConfig,
    ) -> Option<EnhancedDumpSignal> {
        let snapshots = self.recent_prices.get(token_id)?;
        if snapshots.len() < 2 {
            return None;
        }

        let now = Utc::now();
        let window_start = now - Duration::seconds(self.window_secs);

        // Find max price in window
        let mut max_price = Decimal::ZERO;
        let mut max_time = now;

        for snap in snapshots.iter() {
            if snap.timestamp >= window_start && snap.price > max_price {
                max_price = snap.price;
                max_time = snap.timestamp;
            }
        }

        // Get current snapshot (last one)
        let current = snapshots.last()?;

        // Only count as dump if max was before current
        if max_time >= current.timestamp {
            return None;
        }

        // Calculate drop percentage
        if max_price.is_zero() {
            return None;
        }

        let drop_pct = (max_price - current.price) / max_price;

        // Check for enhanced signals
        let has_depth_collapse = self.check_depth_collapse(token_id);
        let has_volume_spike = current.volume_spike;

        // Determine required threshold
        let required_drop = if has_depth_collapse || has_volume_spike {
            config.enhanced_move_pct // Lower threshold with confirmation
        } else {
            config.move_pct // Standard threshold
        };

        if drop_pct >= required_drop {
            let elapsed_ms = (current.timestamp - max_time).num_milliseconds();

            info!(
                "🔴 DUMP detected: {} dropped {:.1}% in {}ms (from {:.1}¢ to {:.1}¢) [depth_collapse={}, volume_spike={}]",
                &token_id[..20.min(token_id.len())],
                drop_pct * dec!(100),
                elapsed_ms,
                max_price * dec!(100),
                current.price * dec!(100),
                has_depth_collapse,
                has_volume_spike
            );

            return Some(EnhancedDumpSignal {
                token_id: token_id.to_string(),
                drop_pct,
                from_price: max_price,
                to_price: current.price,
                elapsed_ms: elapsed_ms as u64,
                timestamp: current.timestamp,
                has_depth_collapse,
                has_volume_spike,
                signal_strength: self.calculate_signal_strength(
                    drop_pct,
                    has_depth_collapse,
                    has_volume_spike,
                ),
            });
        }

        None
    }

    /// Check if bid depth has collapsed
    fn check_depth_collapse(&self, token_id: &str) -> bool {
        let snapshots = match self.recent_prices.get(token_id) {
            Some(s) => s,
            None => return false,
        };

        let baseline = match self.baseline_depth.get(token_id) {
            Some(b) => *b,
            None => return false,
        };

        let current = match snapshots.last().and_then(|s| s.bid_depth) {
            Some(d) => d,
            None => return false,
        };

        // Depth collapse if current is 60% below baseline
        current < baseline * dec!(0.4)
    }

    /// Calculate signal strength (0.0 - 1.0)
    fn calculate_signal_strength(
        &self,
        drop_pct: Decimal,
        depth_collapse: bool,
        volume_spike: bool,
    ) -> f64 {
        let mut strength = 0.0;

        // Drop percentage contribution (0-0.5)
        let drop_f64 = drop_pct.to_string().parse::<f64>().unwrap_or(0.0);
        strength += (drop_f64 * 2.5).min(0.5);

        // Depth collapse contribution (0-0.25)
        if depth_collapse {
            strength += 0.25;
        }

        // Volume spike contribution (0-0.25)
        if volume_spike {
            strength += 0.25;
        }

        strength.min(1.0)
    }

    /// Get current price for a token
    pub fn current_price(&self, token_id: &str) -> Option<Decimal> {
        self.recent_prices.get(token_id)?.last().map(|s| s.price)
    }
}

// ============================================================================
// Signal Types
// ============================================================================

/// Enhanced signal that a dump was detected
#[derive(Debug, Clone)]
pub struct EnhancedDumpSignal {
    pub token_id: String,
    pub drop_pct: Decimal,
    pub from_price: Decimal,
    pub to_price: Decimal,
    pub elapsed_ms: u64,
    pub timestamp: DateTime<Utc>,
    pub has_depth_collapse: bool,
    pub has_volume_spike: bool,
    pub signal_strength: f64,
}

/// Active Leg 1 position waiting for hedge
#[derive(Debug, Clone)]
pub struct PendingHedge {
    pub event_id: String,
    pub symbol: String,
    pub leg1_token_id: String,
    pub leg1_side: Side,
    pub leg1_price: Decimal,
    pub leg1_shares: u64,
    pub leg1_time: DateTime<Utc>,
    pub opposite_token_id: String,
    /// Shares already hedged (for progressive hedging)
    pub hedged_shares: u64,
    /// Average price of hedged shares
    pub avg_hedge_price: Decimal,
    /// Time remaining when Leg 1 was executed
    pub time_remaining_at_entry: i64,
}

impl PendingHedge {
    /// Calculate remaining shares to hedge
    pub fn remaining_shares(&self) -> u64 {
        self.leg1_shares.saturating_sub(self.hedged_shares)
    }

    /// Check if fully hedged
    pub fn is_fully_hedged(&self) -> bool {
        self.hedged_shares >= self.leg1_shares
    }

    /// Calculate current P&L if we close now
    pub fn calculate_pnl(&self, current_leg1_price: Decimal) -> Decimal {
        let leg1_cost = self.leg1_price * Decimal::from(self.leg1_shares);
        let hedge_cost = self.avg_hedge_price * Decimal::from(self.hedged_shares);
        let unhedged_value = current_leg1_price * Decimal::from(self.remaining_shares());

        // If fully hedged: profit = $1 - (leg1_cost + hedge_cost)
        // If partial: estimated value of unhedged + hedge profit
        if self.is_fully_hedged() {
            Decimal::from(self.leg1_shares) - (leg1_cost + hedge_cost)
        } else {
            // Partial: hedge profit + mark-to-market unhedged
            let hedge_profit = Decimal::from(self.hedged_shares)
                - (self.leg1_price * Decimal::from(self.hedged_shares) + hedge_cost);
            hedge_profit + unhedged_value
                - (self.leg1_price * Decimal::from(self.remaining_shares()))
        }
    }

    /// Time elapsed since Leg 1
    pub fn elapsed_secs(&self) -> i64 {
        (Utc::now() - self.leg1_time).num_seconds()
    }
}

/// Progressive hedge opportunity
#[derive(Debug, Clone)]
pub struct ProgressiveHedgeSignal {
    pub event_id: String,
    pub pending: PendingHedge,
    pub leg2_ask: Decimal,
    pub shares_to_hedge: u64,
    pub sum: Decimal,
    pub locked_profit_pct: Decimal,
    pub is_urgent: bool,
}

/// Complete hedge result
#[derive(Debug, Clone)]
pub struct HedgeResult {
    pub event_id: String,
    pub total_leg1_cost: Decimal,
    pub total_leg2_cost: Decimal,
    pub total_shares: u64,
    pub locked_profit: Decimal,
    pub locked_profit_pct: Decimal,
}

/// Stop-loss signal for failed hedge
#[derive(Debug, Clone)]
pub struct StopLossSignal {
    pub event_id: String,
    pub pending: PendingHedge,
    pub reason: StopLossReason,
    pub current_price: Decimal,
    pub loss_pct: Decimal,
}

#[derive(Debug, Clone)]
pub enum StopLossReason {
    HedgeTimeout,
    PriceCrash,
    EventEnding,
}

// ============================================================================
// Dump & Hedge Engine
// ============================================================================

/// Enhanced Dump & Hedge Strategy Engine
pub struct DumpHedgeEngine {
    config: DumpHedgeConfig,
    price_tracker: Arc<RwLock<TokenPriceTracker>>,
    pending_hedges: Arc<RwLock<HashMap<String, PendingHedge>>>,
    completed_hedges: Arc<RwLock<Vec<HedgeResult>>>,
}

impl DumpHedgeEngine {
    pub fn new(config: DumpHedgeConfig) -> Self {
        let window_secs = config.window_secs;
        Self {
            config,
            price_tracker: Arc::new(RwLock::new(TokenPriceTracker::new(window_secs))),
            pending_hedges: Arc::new(RwLock::new(HashMap::new())),
            completed_hedges: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Update price tracker with new PM price data
    pub async fn on_price_update(
        &self,
        token_id: &str,
        price: Decimal,
        bid_depth: Option<Decimal>,
        ask_depth: Option<Decimal>,
    ) {
        let mut tracker = self.price_tracker.write().await;
        tracker.update(token_id, price, bid_depth, ask_depth);
    }

    /// Simplified price update
    pub async fn on_simple_price_update(&self, token_id: &str, price: Decimal) {
        self.on_price_update(token_id, price, None, None).await;
    }

    /// Check for dump and potential Leg 1 entry
    pub async fn check_leg1_signal(
        &self,
        event_id: &str,
        symbol: &str,
        up_token_id: &str,
        down_token_id: &str,
        up_ask: Decimal,
        down_ask: Decimal,
        time_remaining_secs: i64,
    ) -> Option<EnhancedDumpSignal> {
        // Must have enough time to hedge
        if time_remaining_secs < self.config.min_time_remaining_secs as i64 {
            return None;
        }

        // Already have pending hedge for this event?
        {
            let pending = self.pending_hedges.read().await;
            if pending.contains_key(event_id) {
                return None;
            }
        }

        let tracker = self.price_tracker.read().await;

        // Check UP token for dump
        if let Some(mut signal) = tracker.detect_dump(up_token_id, &self.config) {
            if up_ask <= self.config.max_leg1_price {
                // Add symbol priority to signal strength
                let priority_bonus = (5 - self.config.get_priority(symbol).min(5)) as f64 * 0.05;
                signal.signal_strength = (signal.signal_strength + priority_bonus).min(1.0);
                return Some(signal);
            }
        }

        // Check DOWN token for dump
        if let Some(mut signal) = tracker.detect_dump(down_token_id, &self.config) {
            if down_ask <= self.config.max_leg1_price {
                let priority_bonus = (5 - self.config.get_priority(symbol).min(5)) as f64 * 0.05;
                signal.signal_strength = (signal.signal_strength + priority_bonus).min(1.0);
                return Some(signal);
            }
        }

        None
    }

    /// Record a Leg 1 entry
    pub async fn record_leg1(
        &self,
        event_id: &str,
        symbol: &str,
        leg1_token_id: &str,
        leg1_side: Side,
        leg1_price: Decimal,
        leg1_shares: u64,
        opposite_token_id: &str,
        time_remaining_secs: i64,
    ) {
        let pending = PendingHedge {
            event_id: event_id.to_string(),
            symbol: symbol.to_string(),
            leg1_token_id: leg1_token_id.to_string(),
            leg1_side,
            leg1_price,
            leg1_shares,
            leg1_time: Utc::now(),
            opposite_token_id: opposite_token_id.to_string(),
            hedged_shares: 0,
            avg_hedge_price: Decimal::ZERO,
            time_remaining_at_entry: time_remaining_secs,
        };

        let mut hedges = self.pending_hedges.write().await;
        hedges.insert(event_id.to_string(), pending);

        info!(
            "📝 Leg 1 recorded: {} {:?} @ {:.1}¢ x{} shares ({}s remaining)",
            symbol,
            leg1_side,
            leg1_price * dec!(100),
            leg1_shares,
            time_remaining_secs
        );
    }

    /// Check for hedge opportunity (supports progressive hedging)
    pub async fn check_hedge_signal(
        &self,
        event_id: &str,
        opposite_ask: Decimal,
        time_remaining_secs: i64,
    ) -> Option<ProgressiveHedgeSignal> {
        let pending = {
            let hedges = self.pending_hedges.read().await;
            hedges.get(event_id)?.clone()
        };

        if pending.is_fully_hedged() {
            return None;
        }

        let sum = pending.leg1_price + opposite_ask;
        let dynamic_target = self.config.dynamic_sum_target(time_remaining_secs);
        let is_urgent = time_remaining_secs < self.config.urgent_time_threshold_secs as i64;

        if sum <= dynamic_target {
            // Calculate shares to hedge
            let remaining = pending.remaining_shares();
            let shares_to_hedge = if self.config.progressive_hedge && !is_urgent {
                // Progressive: hedge in chunks
                remaining
                    .min(self.config.shares / 3)
                    .max(self.config.min_progressive_shares)
            } else {
                // Full hedge or urgent
                remaining
            };

            let locked_profit_pct = (dec!(1) - sum) / sum * dec!(100);

            info!(
                "✅ HEDGE {}: {} | Leg1={:.1}¢ + Leg2={:.1}¢ = {:.2} <= {:.2} | {} shares | Profit={:.1}%{}",
                if pending.hedged_shares > 0 { "PARTIAL" } else { "READY" },
                event_id,
                pending.leg1_price * dec!(100),
                opposite_ask * dec!(100),
                sum,
                dynamic_target,
                shares_to_hedge,
                locked_profit_pct,
                if is_urgent { " [URGENT]" } else { "" }
            );

            return Some(ProgressiveHedgeSignal {
                event_id: event_id.to_string(),
                pending,
                leg2_ask: opposite_ask,
                shares_to_hedge,
                sum,
                locked_profit_pct,
                is_urgent,
            });
        }

        debug!(
            "Hedge not ready: {} | {:.1}¢ + {:.1}¢ = {:.2} > {:.2}",
            event_id,
            pending.leg1_price * dec!(100),
            opposite_ask * dec!(100),
            sum,
            dynamic_target
        );

        None
    }

    /// Record partial hedge execution
    pub async fn record_partial_hedge(
        &self,
        event_id: &str,
        shares_hedged: u64,
        hedge_price: Decimal,
    ) -> Option<HedgeResult> {
        let mut hedges = self.pending_hedges.write().await;

        let pending = hedges.get_mut(event_id)?;

        // Update average hedge price
        let total_hedged_before = pending.hedged_shares;
        let new_total = total_hedged_before + shares_hedged;

        pending.avg_hedge_price = if total_hedged_before == 0 {
            hedge_price
        } else {
            (pending.avg_hedge_price * Decimal::from(total_hedged_before)
                + hedge_price * Decimal::from(shares_hedged))
                / Decimal::from(new_total)
        };
        pending.hedged_shares = new_total;

        info!(
            "📊 Partial hedge: {} hedged {}/{} shares @ {:.1}¢ (avg {:.1}¢)",
            event_id,
            new_total,
            pending.leg1_shares,
            hedge_price * dec!(100),
            pending.avg_hedge_price * dec!(100)
        );

        // Check if fully hedged
        if pending.is_fully_hedged() {
            let result = self.finalize_hedge(pending);
            hedges.remove(event_id);
            return Some(result);
        }

        None
    }

    /// Finalize a complete hedge
    fn finalize_hedge(&self, pending: &PendingHedge) -> HedgeResult {
        let total_leg1_cost = pending.leg1_price * Decimal::from(pending.leg1_shares);
        let total_leg2_cost = pending.avg_hedge_price * Decimal::from(pending.leg1_shares);
        let total_cost = total_leg1_cost + total_leg2_cost;
        let payout = Decimal::from(pending.leg1_shares);
        let locked_profit = payout - total_cost;
        let locked_profit_pct = locked_profit / total_cost * dec!(100);

        info!(
            "🎉 HEDGE COMPLETE: {} | Cost=${:.2} | Payout=${:.2} | Profit=${:.2} ({:.1}%)",
            pending.event_id, total_cost, payout, locked_profit, locked_profit_pct
        );

        HedgeResult {
            event_id: pending.event_id.clone(),
            total_leg1_cost,
            total_leg2_cost,
            total_shares: pending.leg1_shares,
            locked_profit,
            locked_profit_pct,
        }
    }

    /// Check for positions that need stop-loss (failed hedge)
    pub async fn check_stop_loss(
        &self,
        current_prices: &HashMap<String, Decimal>,
    ) -> Vec<StopLossSignal> {
        let hedges = self.pending_hedges.read().await;
        let mut signals = Vec::new();

        for pending in hedges.values() {
            let elapsed = pending.elapsed_secs();

            // Get current price of Leg 1 token
            let current_price = match current_prices.get(&pending.leg1_token_id) {
                Some(p) => *p,
                None => continue,
            };

            let loss_pct = if current_price < pending.leg1_price {
                (pending.leg1_price - current_price) / pending.leg1_price
            } else {
                Decimal::ZERO
            };

            // Check timeout
            if elapsed > self.config.max_hedge_wait_secs as i64 && !pending.is_fully_hedged() {
                warn!(
                    "⚠️ Hedge timeout: {} waited {}s, only {}/{} hedged",
                    pending.event_id, elapsed, pending.hedged_shares, pending.leg1_shares
                );
                signals.push(StopLossSignal {
                    event_id: pending.event_id.clone(),
                    pending: pending.clone(),
                    reason: StopLossReason::HedgeTimeout,
                    current_price,
                    loss_pct,
                });
                continue;
            }

            // Check price crash (beyond stop-loss threshold)
            if loss_pct >= self.config.hedge_fail_stop_loss_pct {
                warn!(
                    "⚠️ Price crash: {} dropped {:.1}% from entry",
                    pending.event_id,
                    loss_pct * dec!(100)
                );
                signals.push(StopLossSignal {
                    event_id: pending.event_id.clone(),
                    pending: pending.clone(),
                    reason: StopLossReason::PriceCrash,
                    current_price,
                    loss_pct,
                });
            }
        }

        signals
    }

    /// Remove a pending hedge (after stop-loss or completion)
    pub async fn remove_pending(&self, event_id: &str) -> Option<PendingHedge> {
        let mut hedges = self.pending_hedges.write().await;
        hedges.remove(event_id)
    }

    /// Get all pending hedges
    pub async fn get_pending_hedges(&self) -> Vec<PendingHedge> {
        let hedges = self.pending_hedges.read().await;
        hedges.values().cloned().collect()
    }

    /// Get completed hedge stats
    pub async fn get_stats(&self) -> DumpHedgeStats {
        let completed = self.completed_hedges.read().await;
        let pending = self.pending_hedges.read().await;

        let total_profit: Decimal = completed.iter().map(|h| h.locked_profit).sum();
        let avg_profit_pct = if completed.is_empty() {
            Decimal::ZERO
        } else {
            completed
                .iter()
                .map(|h| h.locked_profit_pct)
                .sum::<Decimal>()
                / Decimal::from(completed.len())
        };

        DumpHedgeStats {
            completed_hedges: completed.len(),
            pending_hedges: pending.len(),
            total_profit,
            avg_profit_pct,
        }
    }

    /// Get config
    pub fn config(&self) -> &DumpHedgeConfig {
        &self.config
    }
}

/// Statistics for Dump & Hedge strategy
#[derive(Debug, Clone)]
pub struct DumpHedgeStats {
    pub completed_hedges: usize,
    pub pending_hedges: usize,
    pub total_profit: Decimal,
    pub avg_profit_pct: Decimal,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_sum_target() {
        let config = DumpHedgeConfig::default();

        // Normal: 5+ minutes remaining
        assert_eq!(config.dynamic_sum_target(300), dec!(0.95));

        // Urgent: less than 1 minute
        assert_eq!(config.dynamic_sum_target(30), dec!(0.98));

        // Moderate: 2 minutes remaining (should be between)
        let target = config.dynamic_sum_target(120);
        assert!(target > dec!(0.95) && target < dec!(0.98));
    }

    #[test]
    fn test_pending_hedge_remaining() {
        let pending = PendingHedge {
            event_id: "test".to_string(),
            symbol: "BTCUSDT".to_string(),
            leg1_token_id: "token1".to_string(),
            leg1_side: Side::Up,
            leg1_price: dec!(0.35),
            leg1_shares: 100,
            leg1_time: Utc::now(),
            opposite_token_id: "token2".to_string(),
            hedged_shares: 30,
            avg_hedge_price: dec!(0.60),
            time_remaining_at_entry: 300,
        };

        assert_eq!(pending.remaining_shares(), 70);
        assert!(!pending.is_fully_hedged());
    }

    #[test]
    fn test_hedge_profit_calculation() {
        let pending = PendingHedge {
            event_id: "test".to_string(),
            symbol: "BTCUSDT".to_string(),
            leg1_token_id: "token1".to_string(),
            leg1_side: Side::Up,
            leg1_price: dec!(0.35),
            leg1_shares: 100,
            leg1_time: Utc::now(),
            opposite_token_id: "token2".to_string(),
            hedged_shares: 100,
            avg_hedge_price: dec!(0.60),
            time_remaining_at_entry: 300,
        };

        // Fully hedged: profit = 100 * $1 - (100 * 0.35 + 100 * 0.60) = 100 - 95 = $5
        let pnl = pending.calculate_pnl(dec!(0.35));
        assert_eq!(pnl, dec!(5));
    }

    #[test]
    fn test_signal_strength() {
        let tracker = TokenPriceTracker::new(5);

        // High drop + depth collapse + volume spike = max strength
        let strength = tracker.calculate_signal_strength(dec!(0.20), true, true);
        assert_eq!(strength, 1.0);

        // Low drop, no confirmations
        let strength = tracker.calculate_signal_strength(dec!(0.10), false, false);
        assert!(strength < 0.5);
    }
}
