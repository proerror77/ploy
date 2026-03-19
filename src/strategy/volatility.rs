//! Volatility-based trading strategy for 15-minute prediction markets
//!
//! Uses event start price tracking, OBI signals, and volatility prediction
//! to identify trading opportunities.

mod state;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::domain::Side;

pub use state::{ActiveEvent, EventRecord, EventTracker};

/// Standard normal CDF approximation (Abramowitz-Stegun)
/// Accurate to ~4 decimal places
pub fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;

    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

/// Configuration for volatility strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityConfig {
    /// Maximum entry price for tokens (e.g., 0.30 = 30¢)
    pub max_entry_price: Decimal,
    /// Minimum edge required (fair_value - entry_price)
    pub min_edge: Decimal,
    /// Minimum absolute deviation from start price to trigger signal
    pub min_deviation_pct: Decimal,
    /// OBI threshold for directional confidence (e.g., 0.1 = 10% imbalance)
    pub obi_threshold: Decimal,
    /// OBI levels to use for calculation
    pub obi_levels: usize,
    /// Minimum time remaining to enter (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds)
    pub max_time_remaining_secs: u64,
    /// Number of historical events to track for volatility estimation
    pub history_window: usize,
    /// Shares per trade
    pub shares_per_trade: u64,
}

impl Default for VolatilityConfig {
    fn default() -> Self {
        Self {
            max_entry_price: dec!(0.30),     // Max 30¢ entry
            min_edge: dec!(0.05),            // 5% minimum edge
            min_deviation_pct: dec!(0.0005), // 0.05% minimum deviation from start
            obi_threshold: dec!(0.05),       // 5% OBI imbalance threshold
            obi_levels: 5,                   // Use top 5 levels
            min_time_remaining_secs: 60,     // Min 1 minute left
            max_time_remaining_secs: 600,    // Max 10 minutes left
            history_window: 20,              // Track last 20 events
            shares_per_trade: 100,
        }
    }
}

/// Trading signal from volatility strategy
#[derive(Debug, Clone)]
pub struct VolatilitySignal {
    pub symbol: String,
    pub event_id: String,
    pub side: Side,
    pub entry_price: Decimal,   // Token price to pay
    pub fair_value: Decimal,    // Estimated fair value
    pub edge: Decimal,          // fair_value - entry_price
    pub deviation_pct: Decimal, // Current deviation from start
    pub obi: Decimal,           // Order book imbalance
    pub time_remaining_secs: i64,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
}

/// Volatility-based signal detector
pub struct VolatilityDetector {
    config: VolatilityConfig,
    event_tracker: EventTracker,
}

impl VolatilityDetector {
    pub fn new(config: VolatilityConfig) -> Self {
        Self {
            event_tracker: EventTracker::new(config.history_window),
            config,
        }
    }

    /// Get mutable reference to event tracker
    pub fn event_tracker_mut(&mut self) -> &mut EventTracker {
        &mut self.event_tracker
    }

    /// Get reference to event tracker
    pub fn event_tracker(&self) -> &EventTracker {
        &self.event_tracker
    }

    /// Check for trading signal using internal event tracker
    pub fn check_signal_internal(
        &self,
        symbol: &str,
        event_id: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        obi: Option<Decimal>,
        price_to_beat: Option<Decimal>,
    ) -> Option<VolatilitySignal> {
        self.check_signal(
            symbol,
            event_id,
            &self.event_tracker,
            up_ask,
            down_ask,
            obi,
            price_to_beat,
        )
    }

