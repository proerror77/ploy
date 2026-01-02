//! Dump signal detector for two-leg arbitrage
//!
//! Detects sudden price drops that may indicate arbitrage opportunities.

use crate::domain::{Quote, Side};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, info};

/// Configuration for dump detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpDetectorConfig {
    /// Rolling window size in seconds
    pub window_seconds: i64,
    /// Minimum price drop percentage to trigger (e.g., 0.15 = 15%)
    pub move_pct: Decimal,
    /// Maximum spread in bps to accept signal
    pub max_spread_bps: u32,
    /// Sum target for leg2 condition
    pub sum_target: Decimal,
    /// Fee buffer for effective sum target
    pub fee_buffer: Decimal,
    /// Slippage buffer for effective sum target
    pub slippage_buffer: Decimal,
    /// Profit buffer for effective sum target
    pub profit_buffer: Decimal,
}

impl Default for DumpDetectorConfig {
    fn default() -> Self {
        Self {
            window_seconds: 3,
            move_pct: Decimal::new(15, 2), // 0.15 = 15%
            max_spread_bps: 500,
            sum_target: Decimal::new(95, 2), // 0.95
            fee_buffer: Decimal::new(5, 3), // 0.005
            slippage_buffer: Decimal::new(2, 2), // 0.02
            profit_buffer: Decimal::new(1, 2), // 0.01
        }
    }
}

impl DumpDetectorConfig {
    /// Calculate effective sum target after buffers
    pub fn effective_sum_target(&self) -> Decimal {
        Decimal::ONE - self.fee_buffer - self.slippage_buffer - self.profit_buffer
    }
}

/// A detected dump signal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpSignal {
    /// Which side dumped
    pub side: Side,
    /// Price at which dump was detected
    pub trigger_price: Decimal,
    /// Reference price (rolling high)
    pub reference_price: Decimal,
    /// Drop percentage
    pub drop_pct: Decimal,
    /// When the dump was detected
    pub timestamp: DateTime<Utc>,
    /// Spread at time of signal
    pub spread_bps: u32,
    /// Event/round identifier
    pub event_id: Option<String>,
}

impl DumpSignal {
    /// Check if spread is acceptable
    pub fn spread_ok(&self, max_bps: u32) -> bool {
        self.spread_bps <= max_bps
    }
}

/// Rolling window for tracking price highs
#[derive(Debug, Clone)]
struct PriceWindow {
    prices: VecDeque<(DateTime<Utc>, Decimal)>,
    window_duration: Duration,
}

impl PriceWindow {
    fn new(window_seconds: i64) -> Self {
        Self {
            prices: VecDeque::new(),
            window_duration: Duration::seconds(window_seconds),
        }
    }

    fn push(&mut self, timestamp: DateTime<Utc>, price: Decimal) {
        let cutoff = timestamp - self.window_duration;
        while let Some((ts, _)) = self.prices.front() {
            if *ts < cutoff {
                self.prices.pop_front();
            } else {
                break;
            }
        }
        self.prices.push_back((timestamp, price));
    }

    fn max(&self) -> Option<Decimal> {
        self.prices.iter().map(|(_, p)| *p).max()
    }

    fn clear(&mut self) {
        self.prices.clear();
    }
}

/// Dump signal detector
#[derive(Debug)]
pub struct DumpDetector {
    config: DumpDetectorConfig,
    up_window: PriceWindow,
    down_window: PriceWindow,
    triggered_up: bool,
    triggered_down: bool,
    current_event: Option<String>,
}

impl DumpDetector {
    /// Create a new dump detector
    pub fn new(config: DumpDetectorConfig) -> Self {
        let window_seconds = config.window_seconds;
        Self {
            config,
            up_window: PriceWindow::new(window_seconds),
            down_window: PriceWindow::new(window_seconds),
            triggered_up: false,
            triggered_down: false,
            current_event: None,
        }
    }

    /// Reset for a new event/round
    pub fn reset(&mut self, event_id: Option<&str>) {
        self.up_window.clear();
        self.down_window.clear();
        self.triggered_up = false;
        self.triggered_down = false;
        self.current_event = event_id.map(|s| s.to_string());
        debug!("Dump detector reset for event: {:?}", event_id);
    }

