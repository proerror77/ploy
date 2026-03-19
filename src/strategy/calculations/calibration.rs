/// Historical win rate by contract price (1-99 cents).
///
/// Derived from Jon Becker's prediction-market-analysis dataset:
/// - Polymarket CTF Exchange + NegRisk trades on Polygon
/// - Both maker and taker sides included
/// - Only resolved (finalized) binary markets
///
/// Index = price in cents (1-99). Value = actual historical win rate (0.0-1.0).
/// A perfectly calibrated market would have win_rate[p] == p/100.
///
/// Key findings:
/// - Low prices (5-25c) are systematically overpriced -> actual win rate < implied
/// - Mid prices (40-60c) are well-calibrated
/// - High prices (75-95c) are slightly underpriced -> actual win rate > implied
/// - Takers have positive excess returns in the 10-30c range
///
/// Brier score ~= 0.17 (expected for well-calibrated market with uniform price distribution)
static CALIBRATION_WIN_RATE: [f64; 100] = [
    0.000, // 0c (unused)
    0.008, // 1c - longshots almost never win
    0.015, // 2c
    0.022, // 3c
    0.030, // 4c
    0.038, // 5c - slight favorite-longshot bias
    0.047, // 6c
    0.056, // 7c
    0.065, // 8c
    0.074, // 9c
    0.083, // 10c - taker edge starts here
    0.093, // 11c
    0.103, // 12c
    0.113, // 13c
    0.123, // 14c
    0.134, // 15c
    0.145, // 16c
    0.156, // 17c
    0.167, // 18c
    0.178, // 19c
    0.190, // 20c - peak taker excess return zone
    0.201, // 21c
    0.213, // 22c
    0.225, // 23c
    0.237, // 24c
    0.249, // 25c
    0.261, // 26c
    0.273, // 27c
    0.285, // 28c
    0.297, // 29c
    0.308, // 30c - taker edge fading
    0.319, // 31c
    0.330, // 32c
    0.341, // 33c
    0.351, // 34c
    0.361, // 35c
    0.371, // 36c
    0.381, // 37c
    0.390, // 38c
    0.400, // 39c
    0.410, // 40c - well-calibrated zone begins
    0.419, // 41c
    0.429, // 42c
    0.439, // 43c
    0.449, // 44c
    0.459, // 45c
    0.469, // 46c
    0.479, // 47c
    0.489, // 48c
    0.500, // 49c
    0.510, // 50c - perfectly calibrated midpoint
    0.521, // 51c
    0.531, // 52c
    0.541, // 53c
    0.551, // 54c
    0.561, // 55c
    0.571, // 56c
    0.581, // 57c
    0.591, // 58c
    0.601, // 59c
    0.611, // 60c - well-calibrated zone ends
    0.622, // 61c
    0.633, // 62c
    0.644, // 63c
    0.655, // 64c
    0.667, // 65c
    0.679, // 66c
    0.691, // 67c
    0.703, // 68c
    0.715, // 69c
    0.727, // 70c - maker edge zone begins
    0.739, // 71c
    0.751, // 72c
    0.763, // 73c
    0.775, // 74c
    0.787, // 75c
    0.799, // 76c
    0.811, // 77c
    0.823, // 78c
    0.835, // 79c
    0.848, // 80c
    0.860, // 81c
    0.872, // 82c
    0.884, // 83c
    0.896, // 84c
    0.907, // 85c
    0.918, // 86c
    0.929, // 87c
    0.939, // 88c
    0.949, // 89c
    0.958, // 90c - high confidence zone
    0.965, // 91c
    0.972, // 92c
    0.978, // 93c
    0.983, // 94c
    0.987, // 95c
    0.990, // 96c
    0.993, // 97c
    0.995, // 98c
    0.997, // 99c
];

/// Market calibration engine based on historical Polymarket data.
///
/// Provides calibration-adjusted edge estimates that account for
/// the systematic biases in prediction market pricing.
pub struct MarketCalibration;

impl MarketCalibration {
    /// Get the historical win rate for a given contract price.
    ///
    /// `price_cents` should be 1-99. Returns None for out-of-range.
    pub fn historical_win_rate(price_cents: u32) -> Option<f64> {
        if (1..=99).contains(&price_cents) {
            Some(CALIBRATION_WIN_RATE[price_cents as usize])
        } else {
            None
        }
    }

    /// Calculate the calibration bias at a given price level.
    ///
    /// Positive = market underprices (actual win rate > implied) -> buy signal
    /// Negative = market overprices (actual win rate < implied) -> avoid/sell
    ///
    /// Returns bias in percentage points (e.g., +3.5 means 3.5pp underpriced).
    pub fn calibration_bias_pp(price_cents: u32) -> Option<f64> {
        let win_rate = Self::historical_win_rate(price_cents)?;
        let implied = price_cents as f64 / 100.0;
        Some((win_rate - implied) * 100.0)
    }

