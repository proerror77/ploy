//! State Representation
//!
//! Defines the observation/state space for RL agents.
//! The state encodes market conditions, position info, and risk state.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::Side;
use crate::strategy::RiskLevel;

/// Total number of features in the state representation
pub const TOTAL_FEATURES: usize = 42;

/// Price history length for momentum features
pub const PRICE_HISTORY_LEN: usize = 15;

/// Raw observation from the environment
///
/// Contains all observable market state before encoding to tensor.
/// Organized into feature groups for clarity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawObservation {
    // =========================================================================
    // Price Features (20 features)
    // =========================================================================
    /// Current spot price from CEX
    pub spot_price: Option<Decimal>,

    /// Historical prices for momentum calculation (last 15 prices)
    pub price_history: Vec<Decimal>,

    /// 1-second momentum (price change %)
    pub momentum_1s: Option<Decimal>,

    /// 5-second momentum
    pub momentum_5s: Option<Decimal>,

    /// 15-second momentum
    pub momentum_15s: Option<Decimal>,

    /// 60-second momentum
    pub momentum_60s: Option<Decimal>,

    // =========================================================================
    // Quote Features (8 features)
    // =========================================================================
    /// Best bid price for UP token
    pub up_bid: Option<Decimal>,

    /// Best ask price for UP token
    pub up_ask: Option<Decimal>,

    /// Best bid price for DOWN token
    pub down_bid: Option<Decimal>,

    /// Best ask price for DOWN token
    pub down_ask: Option<Decimal>,

    /// Spread on UP side (ask - bid)
    pub spread_up: Option<Decimal>,

    /// Spread on DOWN side (ask - bid)
    pub spread_down: Option<Decimal>,

    /// Sum of best asks (UP + DOWN) - key arbitrage signal
    pub sum_of_asks: Option<Decimal>,

    /// Available liquidity at best prices
    pub liquidity: Option<Decimal>,

    // =========================================================================
    // Position Features (6 features)
    // =========================================================================
    /// Whether we have an open position
    pub has_position: bool,

    /// Side of current position (if any)
    pub position_side: Option<Side>,

    /// Number of shares held
    pub position_shares: u64,

    /// Entry price of current position
    pub entry_price: Option<Decimal>,

    /// Unrealized PnL of current position
    pub unrealized_pnl: Option<Decimal>,

    /// Duration of current position in seconds
    pub position_duration_secs: Option<i64>,

    // =========================================================================
    // Risk Features (4 features)
    // =========================================================================
    /// Current risk level
    pub risk_level: RiskLevel,

    /// Portfolio exposure as percentage
    pub exposure_pct: Decimal,

    /// Daily realized PnL
    pub daily_pnl: Decimal,

    /// Number of consecutive order failures
    pub consecutive_failures: u32,

    // =========================================================================
    // Time Features (4 features) - cyclical encoding
    // =========================================================================
    /// Hour of day encoded as sin component
    pub hour_sin: f32,

    /// Hour of day encoded as cos component
    pub hour_cos: f32,

    /// Day of week encoded as sin component
    pub day_sin: f32,

    /// Day of week encoded as cos component
    pub day_cos: f32,
}

impl RawObservation {
    /// Create a new empty observation
    pub fn new() -> Self {
        Self::default()
    }

    /// Update time features from current timestamp
    pub fn update_time_features(&mut self, hour: u32, day_of_week: u32) {
        use std::f32::consts::PI;

        // Cyclical encoding for hour (0-23)
        let hour_rad = 2.0 * PI * (hour as f32) / 24.0;
        self.hour_sin = hour_rad.sin();
        self.hour_cos = hour_rad.cos();

        // Cyclical encoding for day of week (0-6)
        let day_rad = 2.0 * PI * (day_of_week as f32) / 7.0;
        self.day_sin = day_rad.sin();
        self.day_cos = day_rad.cos();
    }

    /// Calculate sum of asks if both quotes available
    pub fn calculate_sum_of_asks(&mut self) {
        if let (Some(up_ask), Some(down_ask)) = (self.up_ask, self.down_ask) {
            self.sum_of_asks = Some(up_ask + down_ask);
        }
    }

    /// Calculate spreads if bid/ask available
    pub fn calculate_spreads(&mut self) {
        if let (Some(bid), Some(ask)) = (self.up_bid, self.up_ask) {
            self.spread_up = Some(ask - bid);
        }
        if let (Some(bid), Some(ask)) = (self.down_bid, self.down_ask) {
            self.spread_down = Some(ask - bid);
        }
    }
}

/// Trait for encoding raw observations into tensors
pub trait StateEncoder: Send + Sync {
    /// Encode a raw observation into a feature vector
    fn encode(&self, obs: &RawObservation) -> Vec<f32>;

    /// Get the output dimension
    fn output_dim(&self) -> usize {
        TOTAL_FEATURES
    }
}

/// Default state encoder with normalization
#[derive(Debug, Clone)]
pub struct DefaultStateEncoder {
    /// Running mean for z-score normalization
    running_mean: Vec<f32>,
    /// Running variance for z-score normalization
    running_var: Vec<f32>,
    /// Number of samples seen
    count: usize,
    /// Whether to update statistics
    training: bool,
}

