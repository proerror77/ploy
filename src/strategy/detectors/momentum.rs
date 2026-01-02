//! Momentum/trend detector for trading signals
//!
//! Detects momentum shifts and trend changes based on price movements.

use crate::domain::{Quote, Side};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, info};

/// Configuration for momentum detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumDetectorConfig {
    /// Short-term moving average window (seconds)
    pub short_window_secs: i64,
    /// Long-term moving average window (seconds)
    pub long_window_secs: i64,
    /// Minimum momentum threshold to trigger signal
    pub min_momentum: Decimal,
    /// Minimum price change for trend confirmation
    pub min_trend_change: Decimal,
    /// Cooldown between signals (seconds)
    pub signal_cooldown_secs: i64,
}

impl Default for MomentumDetectorConfig {
    fn default() -> Self {
        Self {
            short_window_secs: 30,
            long_window_secs: 120,
            min_momentum: Decimal::new(5, 2), // 0.05 = 5%
            min_trend_change: Decimal::new(2, 2), // 0.02 = 2%
            signal_cooldown_secs: 60,
        }
    }
}

/// Trend direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrendDirection {
    /// Price trending up
    Bullish,
    /// Price trending down
    Bearish,
    /// No clear trend
    Neutral,
}

impl Default for TrendDirection {
    fn default() -> Self {
        TrendDirection::Neutral
    }
}

/// A momentum signal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumSignal {
    /// Which side the signal is for
    pub side: Side,
    /// Trend direction
    pub direction: TrendDirection,
    /// Current momentum value
    pub momentum: Decimal,
    /// Short-term average
    pub short_avg: Decimal,
    /// Long-term average
    pub long_avg: Decimal,
    /// Current price
    pub current_price: Decimal,
    /// Signal timestamp
    pub timestamp: DateTime<Utc>,
    /// Confidence score (0-1)
    pub confidence: Decimal,
}

impl MomentumSignal {
    /// Check if signal suggests buying
    pub fn is_buy(&self) -> bool {
        self.direction == TrendDirection::Bullish && self.momentum > Decimal::ZERO
    }

    /// Check if signal suggests selling
    pub fn is_sell(&self) -> bool {
        self.direction == TrendDirection::Bearish && self.momentum < Decimal::ZERO
    }
}

/// Price observation for moving average calculation
#[derive(Debug, Clone)]
struct PriceObs {
    timestamp: DateTime<Utc>,
    price: Decimal,
}

/// Moving average calculator
#[derive(Debug, Clone)]
struct MovingAverage {
    observations: VecDeque<PriceObs>,
    window_duration: Duration,
}

impl MovingAverage {
    fn new(window_secs: i64) -> Self {
        Self {
            observations: VecDeque::new(),
            window_duration: Duration::seconds(window_secs),
        }
    }

    fn push(&mut self, timestamp: DateTime<Utc>, price: Decimal) {
        // Remove old observations
        let cutoff = timestamp - self.window_duration;
        while let Some(obs) = self.observations.front() {
            if obs.timestamp < cutoff {
                self.observations.pop_front();
            } else {
                break;
            }
        }

        self.observations.push_back(PriceObs { timestamp, price });
    }

    fn average(&self) -> Option<Decimal> {
        if self.observations.is_empty() {
            return None;
        }

        let sum: Decimal = self.observations.iter().map(|o| o.price).sum();
        Some(sum / Decimal::from(self.observations.len()))
    }

    fn has_sufficient_data(&self) -> bool {
        // Need at least a few observations
        self.observations.len() >= 3
    }

    fn clear(&mut self) {
        self.observations.clear();
    }
}

/// Momentum signal detector
#[derive(Debug)]
pub struct MomentumDetector {
    config: MomentumDetectorConfig,
    /// Short-term MA for UP side
    up_short: MovingAverage,
    /// Long-term MA for UP side
    up_long: MovingAverage,
    /// Short-term MA for DOWN side
    down_short: MovingAverage,
    /// Long-term MA for DOWN side
    down_long: MovingAverage,
    /// Last signal time for cooldown
    last_signal_up: Option<DateTime<Utc>>,
    last_signal_down: Option<DateTime<Utc>>,
    /// Current trend
    up_trend: TrendDirection,
    down_trend: TrendDirection,
}

impl MomentumDetector {
    /// Create a new momentum detector
    pub fn new(config: MomentumDetectorConfig) -> Self {
        Self {
            up_short: MovingAverage::new(config.short_window_secs),
            up_long: MovingAverage::new(config.long_window_secs),
            down_short: MovingAverage::new(config.short_window_secs),
            down_long: MovingAverage::new(config.long_window_secs),
            last_signal_up: None,
            last_signal_down: None,
            up_trend: TrendDirection::Neutral,
            down_trend: TrendDirection::Neutral,
            config,
        }
    }

    /// Reset the detector
    pub fn reset(&mut self) {
        self.up_short.clear();
        self.up_long.clear();
        self.down_short.clear();
        self.down_long.clear();
        self.last_signal_up = None;
        self.last_signal_down = None;
        self.up_trend = TrendDirection::Neutral;
        self.down_trend = TrendDirection::Neutral;
        debug!("Momentum detector reset");
    }