    /// Check for trading signal using external event tracker
    ///
    /// # Arguments
    /// * `price_to_beat` - The threshold price from Polymarket (e.g., $94,000 for "Will BTC be above $94,000?")
    ///                     If None, falls back to using start_price
    pub fn check_signal(
        &self,
        symbol: &str,
        event_id: &str,
        tracker: &EventTracker,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        obi: Option<Decimal>,
        price_to_beat: Option<Decimal>,
    ) -> Option<VolatilitySignal> {
        let event = tracker.get_active_event(symbol, event_id)?;

        let time_remaining = event.time_remaining_secs();

        // Check time window
        if time_remaining < self.config.min_time_remaining_secs as i64 {
            debug!(
                "{} time remaining {}s < min {}s",
                symbol, time_remaining, self.config.min_time_remaining_secs
            );
            return None;
        }
        if time_remaining > self.config.max_time_remaining_secs as i64 {
            debug!(
                "{} time remaining {}s > max {}s",
                symbol, time_remaining, self.config.max_time_remaining_secs
            );
            return None;
        }

        // Use price_to_beat if available, otherwise fall back to start_price
        let reference_price = price_to_beat.unwrap_or(event.start_price);
        let current_price = event.current_price;

        // Calculate deviation from the reference price (price_to_beat or start_price)
        let deviation = if reference_price.is_zero() {
            Decimal::ZERO
        } else {
            (current_price - reference_price) / reference_price
        };

        let obi_value = obi.unwrap_or(Decimal::ZERO);

        // Determine direction: if current > price_to_beat, UP wins; otherwise DOWN wins
        let (predicted_side, token_price) = if deviation > Decimal::ZERO {
            // Current price is ABOVE price_to_beat → UP wins
            (Side::Up, up_ask?)
        } else {
            // Current price is BELOW price_to_beat → DOWN wins
            (Side::Down, down_ask?)
        };

        // Log which reference we're using
        if price_to_beat.is_some() {
            debug!(
                "{} using price_to_beat={:.2} current={:.2} deviation={:.4}%",
                symbol,
                reference_price,
                current_price,
                deviation * dec!(100)
            );
        }

        // Check minimum deviation
        if deviation.abs() < self.config.min_deviation_pct {
            debug!(
                "{} deviation {:.4}% < min {:.4}%",
                symbol,
                deviation * dec!(100),
                self.config.min_deviation_pct * dec!(100)
            );
            return None;
        }

        // Check OBI confirmation (optional but increases confidence)
        let obi_confirms = match predicted_side {
            Side::Up => obi_value > self.config.obi_threshold,
            Side::Down => obi_value < -self.config.obi_threshold,
        };

        // Check entry price
        if token_price > self.config.max_entry_price {
            debug!(
                "{} {:?} token price {:.1}¢ > max {:.1}¢",
                symbol,
                predicted_side,
                token_price * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
            return None;
        }

        // Get historical volatility for proper Z-score calculation
        let volatility = tracker.historical_volatility(symbol).unwrap_or(dec!(0.003)); // Default 0.3% if no history

        // Calculate fair value based on Z-score model
        let fair_value = self.estimate_fair_value(deviation, time_remaining, obi_value, volatility);
        let edge = fair_value - token_price;

        if edge < self.config.min_edge {
            debug!(
                "{} {:?} edge {:.1}% < min {:.1}%",
                symbol,
                predicted_side,
                edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return None;
        }

        // Calculate confidence
        let confidence = self.calculate_confidence(deviation, obi_confirms, time_remaining, edge);

        info!(
            "🎯 SIGNAL: {} {:?} | dev={:.3}% obi={:.2} | price={:.1}¢ fair={:.1}¢ edge={:.1}% | {}s left",
            symbol,
            predicted_side,
            deviation * dec!(100),
            obi_value,
            token_price * dec!(100),
            fair_value * dec!(100),
            edge * dec!(100),
            time_remaining
        );

        Some(VolatilitySignal {
            symbol: symbol.to_string(),
            event_id: event_id.to_string(),
            side: predicted_side,
            entry_price: token_price,
            fair_value,
            edge,
            deviation_pct: deviation,
            obi: obi_value,
            time_remaining_secs: time_remaining,
            confidence,
            timestamp: Utc::now(),
        })
    }

    /// Estimate fair value using Z-score model with proper statistical foundations
    ///
    /// The model calculates probability that price stays above/below start price:
    /// 1. Z-score = deviation / (volatility × √(time_remaining / 900))
    /// 2. Fair value = Φ(z_score) where Φ is standard normal CDF
    /// 3. Apply OBI confirmation bonus
    ///
    /// This properly accounts for:
    /// - Larger deviations = higher probability (non-linear via normal CDF)
    /// - Less time remaining = higher probability (√time scaling from Brownian motion)
    /// - Higher volatility = lower probability for same deviation
    fn estimate_fair_value(
        &self,
        deviation: Decimal,
        time_remaining_secs: i64,
        obi: Decimal,
        historical_volatility: Decimal,
    ) -> Decimal {
        // Convert to f64 for math operations
        let dev = deviation.abs().to_f64().unwrap_or(0.0);
        let vol = historical_volatility.abs().to_f64().unwrap_or(0.003);
        let time_remaining = time_remaining_secs.max(1) as f64;

        // Total window is 900 seconds (15 minutes)
        const TOTAL_WINDOW_SECS: f64 = 900.0;

        // Calculate expected volatility for remaining time using √time scaling
        // If 5 min (300s) remaining, expected move = vol × √(300/900) = vol × 0.577
        let time_factor = (time_remaining / TOTAL_WINDOW_SECS).sqrt();
        let expected_vol = vol * time_factor;

        // Avoid division by zero
        let expected_vol = expected_vol.max(0.0001);

        // Z-score: how many standard deviations away from mean (0) is current deviation?
        // Higher z-score = current deviation is more significant relative to expected movement
        let z_score = dev / expected_vol;

        // Convert z-score to probability using normal CDF
        // Φ(1.0) ≈ 0.84, Φ(2.0) ≈ 0.98, Φ(3.0) ≈ 0.999
        let base_probability = normal_cdf(z_score);

        // OBI confirmation bonus: +3% if strong OBI confirms direction
        let obi_val = obi.to_f64().unwrap_or(0.0);
        let obi_bonus = if obi_val.abs() > 0.1 { 0.03 } else { 0.0 };

        // Calculate fair value with OBI bonus, cap at 95%
        let fair_value = (base_probability + obi_bonus).min(0.95);

        // Log the calculation for debugging
        debug!(
            "Z-score calc: dev={:.4}% vol={:.4}% time={}s => z={:.2} => p={:.1}%",
            dev * 100.0,
            vol * 100.0,
            time_remaining_secs,
            z_score,
            fair_value * 100.0
        );

        Decimal::try_from(fair_value).unwrap_or(dec!(0.50))
    }

    /// Calculate confidence score (0.0 to 1.0)
    fn calculate_confidence(
        &self,
        deviation: Decimal,
        obi_confirms: bool,
        time_remaining_secs: i64,
        edge: Decimal,
    ) -> f64 {
        let mut score: f64 = 0.0;

        // Deviation score (0-0.3)
        let dev_abs = deviation.abs();
        if dev_abs > dec!(0.005) {
            score += 0.3;
        } else if dev_abs > dec!(0.002) {
            score += 0.2;
        } else if dev_abs > dec!(0.001) {
            score += 0.1;
        }

        // OBI confirmation (0-0.2)
        if obi_confirms {
            score += 0.2;
        }

        // Time remaining score (0-0.3)
        if time_remaining_secs < 120 {
            score += 0.3;
        } else if time_remaining_secs < 300 {
            score += 0.2;
        } else {
            score += 0.1;
        }

        // Edge score (0-0.2)
        if edge > dec!(0.15) {
            score += 0.2;
        } else if edge > dec!(0.10) {
            score += 0.15;
        } else if edge > dec!(0.05) {
            score += 0.1;
        }

        score.min(1.0)
    }
}