    /// Update with new quote and check for dump signal
    pub fn update(&mut self, quote: &Quote, event_id: Option<&str>) -> Option<DumpSignal> {
        // Check for event change
        if event_id != self.current_event.as_deref() {
            self.reset(event_id);
        }

        let Some(best_ask) = quote.best_ask else {
            return None;
        };

        let now = quote.timestamp;
        let side = quote.side;

        // Check if already triggered
        let already_triggered = match side {
            Side::Up => self.triggered_up,
            Side::Down => self.triggered_down,
        };

        // Update window
        match side {
            Side::Up => self.up_window.push(now, best_ask),
            Side::Down => self.down_window.push(now, best_ask),
        };

        if already_triggered {
            return None;
        }

        // Get rolling high
        let rolling_high = match side {
            Side::Up => self.up_window.max(),
            Side::Down => self.down_window.max(),
        };

        // Check for dump
        if let Some(signal) = self.check_dump(side, rolling_high, quote) {
            match side {
                Side::Up => self.triggered_up = true,
                Side::Down => self.triggered_down = true,
            };

            info!(
                "Dump signal: {:?} dropped {:.2}% from {:.4} to {:.4}",
                signal.side,
                signal.drop_pct * Decimal::from(100),
                signal.reference_price,
                signal.trigger_price
            );

            return Some(signal);
        }

        None
    }

    fn check_dump(
        &self,
        side: Side,
        rolling_high: Option<Decimal>,
        quote: &Quote,
    ) -> Option<DumpSignal> {
        let rolling_high = rolling_high?;
        let current_ask = quote.best_ask?;

        if rolling_high <= Decimal::ZERO {
            return None;
        }

        let ratio = current_ask / rolling_high;
        let drop_pct = Decimal::ONE - ratio;

        if drop_pct >= self.config.move_pct {
            let spread_bps = quote.spread_bps().unwrap_or(9999);

            Some(DumpSignal {
                side,
                trigger_price: current_ask,
                reference_price: rolling_high,
                drop_pct,
                timestamp: quote.timestamp,
                spread_bps,
                event_id: self.current_event.clone(),
            })
        } else {
            None
        }
    }

    /// Check if side can still trigger
    pub fn can_trigger(&self, side: Side) -> bool {
        match side {
            Side::Up => !self.triggered_up,
            Side::Down => !self.triggered_down,
        }
    }

    /// Mark side as triggered
    pub fn mark_triggered(&mut self, side: Side) {
        match side {
            Side::Up => self.triggered_up = true,
            Side::Down => self.triggered_down = true,
        }
    }

    /// Get effective sum target for leg2
    pub fn effective_sum_target(&self) -> Decimal {
        self.config.effective_sum_target()
    }

    /// Check leg2 condition
    pub fn check_leg2_condition(&self, leg1_price: Decimal, opposite_ask: Decimal) -> bool {
        let sum = leg1_price + opposite_ask;
        let target = self.effective_sum_target();

        debug!(
            "Leg2 check: {} + {} = {} <= {} ? {}",
            leg1_price, opposite_ask, sum, target, sum <= target
        );

        sum <= target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> DumpDetectorConfig {
        DumpDetectorConfig {
            window_seconds: 3,
            move_pct: dec!(0.15),
            max_spread_bps: 500,
            sum_target: dec!(0.95),
            fee_buffer: dec!(0.005),
            slippage_buffer: dec!(0.02),
            profit_buffer: dec!(0.01),
        }
    }

    #[test]
    fn test_dump_detection() {
        let mut detector = DumpDetector::new(test_config());
        detector.reset(Some("test"));

        let now = Utc::now();

        // First quote at 0.50
        let quote1 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.49)),
            best_ask: Some(dec!(0.50)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now,
        };
        assert!(detector.update(&quote1, Some("test")).is_none());

        // Drop to 0.42 (16%)
        let quote2 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.41)),
            best_ask: Some(dec!(0.42)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(1),
        };
        let signal = detector.update(&quote2, Some("test"));
        assert!(signal.is_some());

        let sig = signal.unwrap();
        assert_eq!(sig.side, Side::Up);
        assert!(sig.drop_pct >= dec!(0.15));
    }

    #[test]
    fn test_no_duplicate() {
        let mut detector = DumpDetector::new(test_config());
        detector.reset(Some("test"));

        let now = Utc::now();

        let quote1 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.49)),
            best_ask: Some(dec!(0.50)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now,
        };
        detector.update(&quote1, Some("test"));

        let quote2 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.41)),
            best_ask: Some(dec!(0.42)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(1),
        };
        assert!(detector.update(&quote2, Some("test")).is_some());

        // Second trigger should fail
        let quote3 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.35)),
            best_ask: Some(dec!(0.36)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(2),
        };
        assert!(detector.update(&quote3, Some("test")).is_none());
    }

    #[test]
    fn test_leg2_condition() {
        let detector = DumpDetector::new(test_config());

        // effective = 1 - 0.005 - 0.02 - 0.01 = 0.965
        assert!(detector.check_leg2_condition(dec!(0.45), dec!(0.50))); // 0.95 <= 0.965
        assert!(!detector.check_leg2_condition(dec!(0.45), dec!(0.55))); // 1.00 > 0.965
    }
}