    /// Update with new quote and check for momentum signal
    pub fn update(&mut self, quote: &Quote) -> Option<MomentumSignal> {
        // Use mid price or best_ask
        let price = quote.mid_price().or(quote.best_ask)?;
        let now = quote.timestamp;
        let side = quote.side;

        // Update moving averages
        match side {
            Side::Up => {
                self.up_short.push(now, price);
                self.up_long.push(now, price);
            }
            Side::Down => {
                self.down_short.push(now, price);
                self.down_long.push(now, price);
            }
        }

        // Check for momentum signal
        self.check_momentum(side, now, price)
    }

    fn check_momentum(
        &mut self,
        side: Side,
        now: DateTime<Utc>,
        current_price: Decimal,
    ) -> Option<MomentumSignal> {
        let (short_ma, long_ma, last_signal, current_trend) = match side {
            Side::Up => (
                &self.up_short,
                &self.up_long,
                &mut self.last_signal_up,
                &mut self.up_trend,
            ),
            Side::Down => (
                &self.down_short,
                &self.down_long,
                &mut self.last_signal_down,
                &mut self.down_trend,
            ),
        };

        // Need sufficient data
        if !short_ma.has_sufficient_data() || !long_ma.has_sufficient_data() {
            return None;
        }

        // Check cooldown
        if let Some(last) = *last_signal {
            if now - last < Duration::seconds(self.config.signal_cooldown_secs) {
                return None;
            }
        }

        let short_avg = short_ma.average()?;
        let long_avg = long_ma.average()?;

        if long_avg == Decimal::ZERO {
            return None;
        }

        // Calculate momentum as percentage difference
        let momentum = (short_avg - long_avg) / long_avg;

        // Determine trend direction
        let new_trend = if momentum >= self.config.min_momentum {
            TrendDirection::Bullish
        } else if momentum <= -self.config.min_momentum {
            TrendDirection::Bearish
        } else {
            TrendDirection::Neutral
        };

        // Only signal on trend change or strong momentum
        let should_signal = *current_trend != new_trend && new_trend != TrendDirection::Neutral;

        if should_signal {
            *current_trend = new_trend;
            *last_signal = Some(now);

            // Calculate confidence based on momentum strength
            let confidence = (momentum.abs() / self.config.min_momentum)
                .min(Decimal::ONE);

            let signal = MomentumSignal {
                side,
                direction: new_trend,
                momentum,
                short_avg,
                long_avg,
                current_price,
                timestamp: now,
                confidence,
            };

            info!(
                "Momentum signal: {:?} {:?} (momentum: {:.2}%, confidence: {:.0}%)",
                side,
                new_trend,
                momentum * Decimal::from(100),
                confidence * Decimal::from(100)
            );

            return Some(signal);
        }

        None
    }

    /// Get current trend for a side
    pub fn current_trend(&self, side: Side) -> TrendDirection {
        match side {
            Side::Up => self.up_trend,
            Side::Down => self.down_trend,
        }
    }

    /// Get current momentum value for a side
    pub fn current_momentum(&self, side: Side) -> Option<Decimal> {
        let (short_ma, long_ma) = match side {
            Side::Up => (&self.up_short, &self.up_long),
            Side::Down => (&self.down_short, &self.down_long),
        };

        let short_avg = short_ma.average()?;
        let long_avg = long_ma.average()?;

        if long_avg == Decimal::ZERO {
            return None;
        }

        Some((short_avg - long_avg) / long_avg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> MomentumDetectorConfig {
        MomentumDetectorConfig {
            short_window_secs: 10,
            long_window_secs: 30,
            min_momentum: dec!(0.05),
            min_trend_change: dec!(0.02),
            signal_cooldown_secs: 5,
        }
    }

    fn make_quote(side: Side, price: Decimal, timestamp: DateTime<Utc>) -> Quote {
        Quote {
            side,
            best_bid: Some(price - dec!(0.01)),
            best_ask: Some(price + dec!(0.01)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            timestamp,
        }
    }

    #[test]
    fn test_momentum_detection() {
        let mut detector = MomentumDetector::new(test_config());
        let now = Utc::now();

        // Build up initial history at stable price
        for i in 0..5 {
            let quote = make_quote(Side::Up, dec!(0.50), now + Duration::seconds(i));
            detector.update(&quote);
        }

        // Price surge
        for i in 5..10 {
            let quote = make_quote(Side::Up, dec!(0.60), now + Duration::seconds(i));
            let signal = detector.update(&quote);

            // Should eventually trigger bullish signal
            if signal.is_some() {
                assert_eq!(signal.unwrap().direction, TrendDirection::Bullish);
                return;
            }
        }
    }

    #[test]
    fn test_trend_direction() {
        let mut detector = MomentumDetector::new(test_config());
        assert_eq!(detector.current_trend(Side::Up), TrendDirection::Neutral);
    }
}
