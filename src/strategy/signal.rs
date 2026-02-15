use crate::config::StrategyConfig;
use crate::domain::{DumpSignal, Quote, Side};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::VecDeque;
use tracing::{debug, info};

/// Rolling window for tracking price highs
#[derive(Debug, Clone)]
struct PriceWindow {
    /// (timestamp, price) pairs
    prices: VecDeque<(DateTime<Utc>, Decimal)>,
    /// Window duration
    window_duration: Duration,
}

impl PriceWindow {
    fn new(window_seconds: i64) -> Self {
        Self {
            prices: VecDeque::new(),
            window_duration: Duration::seconds(window_seconds),
        }
    }

    /// Add a price observation
    fn push(&mut self, timestamp: DateTime<Utc>, price: Decimal) {
        // Remove old entries
        let cutoff = timestamp - self.window_duration;
        while let Some((ts, _)) = self.prices.front() {
            if *ts < cutoff {
                self.prices.pop_front();
            } else {
                break;
            }
        }

        // Add new entry
        self.prices.push_back((timestamp, price));
    }

    /// Get the maximum price in the window
    fn max(&self) -> Option<Decimal> {
        self.prices.iter().map(|(_, p)| *p).max()
    }

    /// Get the most recent price
    fn latest(&self) -> Option<Decimal> {
        self.prices.back().map(|(_, p)| *p)
    }

    /// Check if we have enough data
    fn has_data(&self) -> bool {
        !self.prices.is_empty()
    }

    /// Clear all data
    fn clear(&mut self) {
        self.prices.clear();
    }
}

/// Signal detector for identifying dump opportunities
#[derive(Debug)]
pub struct SignalDetector {
    /// Configuration
    config: StrategyConfig,
    /// Rolling window for UP side best_ask
    up_window: PriceWindow,
    /// Rolling window for DOWN side best_ask
    down_window: PriceWindow,
    /// Window size in seconds (for 3-second rolling high)
    window_seconds: i64,
    /// Whether we've triggered in the current round
    triggered_up: bool,
    triggered_down: bool,
    /// Current round slug (for reset detection)
    current_round: Option<String>,
}

impl SignalDetector {
    /// Create a new signal detector with default 3-second window
    pub fn new(config: StrategyConfig) -> Self {
        Self::with_window(config, 3)
    }

    /// Create a new signal detector with custom window size
    pub fn with_window(config: StrategyConfig, window_seconds: i64) -> Self {
        Self {
            config,
            up_window: PriceWindow::new(window_seconds),
            down_window: PriceWindow::new(window_seconds),
            window_seconds,
            triggered_up: false,
            triggered_down: false,
            current_round: None,
        }
    }

    /// Reset for a new round
    pub fn reset(&mut self, round_slug: Option<&str>) {
        self.up_window.clear();
        self.down_window.clear();
        self.triggered_up = false;
        self.triggered_down = false;
        self.current_round = round_slug.map(|s| s.to_string());
        debug!("Signal detector reset for round: {:?}", round_slug);
    }

    /// Update with new quote data and check for signals
    pub fn update(&mut self, quote: &Quote, round_slug: Option<&str>) -> Option<DumpSignal> {
        // Check if we've moved to a new round
        if round_slug != self.current_round.as_deref() {
            self.reset(round_slug);
        }

        // Only process if we have a valid best_ask
        let Some(best_ask) = quote.best_ask else {
            return None;
        };

        let now = quote.timestamp;
        let side = quote.side;

        // Check if already triggered for this side
        let already_triggered = match side {
            Side::Up => self.triggered_up,
            Side::Down => self.triggered_down,
        };

        if already_triggered {
            // Still update the window for tracking
            match side {
                Side::Up => self.up_window.push(now, best_ask),
                Side::Down => self.down_window.push(now, best_ask),
            };
            return None;
        }

        // Update the appropriate window
        match side {
            Side::Up => self.up_window.push(now, best_ask),
            Side::Down => self.down_window.push(now, best_ask),
        };

        // Get rolling high from window
        let rolling_high = match side {
            Side::Up => self.up_window.max(),
            Side::Down => self.down_window.max(),
        };

        // Check for dump signal
        if let Some(signal) = self.check_dump_inner(side, rolling_high, quote) {
            // Mark as triggered
            match side {
                Side::Up => self.triggered_up = true,
                Side::Down => self.triggered_down = true,
            };
            info!(
                "Dump signal detected: {:?} dropped {:.2}% from {:.4} to {:.4}",
                signal.side,
                signal.drop_pct * Decimal::from(100),
                signal.reference_price,
                signal.trigger_price
            );
            return Some(signal);
        }

        None
    }

