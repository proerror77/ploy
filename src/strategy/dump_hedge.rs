//! Dump & Hedge Strategy for Polymarket
//!
//! Exploits temporary mispricing when someone market dumps on PM.
//! Two-leg arbitrage:
//! - Leg 1: Buy when token price drops fast (e.g., 15% in ~3s)
//! - Leg 2: Hedge by buying opposite when leg1_price + opposite_ask <= sumTarget

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::domain::Side;

/// Configuration for Dump & Hedge strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpHedgeConfig {
    /// Target sum for full hedge (e.g., 0.95 means 5% profit locked)
    pub sum_target: Decimal,
    /// Minimum price drop to trigger Leg 1 (e.g., 0.15 = 15%)
    pub move_pct: Decimal,
    /// Detection window in seconds
    pub window_secs: u64,
    /// Shares per trade
    pub shares: u64,
    /// Maximum entry price for Leg 1
    pub max_leg1_price: Decimal,
    /// Minimum time remaining before event end (seconds)
    pub min_time_remaining_secs: u64,
}

impl Default for DumpHedgeConfig {
    fn default() -> Self {
        Self {
            sum_target: dec!(0.95),          // Target 5% locked profit
            move_pct: dec!(0.15),             // 15% drop triggers
            window_secs: 5,                   // Detect within 5 seconds
            shares: 20,                       // 20 shares per leg
            max_leg1_price: dec!(0.40),       // Max 40¢ for Leg 1
            min_time_remaining_secs: 120,     // At least 2 min for hedge
        }
    }
}

/// Price snapshot for dump detection
#[derive(Debug, Clone)]
pub struct PriceSnapshot {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Tracks PM token price movements for dump detection
#[derive(Debug, Clone)]
pub struct TokenPriceTracker {
    /// Recent prices for dump detection
    recent_prices: HashMap<String, Vec<PriceSnapshot>>,
    /// Window size for detection
    window_secs: i64,
}

impl TokenPriceTracker {
    pub fn new(window_secs: u64) -> Self {
        Self {
            recent_prices: HashMap::new(),
            window_secs: window_secs as i64,
        }
    }

    /// Record a new price update
    pub fn update(&mut self, token_id: &str, price: Decimal) {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(self.window_secs * 2);

        let snapshots = self.recent_prices.entry(token_id.to_string()).or_default();

        // Add new snapshot
        snapshots.push(PriceSnapshot {
            price,
            timestamp: now,
        });

        // Prune old snapshots
        snapshots.retain(|s| s.timestamp > cutoff);
    }

    /// Check if price dropped by at least `move_pct` within the window
    pub fn detect_dump(&self, token_id: &str, move_pct: Decimal) -> Option<DumpSignal> {
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

        // Get current price (last snapshot)
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

        if drop_pct >= move_pct {
            let elapsed_ms = (current.timestamp - max_time).num_milliseconds();
            info!(
                "🔴 DUMP detected: {} dropped {:.1}% in {}ms (from {:.1}¢ to {:.1}¢)",
                &token_id[..20.min(token_id.len())],
                drop_pct * dec!(100),
                elapsed_ms,
                max_price * dec!(100),
                current.price * dec!(100)
            );

            return Some(DumpSignal {
                token_id: token_id.to_string(),
                drop_pct,
                from_price: max_price,
                to_price: current.price,
                elapsed_ms: elapsed_ms as u64,
                timestamp: current.timestamp,
            });
        }

        None
    }