impl Default for DefaultStateEncoder {
    fn default() -> Self {
        Self {
            running_mean: vec![0.0; TOTAL_FEATURES],
            running_var: vec![1.0; TOTAL_FEATURES],
            count: 0,
            training: true,
        }
    }
}

impl DefaultStateEncoder {
    /// Create a new encoder
    pub fn new() -> Self {
        Self::default()
    }

    /// Set training mode
    pub fn set_training(&mut self, training: bool) {
        self.training = training;
    }

    /// Convert Decimal to f32, defaulting to 0.0
    fn decimal_to_f32(d: Option<Decimal>) -> f32 {
        d.and_then(|v| v.to_string().parse().ok()).unwrap_or(0.0)
    }

    /// Normalize using running statistics
    fn normalize(&self, features: &mut [f32]) {
        for (i, f) in features.iter_mut().enumerate() {
            let std = self.running_var[i].sqrt().max(1e-8);
            *f = (*f - self.running_mean[i]) / std;
        }
    }

    /// Update running statistics
    fn update_stats(&mut self, features: &[f32]) {
        self.count += 1;
        let n = self.count as f32;

        for (i, &f) in features.iter().enumerate() {
            let delta = f - self.running_mean[i];
            self.running_mean[i] += delta / n;
            let delta2 = f - self.running_mean[i];
            self.running_var[i] += (delta * delta2 - self.running_var[i]) / n;
        }
    }
}

impl StateEncoder for DefaultStateEncoder {
    fn encode(&self, obs: &RawObservation) -> Vec<f32> {
        let mut features = Vec::with_capacity(TOTAL_FEATURES);

        // Price features (20)
        features.push(Self::decimal_to_f32(obs.spot_price));

        // Pad or truncate price history to PRICE_HISTORY_LEN
        for i in 0..PRICE_HISTORY_LEN {
            let price = obs
                .price_history
                .get(i)
                .map(|p| Self::decimal_to_f32(Some(*p)))
                .unwrap_or(0.0);
            features.push(price);
        }

        features.push(Self::decimal_to_f32(obs.momentum_1s));
        features.push(Self::decimal_to_f32(obs.momentum_5s));
        features.push(Self::decimal_to_f32(obs.momentum_15s));
        features.push(Self::decimal_to_f32(obs.momentum_60s));

        // Quote features (8)
        features.push(Self::decimal_to_f32(obs.up_bid));
        features.push(Self::decimal_to_f32(obs.up_ask));
        features.push(Self::decimal_to_f32(obs.down_bid));
        features.push(Self::decimal_to_f32(obs.down_ask));
        features.push(Self::decimal_to_f32(obs.spread_up));
        features.push(Self::decimal_to_f32(obs.spread_down));
        features.push(Self::decimal_to_f32(obs.sum_of_asks));
        features.push(Self::decimal_to_f32(obs.liquidity));

        // Position features (6)
        features.push(if obs.has_position { 1.0 } else { 0.0 });
        features.push(match obs.position_side {
            Some(Side::Up) => 1.0,
            Some(Side::Down) => -1.0,
            None => 0.0,
        });
        features.push(obs.position_shares as f32);
        features.push(Self::decimal_to_f32(obs.entry_price));
        features.push(Self::decimal_to_f32(obs.unrealized_pnl));
        features.push(obs.position_duration_secs.unwrap_or(0) as f32);

        // Risk features (4)
        features.push(match obs.risk_level {
            RiskLevel::Normal => 0.0,
            RiskLevel::Elevated => 0.33,
            RiskLevel::Critical => 0.67,
            RiskLevel::Halted => 1.0,
        });
        features.push(Self::decimal_to_f32(Some(obs.exposure_pct)));
        features.push(Self::decimal_to_f32(Some(obs.daily_pnl)));
        features.push(obs.consecutive_failures as f32);

        // Time features (4)
        features.push(obs.hour_sin);
        features.push(obs.hour_cos);
        features.push(obs.day_sin);
        features.push(obs.day_cos);

        // Ensure we have exactly TOTAL_FEATURES
        debug_assert_eq!(
            features.len(),
            TOTAL_FEATURES,
            "Feature count mismatch: {} vs {}",
            features.len(),
            TOTAL_FEATURES
        );

        features
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observation_default() {
        let obs = RawObservation::new();
        assert!(!obs.has_position);
        assert_eq!(obs.position_shares, 0);
    }

    #[test]
    fn test_time_encoding() {
        let mut obs = RawObservation::new();
        obs.update_time_features(12, 3); // Noon on Wednesday

        // At noon (12), sin should be ~0, cos should be ~-1
        assert!(obs.hour_sin.abs() < 0.1);
        assert!(obs.hour_cos < -0.9);
    }

    #[test]
    fn test_encoder_output_dim() {
        let encoder = DefaultStateEncoder::new();
        let obs = RawObservation::new();
        let features = encoder.encode(&obs);

        assert_eq!(features.len(), TOTAL_FEATURES);
    }
}