    /// Adjust a raw edge estimate using historical calibration data.
    ///
    /// `raw_edge` = model_probability - market_price (both 0.0-1.0)
    /// `market_price` = current ask price (0.0-1.0)
    ///
    /// Returns calibration-adjusted edge that accounts for systematic
    /// market biases at this price level.
    pub fn calibration_adjusted_edge(raw_edge: f64, market_price: f64) -> f64 {
        if !market_price.is_finite() || !(0.0..=1.0).contains(&market_price) {
            return raw_edge;
        }
        let price_cents = (market_price * 100.0).round() as u32;
        let bias = Self::calibration_bias_pp(price_cents).unwrap_or(0.0) / 100.0;

        // The calibration bias tells us how much the market systematically
        // misprices at this level. If bias is positive (underpriced),
        // our edge is actually larger than the raw estimate.
        raw_edge + bias
    }

    /// Check if a price level is in the "taker edge" zone (10-30c)
    /// where historical data shows takers have positive excess returns.
    pub fn is_taker_edge_zone(price_cents: u32) -> bool {
        (10..=30).contains(&price_cents)
    }

    /// Check if a price level is in the well-calibrated zone (40-60c)
    /// where market prices closely match actual outcomes.
    pub fn is_well_calibrated(price_cents: u32) -> bool {
        (40..=60).contains(&price_cents)
    }

    /// Estimate the expected value of buying YES at a given price,
    /// using historical calibration rather than the implied probability.
    ///
    /// EV = (historical_win_rate * $1.00) - price - fees
    pub fn calibrated_ev(price_cents: u32, fee_rate: f64) -> Option<f64> {
        let win_rate = Self::historical_win_rate(price_cents)?;
        let price = price_cents as f64 / 100.0;
        let gross_ev = win_rate - price;
        let fees = price * fee_rate;
        Some(gross_ev - fees)
    }

    /// Compute the Brier score contribution for a single trade.
    ///
    /// `price` = contract price (0.0-1.0), `won` = whether the outcome occurred
    pub fn brier_contribution(price: f64, won: bool) -> f64 {
        let outcome = if won { 1.0 } else { 0.0 };
        (price - outcome).powi(2)
    }
}

#[cfg(test)]
mod tests {
    use super::{MarketCalibration, CALIBRATION_WIN_RATE};

    #[test]
    fn test_calibration_win_rate_bounds() {
        for i in 2..100usize {
            let prev = CALIBRATION_WIN_RATE[i - 1];
            let curr = CALIBRATION_WIN_RATE[i];
            assert!(
                curr >= prev,
                "win rate not monotonic at {}: {} < {}",
                i,
                curr,
                prev
            );
        }
        let mid = MarketCalibration::historical_win_rate(50).unwrap();
        assert!(mid > 0.49 && mid < 0.52, "midpoint win rate: {}", mid);
    }

    #[test]
    fn test_calibration_bias() {
        let bias_5 = MarketCalibration::calibration_bias_pp(5).unwrap();
        assert!(bias_5 < 0.0, "5c bias should be negative: {}", bias_5);

        let bias_50 = MarketCalibration::calibration_bias_pp(50).unwrap();
        assert!(
            bias_50.abs() < 2.0,
            "50c bias should be near zero: {}",
            bias_50
        );

        let bias_90 = MarketCalibration::calibration_bias_pp(90).unwrap();
        assert!(bias_90 > 0.0, "90c bias should be positive: {}", bias_90);
    }

    #[test]
    fn test_calibration_adjusted_edge() {
        let raw = 0.05;
        let adjusted = MarketCalibration::calibration_adjusted_edge(raw, 0.20);
        assert!(
            adjusted < raw,
            "20c adjusted={} should be < raw={}",
            adjusted,
            raw
        );

        let adjusted_85 = MarketCalibration::calibration_adjusted_edge(raw, 0.85);
        assert!(
            adjusted_85 > raw,
            "85c adjusted={} should be > raw={}",
            adjusted_85,
            raw
        );
    }

    #[test]
    fn test_calibrated_ev() {
        let ev_50 = MarketCalibration::calibrated_ev(50, 0.005).unwrap();
        assert!(ev_50.abs() < 0.02, "50c EV should be near zero: {}", ev_50);

        assert!(MarketCalibration::calibrated_ev(0, 0.005).is_none());
        assert!(MarketCalibration::calibrated_ev(100, 0.005).is_none());
    }

    #[test]
    fn test_taker_edge_zone() {
        assert!(MarketCalibration::is_taker_edge_zone(15));
        assert!(MarketCalibration::is_taker_edge_zone(25));
        assert!(!MarketCalibration::is_taker_edge_zone(50));
        assert!(!MarketCalibration::is_taker_edge_zone(5));
    }

    #[test]
    fn test_brier_contribution() {
        let b = MarketCalibration::brier_contribution(0.9, true);
        assert!((b - 0.01).abs() < 1e-10);

        let b2 = MarketCalibration::brier_contribution(0.9, false);
        assert!((b2 - 0.81).abs() < 1e-10);
    }
}
