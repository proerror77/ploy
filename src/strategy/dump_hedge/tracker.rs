use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::info;

use super::DumpHedgeConfig;

/// Enhanced price snapshot with market depth info.
#[derive(Debug, Clone)]
pub struct EnhancedSnapshot {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    pub bid_depth: Option<Decimal>,
    pub ask_depth: Option<Decimal>,
    pub volume_spike: bool,
}

/// Tracks PM token price movements with enhanced signals.
#[derive(Debug, Clone)]
pub struct TokenPriceTracker {
    recent_prices: HashMap<String, Vec<EnhancedSnapshot>>,
    baseline_depth: HashMap<String, Decimal>,
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

    /// Record a new price update with optional depth info.
    pub fn update(
        &mut self,
        token_id: &str,
        price: Decimal,
        bid_depth: Option<Decimal>,
        ask_depth: Option<Decimal>,
    ) {
        let now = Utc::now();
        let cutoff = now - Duration::seconds(self.window_secs * 3);

        let volume_spike = match self.recent_prices.get(token_id) {
            Some(snapshots) => self.detect_volume_spike(snapshots, bid_depth),
            None => false,
        };

        let snapshots = self.recent_prices.entry(token_id.to_string()).or_default();

        if let Some(depth) = bid_depth {
            let baseline = self
                .baseline_depth
                .entry(token_id.to_string())
                .or_insert(depth);
            *baseline = *baseline * dec!(0.95) + depth * dec!(0.05);
        }

        snapshots.push(EnhancedSnapshot {
            price,
            timestamp: now,
            bid_depth,
            ask_depth,
            volume_spike,
        });
        snapshots.retain(|snapshot| snapshot.timestamp > cutoff);
    }

    /// Check if price dropped by at least `move_pct` within the window.
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

        let mut max_price = Decimal::ZERO;
        let mut max_time = now;

        for snapshot in snapshots {
            if snapshot.timestamp >= window_start && snapshot.price > max_price {
                max_price = snapshot.price;
                max_time = snapshot.timestamp;
            }
        }

        let current = snapshots.last()?;
        if max_time >= current.timestamp || max_price.is_zero() {
            return None;
        }

        let drop_pct = (max_price - current.price) / max_price;
        let has_depth_collapse = self.check_depth_collapse(token_id);
        let has_volume_spike = current.volume_spike;
        let required_drop = if has_depth_collapse || has_volume_spike {
            config.enhanced_move_pct
        } else {
            config.move_pct
        };

        if drop_pct < required_drop {
            return None;
        }

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

        Some(EnhancedDumpSignal {
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
        })
    }

    /// Get current price for a token.
    pub fn current_price(&self, token_id: &str) -> Option<Decimal> {
        self.recent_prices
            .get(token_id)?
            .last()
            .map(|snapshot| snapshot.price)
    }

    fn detect_volume_spike(
        &self,
        snapshots: &[EnhancedSnapshot],
        current_depth: Option<Decimal>,
    ) -> bool {
        if snapshots.len() < 5 {
            return false;
        }

        let current = match current_depth {
            Some(depth) => depth,
            None => return false,
        };

        let depths: Vec<Decimal> = snapshots
            .iter()
            .filter_map(|snapshot| snapshot.bid_depth)
            .collect();
        if depths.is_empty() {
            return false;
        }

        let avg_depth: Decimal = depths.iter().sum::<Decimal>() / Decimal::from(depths.len());
        current < avg_depth * dec!(0.5)
    }

    fn check_depth_collapse(&self, token_id: &str) -> bool {
        let snapshots = match self.recent_prices.get(token_id) {
            Some(snapshots) => snapshots,
            None => return false,
        };
        let baseline = match self.baseline_depth.get(token_id) {
            Some(baseline) => *baseline,
            None => return false,
        };
        let current = match snapshots.last().and_then(|snapshot| snapshot.bid_depth) {
            Some(depth) => depth,
            None => return false,
        };

        current < baseline * dec!(0.4)
    }

    pub(crate) fn calculate_signal_strength(
        &self,
        drop_pct: Decimal,
        depth_collapse: bool,
        volume_spike: bool,
    ) -> f64 {
        let mut strength = 0.0;

        let drop_f64 = drop_pct.to_string().parse::<f64>().unwrap_or(0.0);
        strength += (drop_f64 * 2.5).min(0.5);

        if depth_collapse {
            strength += 0.25;
        }
        if volume_spike {
            strength += 0.25;
        }

        strength.min(1.0)
    }
}

/// Enhanced signal that a dump was detected.
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
