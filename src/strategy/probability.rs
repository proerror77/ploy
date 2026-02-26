//! Probability estimation for directional prediction markets
//!
//! Uses a log-normal model to estimate P(ST >= S0) given current price,
//! open price, realized volatility, and time remaining.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::volatility::normal_cdf;

/// Full probability estimate with features for logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbabilityEstimate {
    pub p_hat: f64,          // P(Up) estimate [0, 1]
    pub confidence: f64,     // model confidence [0, 1]
    pub features: Features,  // for logging/debugging
}

/// Feature vector used in probability estimation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Features {
    pub distance_to_beat: f64,  // (St - S0) / S0
    pub realized_vol: f64,      // σ from Chainlink history
    pub time_remaining: f64,    // Δt in seconds
    pub momentum_10s: f64,      // Binance 10s momentum (feature, not truth)
    pub momentum_60s: f64,      // Binance 60s momentum
    pub obi: f64,               // Binance L2 orderbook imbalance
    pub binance_chainlink_spread: f64,  // basis risk indicator
}

/// Estimate P(ST >= S0) using log-normal model
///
/// Uses the formula: p_hat = Φ((ln(St/S0) + μΔt) / (σ√Δt))
///
/// Where:
/// - St = current Chainlink price
/// - S0 = Chainlink price at window open
/// - σ = realized volatility from Chainlink history
/// - Δt = time remaining normalized to 15min window
/// - μ = drift estimate (start with 0.0, calibrate later)
/// - Φ = standard normal CDF
pub fn estimate_probability(
    s0: Decimal,
    st: Decimal,
    sigma: f64,
    time_remaining_secs: f64,
    mu: f64,
) -> f64 {
    // Edge case: expired window
    if time_remaining_secs <= 0.0 {
        let s0_f = s0.to_f64().unwrap_or(0.0);
        let st_f = st.to_f64().unwrap_or(0.0);
        return if st_f >= s0_f { 1.0 } else { 0.0 };
    }

    // Edge case: zero vol or invalid prices
    let s0_f = s0.to_f64().unwrap_or(0.0);
    let st_f = st.to_f64().unwrap_or(0.0);
    if sigma <= 0.0 || s0_f <= 0.0 || st_f <= 0.0 {
        return 0.5;
    }

    // Normalize time to 15-minute window
    let dt = time_remaining_secs / 900.0;

    let log_ratio = (st_f / s0_f).ln();
    let z = (log_ratio + mu * dt) / (sigma * dt.sqrt());
    normal_cdf(z)
}

/// Full probability estimate including features for debugging
pub fn full_estimate(
    s0: Decimal,
    st: Decimal,
    sigma: f64,
    time_remaining_secs: f64,
    mu: f64,
    binance_momentum_10s: f64,
    binance_momentum_60s: f64,
    obi: f64,
    binance_price: Option<Decimal>,
) -> ProbabilityEstimate {
    let p_hat = estimate_probability(s0, st, sigma, time_remaining_secs, mu);

    let s0_f = s0.to_f64().unwrap_or(1.0);
    let st_f = st.to_f64().unwrap_or(1.0);
    let distance_to_beat = (st_f - s0_f) / s0_f;

    // Compute basis spread between Binance and Chainlink
    let binance_chainlink_spread = binance_price
        .and_then(|bp| bp.to_f64())
        .map(|bp| (bp - st_f) / st_f)
        .unwrap_or(0.0);

    // Confidence: higher when we have more data points and clearer signal
    // Simple heuristic: confidence scales with |z-score| and available time
    let dt = (time_remaining_secs / 900.0).max(0.001);
    let z = if sigma > 0.0 {
        ((st_f / s0_f).ln() + mu * dt) / (sigma * dt.sqrt())
    } else {
        0.0
    };
    let confidence = (z.abs() / 3.0).min(1.0); // Normalize z to [0,1]

    ProbabilityEstimate {
        p_hat,
        confidence,
        features: Features {
            distance_to_beat,
            realized_vol: sigma,
            time_remaining: time_remaining_secs,
            momentum_10s: binance_momentum_10s,
            momentum_60s: binance_momentum_60s,
            obi,
            binance_chainlink_spread,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_price_above_s0_high_probability() {
        // BTC moved up 1% with low vol, 7.5 min remaining
        let p = estimate_probability(dec!(100), dec!(101), 0.001, 450.0, 0.0);
        assert!(p > 0.7, "p_hat={} should be > 0.7 when price is above S0", p);
    }

    #[test]
    fn test_price_at_s0_near_half() {
        // Price at S0, should be ~0.5
        let p = estimate_probability(dec!(100), dec!(100), 0.001, 450.0, 0.0);
        assert!(
            (p - 0.5).abs() < 0.05,
            "p_hat={} should be ~0.5 at S0",
            p
        );
    }

    #[test]
    fn test_price_below_s0_low_probability() {
        // BTC moved down 1% with low vol
        let p = estimate_probability(dec!(100), dec!(99), 0.001, 450.0, 0.0);
        assert!(
            p < 0.3,
            "p_hat={} should be < 0.3 when price is below S0",
            p
        );
    }

    #[test]
    fn test_expired_window_above() {
        let p = estimate_probability(dec!(100), dec!(101), 0.001, 0.0, 0.0);
        assert_eq!(p, 1.0);
    }

    #[test]
    fn test_expired_window_below() {
        let p = estimate_probability(dec!(100), dec!(99), 0.001, 0.0, 0.0);
        assert_eq!(p, 0.0);
    }

    #[test]
    fn test_zero_vol_returns_half() {
        let p = estimate_probability(dec!(100), dec!(101), 0.0, 450.0, 0.0);
        assert_eq!(p, 0.5);
    }

    #[test]
    fn test_symmetry() {
        // P(up|+1%) + P(up|-1%) should ≈ 1.0
        let p_up = estimate_probability(dec!(100), dec!(101), 0.001, 450.0, 0.0);
        let p_down = estimate_probability(dec!(100), dec!(99), 0.001, 450.0, 0.0);
        assert!(
            (p_up + p_down - 1.0).abs() < 0.1,
            "symmetry: {} + {} should be ~1.0",
            p_up,
            p_down
        );
    }

    #[test]
    fn test_high_vol_reduces_certainty() {
        // Same price move but higher vol should give less extreme probability
        let p_low_vol = estimate_probability(dec!(100), dec!(101), 0.001, 450.0, 0.0);
        let p_high_vol = estimate_probability(dec!(100), dec!(101), 0.01, 450.0, 0.0);
        assert!(
            p_low_vol > p_high_vol,
            "higher vol should reduce certainty"
        );
    }

    #[test]
    fn test_less_time_increases_certainty() {
        // Same price move with less time should be more certain
        // Use sigma=0.01 so z-scores don't saturate the CDF (sigma=0.001 gives z>14)
        let p_more_time = estimate_probability(dec!(100), dec!(101), 0.01, 450.0, 0.0);
        let p_less_time = estimate_probability(dec!(100), dec!(101), 0.01, 60.0, 0.0);
        assert!(
            p_less_time > p_more_time,
            "less time should increase certainty when above S0: p_less={} p_more={}",
            p_less_time,
            p_more_time,
        );
    }

    #[test]
    fn test_full_estimate() {
        let est = full_estimate(
            dec!(100),
            dec!(101),
            0.001,
            450.0,
            0.0,
            0.001,
            0.002,
            0.1,
            Some(dec!(101.05)),
        );
        assert!(est.p_hat > 0.5);
        assert!(est.confidence > 0.0);
        assert!(est.features.distance_to_beat > 0.0);
    }
}