    /// Check if a dump has occurred
    fn check_dump_inner(
        &self,
        side: Side,
        rolling_high: Option<Decimal>,
        quote: &Quote,
    ) -> Option<DumpSignal> {
        let Some(rolling_high) = rolling_high else {
            return None;
        };

        let Some(current_ask) = quote.best_ask else {
            return None;
        };

        // Calculate drop percentage
        // Dump = current_ask / rolling_high <= (1 - move_pct)
        // Or: drop_pct = 1 - (current_ask / rolling_high) >= move_pct
        if rolling_high <= Decimal::ZERO {
            return None;
        }

        let ratio = current_ask / rolling_high;
        let drop_pct = Decimal::ONE - ratio;

        // Check if drop exceeds threshold
        if drop_pct >= self.config.move_pct {
            // Calculate spread for anti-fake-dump filter
            let spread_bps = quote.spread_bps().unwrap_or(9999);

            Some(DumpSignal {
                side,
                trigger_price: current_ask,
                reference_price: rolling_high,
                drop_pct,
                timestamp: quote.timestamp,
                spread_bps,
            })
        } else {
            None
        }
    }

    /// Check if we can still trigger on a specific side
    pub fn can_trigger(&self, side: Side) -> bool {
        match side {
            Side::Up => !self.triggered_up,
            Side::Down => !self.triggered_down,
        }
    }

    /// Mark a side as triggered (called after successful Leg1)
    pub fn mark_triggered(&mut self, side: Side) {
        match side {
            Side::Up => self.triggered_up = true,
            Side::Down => self.triggered_down = true,
        }
    }

    /// Get the effective sum target for Leg2 calculation
    pub fn effective_sum_target(&self) -> Decimal {
        self.config.effective_sum_target()
    }

    /// Check if Leg2 condition is met
    pub fn check_leg2_condition(&self, leg1_price: Decimal, opposite_ask: Decimal) -> bool {
        let sum = leg1_price + opposite_ask;
        let target = self.effective_sum_target();

        debug!(
            "Leg2 check: leg1={:.4} + opposite_ask={:.4} = {:.4} <= target={:.4} ? {}",
            leg1_price,
            opposite_ask,
            sum,
            target,
            sum <= target
        );

        sum <= target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> StrategyConfig {
        StrategyConfig {
            shares: 20,
            window_min: 2,
            move_pct: dec!(0.15),
            sum_target: dec!(0.95),
            fee_buffer: dec!(0.005),
            slippage_buffer: dec!(0.02),
            profit_buffer: dec!(0.01),
        }
    }

    #[test]
    fn test_dump_detection() {
        let config = test_config();
        let mut detector = SignalDetector::new(config);
        detector.reset(Some("test-round"));

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
        assert!(detector.update(&quote1, Some("test-round")).is_none());

        // Price drops to 0.42 (16% drop) - should trigger
        let quote2 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.41)),
            best_ask: Some(dec!(0.42)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(1),
        };
        let signal = detector.update(&quote2, Some("test-round"));
        assert!(signal.is_some());

        let sig = signal.unwrap();
        assert_eq!(sig.side, Side::Up);
        assert_eq!(sig.trigger_price, dec!(0.42));
        assert_eq!(sig.reference_price, dec!(0.50));
        assert!(sig.drop_pct >= dec!(0.15));
    }

    #[test]
    fn test_no_duplicate_trigger() {
        let config = test_config();
        let mut detector = SignalDetector::new(config);
        detector.reset(Some("test-round"));

        let now = Utc::now();

        // Trigger once
        let quote1 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.49)),
            best_ask: Some(dec!(0.50)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now,
        };
        detector.update(&quote1, Some("test-round"));

        let quote2 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.41)),
            best_ask: Some(dec!(0.42)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(1),
        };
        assert!(detector.update(&quote2, Some("test-round")).is_some());

        // Second trigger on same side should not work
        let quote3 = Quote {
            side: Side::Up,
            best_bid: Some(dec!(0.35)),
            best_ask: Some(dec!(0.36)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp: now + Duration::seconds(2),
        };
        assert!(detector.update(&quote3, Some("test-round")).is_none());
    }

    #[test]
    fn test_leg2_condition() {
        let config = test_config();
        let detector = SignalDetector::new(config);

        // effective_target = 0.95 - 0.005 - 0.02 - 0.01 = 0.915
        // leg1 = 0.45, opposite = 0.46, sum = 0.91 <= 0.915 -> true
        assert!(detector.check_leg2_condition(dec!(0.45), dec!(0.46)));

        // leg1 = 0.45, opposite = 0.47, sum = 0.92 > 0.915 -> false
        assert!(!detector.check_leg2_condition(dec!(0.45), dec!(0.47)));
    }
}