    /// Get current price for a token
    pub fn current_price(&self, token_id: &str) -> Option<Decimal> {
        self.recent_prices.get(token_id)?.last().map(|s| s.price)
    }
}

/// Signal that a dump was detected
#[derive(Debug, Clone)]
pub struct DumpSignal {
    pub token_id: String,
    pub drop_pct: Decimal,
    pub from_price: Decimal,
    pub to_price: Decimal,
    pub elapsed_ms: u64,
    pub timestamp: DateTime<Utc>,
}

/// Active Leg 1 position waiting for hedge
#[derive(Debug, Clone)]
pub struct PendingHedge {
    pub event_id: String,
    pub leg1_token_id: String,
    pub leg1_side: Side,
    pub leg1_price: Decimal,
    pub leg1_shares: u64,
    pub leg1_time: DateTime<Utc>,
    pub opposite_token_id: String,
}

/// Complete hedge opportunity
#[derive(Debug, Clone)]
pub struct HedgeSignal {
    pub event_id: String,
    pub pending: PendingHedge,
    pub leg2_ask: Decimal,
    pub sum: Decimal,
    pub locked_profit_pct: Decimal,
}

/// Dump & Hedge Strategy Engine
pub struct DumpHedgeEngine {
    config: DumpHedgeConfig,
    price_tracker: Arc<RwLock<TokenPriceTracker>>,
    pending_hedges: Arc<RwLock<HashMap<String, PendingHedge>>>,
}

impl DumpHedgeEngine {
    pub fn new(config: DumpHedgeConfig) -> Self {
        let window_secs = config.window_secs;
        Self {
            config,
            price_tracker: Arc::new(RwLock::new(TokenPriceTracker::new(window_secs))),
            pending_hedges: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update price tracker with new PM price data
    pub async fn on_price_update(&self, token_id: &str, price: Decimal) {
        let mut tracker = self.price_tracker.write().await;
        tracker.update(token_id, price);
    }

    /// Check for dump and potential Leg 1 entry
    pub async fn check_leg1_signal(
        &self,
        event_id: &str,
        up_token_id: &str,
        down_token_id: &str,
        up_ask: Decimal,
        down_ask: Decimal,
        time_remaining_secs: i64,
    ) -> Option<DumpSignal> {
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
        if let Some(signal) = tracker.detect_dump(up_token_id, self.config.move_pct) {
            if up_ask <= self.config.max_leg1_price {
                return Some(signal);
            }
        }

        // Check DOWN token for dump
        if let Some(signal) = tracker.detect_dump(down_token_id, self.config.move_pct) {
            if down_ask <= self.config.max_leg1_price {
                return Some(signal);
            }
        }

        None
    }

    /// Record a Leg 1 entry
    pub async fn record_leg1(
        &self,
        event_id: &str,
        leg1_token_id: &str,
        leg1_side: Side,
        leg1_price: Decimal,
        leg1_shares: u64,
        opposite_token_id: &str,
    ) {
        let pending = PendingHedge {
            event_id: event_id.to_string(),
            leg1_token_id: leg1_token_id.to_string(),
            leg1_side,
            leg1_price,
            leg1_shares,
            leg1_time: Utc::now(),
            opposite_token_id: opposite_token_id.to_string(),
        };

        let mut hedges = self.pending_hedges.write().await;
        hedges.insert(event_id.to_string(), pending);

        info!(
            "📝 Leg 1 recorded: {} {:?} @ {:.1}¢ x{} shares",
            &leg1_token_id[..20.min(leg1_token_id.len())],
            leg1_side,
            leg1_price * dec!(100),
            leg1_shares
        );
    }

    /// Check if hedge opportunity exists for pending Leg 1
    pub async fn check_hedge_signal(&self, event_id: &str, opposite_ask: Decimal) -> Option<HedgeSignal> {
        let pending = {
            let hedges = self.pending_hedges.read().await;
            hedges.get(event_id)?.clone()
        };

        let sum = pending.leg1_price + opposite_ask;

        if sum <= self.config.sum_target {
            let locked_profit_pct = (dec!(1) - sum) / sum * dec!(100);

            info!(
                "✅ HEDGE READY: {} | Leg1={:.1}¢ + Leg2={:.1}¢ = {:.2} <= {:.2} | Profit={:.1}%",
                event_id,
                pending.leg1_price * dec!(100),
                opposite_ask * dec!(100),
                sum,
                self.config.sum_target,
                locked_profit_pct
            );

            return Some(HedgeSignal {
                event_id: event_id.to_string(),
                pending,
                leg2_ask: opposite_ask,
                sum,
                locked_profit_pct,
            });
        }

        debug!(
            "Hedge not ready: {} | {:.1}¢ + {:.1}¢ = {:.2} > {:.2}",
            event_id,
            pending.leg1_price * dec!(100),
            opposite_ask * dec!(100),
            sum,
            self.config.sum_target
        );

        None
    }

    /// Complete hedge and remove from pending
    pub async fn complete_hedge(&self, event_id: &str) {
        let mut hedges = self.pending_hedges.write().await;
        if hedges.remove(event_id).is_some() {
            info!("🎉 Hedge completed for {}", event_id);
        }
    }

    /// Get pending hedges that are timing out (event ending soon)
    pub async fn get_expiring_hedges(&self, max_time_remaining_secs: i64) -> Vec<PendingHedge> {
        let hedges = self.pending_hedges.read().await;
        let now = Utc::now();

        hedges
            .values()
            .filter(|h| {
                let elapsed = (now - h.leg1_time).num_seconds();
                elapsed > max_time_remaining_secs
            })
            .cloned()
            .collect()
    }

    /// Cancel a pending hedge (e.g., if event ending without hedge)
    pub async fn cancel_hedge(&self, event_id: &str) {
        let mut hedges = self.pending_hedges.write().await;
        if let Some(pending) = hedges.remove(event_id) {
            warn!(
                "⚠️ Hedge cancelled for {} - Leg 1 was {:?} @ {:.1}¢",
                event_id,
                pending.leg1_side,
                pending.leg1_price * dec!(100)
            );
        }
    }

    /// Get config
    pub fn config(&self) -> &DumpHedgeConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dump_detection() {
        let mut tracker = TokenPriceTracker::new(5);
        let token = "test-token-123";

        // Simulate price drop from 50¢ to 35¢ (30% drop)
        tracker.update(token, dec!(0.50));
        // Advance time simulation by using different price
        tracker.update(token, dec!(0.45));
        tracker.update(token, dec!(0.40));
        tracker.update(token, dec!(0.35));

        // Should detect dump with 15% threshold
        let signal = tracker.detect_dump(token, dec!(0.15));
        assert!(signal.is_some());

        if let Some(s) = signal {
            assert!(s.drop_pct >= dec!(0.15));
            assert_eq!(s.to_price, dec!(0.35));
        }
    }

    #[test]
    fn test_hedge_sum_calculation() {
        // Leg 1: 35¢
        // Leg 2: 60¢
        // Sum: 95¢ <= target 95¢ ✓
        // Profit: (1 - 0.95) / 0.95 = 5.26%

        let leg1 = dec!(0.35);
        let leg2 = dec!(0.60);
        let sum = leg1 + leg2;
        let target = dec!(0.95);

        assert!(sum <= target);

        let profit_pct = (dec!(1) - sum) / sum * dec!(100);
        assert!(profit_pct > dec!(5)); // > 5% profit
    }
}
