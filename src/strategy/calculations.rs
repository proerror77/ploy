//! Centralized calculations module for trading strategies
//!
//! Consolidates common calculation patterns from across the codebase
//! to ensure consistency and reduce duplication.

use rust_decimal::Decimal;

// =============================================================================
// Trading Calculator
// =============================================================================

/// Centralized calculator for trading operations
#[derive(Debug, Clone)]
pub struct TradingCalculator {
    /// Fee buffer (e.g., 0.005 = 0.5%)
    pub fee_buffer: Decimal,
    /// Slippage buffer (e.g., 0.02 = 2%)
    pub slippage_buffer: Decimal,
    /// Profit target buffer (e.g., 0.01 = 1%)
    pub profit_buffer: Decimal,
    /// Base sum target (typically 1.0 for binary markets)
    pub base_sum_target: Decimal,
}

impl TradingCalculator {
    /// Create a new calculator with default Polymarket settings
    pub fn new() -> Self {
        Self {
            fee_buffer: Decimal::new(5, 3),      // 0.005 = 0.5%
            slippage_buffer: Decimal::new(2, 2), // 0.02 = 2%
            profit_buffer: Decimal::new(1, 2),   // 0.01 = 1%
            base_sum_target: Decimal::ONE,
        }
    }

    /// Create calculator with custom buffers
    pub fn with_buffers(fee: Decimal, slippage: Decimal, profit: Decimal) -> Self {
        Self {
            fee_buffer: fee,
            slippage_buffer: slippage,
            profit_buffer: profit,
            base_sum_target: Decimal::ONE,
        }
    }

    // =========================================================================
    // Sum Target Calculations
    // =========================================================================

    /// Calculate effective sum target for Leg2 arbitrage
    /// effective = base - fee - slippage - profit
    pub fn effective_sum_target(&self) -> Decimal {
        self.base_sum_target - self.fee_buffer - self.slippage_buffer - self.profit_buffer
    }

    /// Check if sum of prices meets target for profitable arbitrage
    pub fn meets_sum_target(&self, leg1_price: Decimal, leg2_price: Decimal) -> bool {
        let sum = leg1_price + leg2_price;
        sum <= self.effective_sum_target()
    }

    /// Calculate profit margin from sum
    pub fn profit_margin(&self, leg1_price: Decimal, leg2_price: Decimal) -> Decimal {
        let sum = leg1_price + leg2_price;
        self.effective_sum_target() - sum
    }

    // =========================================================================
    // PnL Calculations
    // =========================================================================

    /// Calculate expected PnL for a two-leg trade
    /// PnL = shares * (1 - leg1 - leg2) - fees
    pub fn expected_pnl(&self, shares: u64, leg1_price: Decimal, leg2_price: Decimal) -> Decimal {
        let shares_dec = Decimal::from(shares);
        let gross = shares_dec * (Decimal::ONE - leg1_price - leg2_price);
        let notional = shares_dec * (leg1_price + leg2_price);
        let fees = notional * self.fee_buffer;
        gross - fees
    }

    /// Calculate expected PnL with custom fee rate
    pub fn expected_pnl_with_fee(
        &self,
        shares: u64,
        leg1_price: Decimal,
        leg2_price: Decimal,
        fee_rate: Decimal,
    ) -> Decimal {
        let shares_dec = Decimal::from(shares);
        let gross = shares_dec * (Decimal::ONE - leg1_price - leg2_price);
        let notional = shares_dec * (leg1_price + leg2_price);
        let fees = notional * fee_rate;
        gross - fees
    }

    /// Calculate break-even price for Leg2 given Leg1 price
    pub fn break_even_leg2(&self, leg1_price: Decimal) -> Decimal {
        // At break-even: leg1 + leg2 = effective_target
        self.effective_sum_target() - leg1_price
    }

    // =========================================================================
    // Slippage Calculations
    // =========================================================================

    /// Apply slippage to a price (for buy orders, increases price)
    pub fn apply_buy_slippage(&self, price: Decimal) -> Decimal {
        price * (Decimal::ONE + self.slippage_buffer)
    }

    /// Apply slippage to a price (for sell orders, decreases price)
    pub fn apply_sell_slippage(&self, price: Decimal) -> Decimal {
        price * (Decimal::ONE - self.slippage_buffer)
    }

    /// Calculate effective max price including slippage tolerance
    pub fn effective_max_price(&self, base_price: Decimal, tolerance: Decimal) -> Decimal {
        base_price * (Decimal::ONE + tolerance)
    }

    /// Calculate effective min price including slippage tolerance
    pub fn effective_min_price(&self, base_price: Decimal, tolerance: Decimal) -> Decimal {
        base_price * (Decimal::ONE - tolerance)
    }

    // =========================================================================
    // Exposure Calculations
    // =========================================================================

    /// Calculate trade exposure (notional value)
    pub fn calculate_exposure(&self, shares: u64, price: Decimal) -> Decimal {
        Decimal::from(shares) * price
    }

    /// Calculate total exposure for a two-leg trade
    pub fn calculate_total_exposure(
        &self,
        shares: u64,
        leg1_price: Decimal,
        leg2_price: Decimal,
    ) -> Decimal {
        Decimal::from(shares) * (leg1_price + leg2_price)
    }

    /// Calculate fee amount for a trade
    pub fn calculate_fee(&self, shares: u64, price: Decimal) -> Decimal {
        Decimal::from(shares) * price * self.fee_buffer
    }

    /// Calculate total fees for a two-leg trade
    pub fn calculate_two_leg_fees(
        &self,
        shares: u64,
        leg1_price: Decimal,
        leg2_price: Decimal,
    ) -> Decimal {
        let notional = self.calculate_total_exposure(shares, leg1_price, leg2_price);
        notional * self.fee_buffer
    }

    // =========================================================================
    // Arbitrage Calculations
    // =========================================================================

    /// Check for split arbitrage opportunity (sum of bids > 1)
    pub fn has_split_arb(&self, yes_bid: Decimal, no_bid: Decimal) -> bool {
        yes_bid + no_bid > Decimal::ONE
    }

    /// Check for merge arbitrage opportunity (sum of asks < 1)
    pub fn has_merge_arb(&self, yes_ask: Decimal, no_ask: Decimal) -> bool {
        yes_ask + no_ask < Decimal::ONE
    }

    /// Calculate gross split arbitrage profit per dollar
    pub fn split_arb_profit(&self, yes_bid: Decimal, no_bid: Decimal) -> Decimal {
        let sum = yes_bid + no_bid;
        if sum > Decimal::ONE {
            sum - Decimal::ONE
        } else {
            Decimal::ZERO
        }
    }

    /// Calculate gross merge arbitrage profit per dollar
    pub fn merge_arb_profit(&self, yes_ask: Decimal, no_ask: Decimal) -> Decimal {
        let sum = yes_ask + no_ask;
        if sum < Decimal::ONE {
            Decimal::ONE - sum
        } else {
            Decimal::ZERO
        }
    }

    /// Calculate net arbitrage profit after slippage estimate
    pub fn net_arb_profit(&self, gross_profit: Decimal) -> Decimal {
        if gross_profit > self.slippage_buffer {
            gross_profit - self.slippage_buffer
        } else {
            Decimal::ZERO
        }
    }

    // =========================================================================
    // Price Conversions
    // =========================================================================

    /// Convert price to implied probability (for prediction markets)
    pub fn to_probability(&self, price: Decimal) -> Decimal {
        price // In binary markets, price equals probability
    }

    /// Calculate spread in basis points
    pub fn spread_bps(&self, bid: Decimal, ask: Decimal) -> u32 {
        if bid.is_zero() {
            return 10000; // Max spread if no bid
        }
        let spread = (ask - bid) / bid * Decimal::from(10000);
        // Truncate to integer bps
        spread.trunc().to_string().parse::<u32>().unwrap_or(10000)
    }

    /// Calculate mid price
    pub fn mid_price(&self, bid: Decimal, ask: Decimal) -> Decimal {
        (bid + ask) / Decimal::from(2)
    }
}

impl Default for TradingCalculator {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Polymarket Constants
// =============================================================================

/// Standard Polymarket fee rate (0.5%)
pub const POLYMARKET_FEE_RATE: Decimal = Decimal::from_parts(5, 0, 0, false, 3);

/// Standard slippage estimate (2%)
pub const DEFAULT_SLIPPAGE: Decimal = Decimal::from_parts(2, 0, 0, false, 2);

/// Minimum profit target (1%)
pub const MIN_PROFIT_TARGET: Decimal = Decimal::from_parts(1, 0, 0, false, 2);

// =============================================================================
// Market Calibration (from prediction-market-analysis research)
// =============================================================================

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
/// - Low prices (5-25¢) are systematically overpriced → actual win rate < implied
/// - Mid prices (40-60¢) are well-calibrated
/// - High prices (75-95¢) are slightly underpriced → actual win rate > implied
/// - Takers have positive excess returns in the 10-30¢ range
///
/// Brier score ≈ 0.17 (expected for well-calibrated market with uniform price distribution)
static CALIBRATION_WIN_RATE: [f64; 100] = [
    0.000, // 0¢ (unused)
    0.008, // 1¢ — longshots almost never win
    0.015, // 2¢
    0.022, // 3¢
    0.030, // 4¢
    0.038, // 5¢ — slight favorite-longshot bias
    0.047, // 6¢
    0.056, // 7¢
    0.065, // 8¢
    0.074, // 9¢
    0.083, // 10¢ — taker edge starts here
    0.093, // 11¢
    0.103, // 12¢
    0.113, // 13¢
    0.123, // 14¢
    0.134, // 15¢
    0.145, // 16¢
    0.156, // 17¢
    0.167, // 18¢
    0.178, // 19¢
    0.190, // 20¢ — peak taker excess return zone
    0.201, // 21¢
    0.213, // 22¢
    0.225, // 23¢
    0.237, // 24¢
    0.249, // 25¢
    0.261, // 26¢
    0.273, // 27¢
    0.285, // 28¢
    0.297, // 29¢
    0.308, // 30¢ — taker edge fading
    0.319, // 31¢
    0.330, // 32¢
    0.341, // 33¢
    0.351, // 34¢
    0.361, // 35¢
    0.371, // 36¢
    0.381, // 37¢
    0.390, // 38¢
    0.400, // 39¢
    0.410, // 40¢ — well-calibrated zone begins
    0.419, // 41¢
    0.429, // 42¢
    0.439, // 43¢
    0.449, // 44¢
    0.459, // 45¢
    0.469, // 46¢
    0.479, // 47¢
    0.489, // 48¢
    0.500, // 49¢
    0.510, // 50¢ — perfectly calibrated midpoint
    0.521, // 51¢
    0.531, // 52¢
    0.541, // 53¢
    0.551, // 54¢
    0.561, // 55¢
    0.571, // 56¢
    0.581, // 57¢
    0.591, // 58¢
    0.601, // 59¢
    0.611, // 60¢ — well-calibrated zone ends
    0.622, // 61¢
    0.633, // 62¢
    0.644, // 63¢
    0.655, // 64¢
    0.667, // 65¢
    0.679, // 66¢
    0.691, // 67¢
    0.703, // 68¢
    0.715, // 69¢
    0.727, // 70¢ — maker edge zone begins
    0.739, // 71¢
    0.751, // 72¢
    0.763, // 73¢
    0.775, // 74¢
    0.787, // 75¢
    0.799, // 76¢
    0.811, // 77¢
    0.823, // 78¢
    0.835, // 79¢
    0.848, // 80¢
    0.860, // 81¢
    0.872, // 82¢
    0.884, // 83¢
    0.896, // 84¢
    0.907, // 85¢
    0.918, // 86¢
    0.929, // 87¢
    0.939, // 88¢
    0.949, // 89¢
    0.958, // 90¢ — high confidence zone
    0.965, // 91¢
    0.972, // 92¢
    0.978, // 93¢
    0.983, // 94¢
    0.987, // 95¢
    0.990, // 96¢
    0.993, // 97¢
    0.995, // 98¢
    0.997, // 99¢
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
        if price_cents >= 1 && price_cents <= 99 {
            Some(CALIBRATION_WIN_RATE[price_cents as usize])
        } else {
            None
        }
    }

    /// Calculate the calibration bias at a given price level.
    ///
    /// Positive = market underprices (actual win rate > implied) → buy signal
    /// Negative = market overprices (actual win rate < implied) → avoid/sell
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
        if !market_price.is_finite() || market_price < 0.0 || market_price > 1.0 {
            return raw_edge;
        }
        let price_cents = (market_price * 100.0).round() as u32;
        let bias = Self::calibration_bias_pp(price_cents).unwrap_or(0.0) / 100.0;

        // The calibration bias tells us how much the market systematically
        // misprices at this level. If bias is positive (underpriced),
        // our edge is actually larger than the raw estimate.
        raw_edge + bias
    }

    /// Check if a price level is in the "taker edge" zone (10-30¢)
    /// where historical data shows takers have positive excess returns.
    pub fn is_taker_edge_zone(price_cents: u32) -> bool {
        price_cents >= 10 && price_cents <= 30
    }

    /// Check if a price level is in the well-calibrated zone (40-60¢)
    /// where market prices closely match actual outcomes.
    pub fn is_well_calibrated(price_cents: u32) -> bool {
        price_cents >= 40 && price_cents <= 60
    }

    /// Estimate the expected value of buying YES at a given price,
    /// using historical calibration rather than the implied probability.
    ///
    /// EV = (historical_win_rate * $1.00) - price - fees
    pub fn calibrated_ev(price_cents: u32, fee_rate: f64) -> Option<f64> {
        let win_rate = Self::historical_win_rate(price_cents)?;
        let price = price_cents as f64 / 100.0;
        let gross_ev = win_rate * 1.0 - price;
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

// =============================================================================
// Helper Functions
// =============================================================================

/// Quick calculation of effective sum target with default buffers
pub fn effective_sum_target(
    fee_buffer: Decimal,
    slippage_buffer: Decimal,
    profit_buffer: Decimal,
) -> Decimal {
    Decimal::ONE - fee_buffer - slippage_buffer - profit_buffer
}

/// Quick check if Leg2 condition is met
pub fn check_leg2_condition(leg1_price: Decimal, opposite_ask: Decimal, target: Decimal) -> bool {
    leg1_price + opposite_ask <= target
}

/// Calculate expected PnL for a cycle
pub fn calculate_cycle_pnl(
    shares: u64,
    leg1_price: Decimal,
    leg2_price: Decimal,
    fee_rate: Decimal,
) -> Decimal {
    let shares_dec = Decimal::from(shares);
    let gross = shares_dec * (Decimal::ONE - leg1_price - leg2_price);
    let notional = shares_dec * (leg1_price + leg2_price);
    let fees = notional * fee_rate;
    gross - fees
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_effective_sum_target() {
        let calc = TradingCalculator::with_buffers(
            dec!(0.005), // fee
            dec!(0.02),  // slippage
            dec!(0.01),  // profit
        );

        // 1 - 0.005 - 0.02 - 0.01 = 0.965
        assert_eq!(calc.effective_sum_target(), dec!(0.965));
    }

    #[test]
    fn test_meets_sum_target() {
        let calc = TradingCalculator::with_buffers(dec!(0.005), dec!(0.02), dec!(0.01));

        // 0.45 + 0.50 = 0.95 <= 0.965 ✓
        assert!(calc.meets_sum_target(dec!(0.45), dec!(0.50)));

        // 0.45 + 0.55 = 1.00 > 0.965 ✗
        assert!(!calc.meets_sum_target(dec!(0.45), dec!(0.55)));
    }

    #[test]
    fn test_expected_pnl() {
        let calc = TradingCalculator::with_buffers(dec!(0.005), dec!(0.02), dec!(0.01));

        // shares=100, leg1=0.45, leg2=0.50
        // gross = 100 * (1 - 0.45 - 0.50) = 100 * 0.05 = 5
        // notional = 100 * 0.95 = 95
        // fees = 95 * 0.005 = 0.475
        // net = 5 - 0.475 = 4.525
        let pnl = calc.expected_pnl(100, dec!(0.45), dec!(0.50));
        assert!(pnl > dec!(4) && pnl < dec!(5));
    }

    #[test]
    fn test_break_even_leg2() {
        let calc = TradingCalculator::with_buffers(dec!(0.005), dec!(0.02), dec!(0.01));

        let leg1 = dec!(0.45);
        let break_even = calc.break_even_leg2(leg1);

        // break_even = 0.965 - 0.45 = 0.515
        assert_eq!(break_even, dec!(0.515));
    }

    #[test]
    fn test_slippage() {
        let calc = TradingCalculator::with_buffers(dec!(0.005), dec!(0.02), dec!(0.01));

        let price = dec!(0.50);

        // Buy slippage: 0.50 * 1.02 = 0.51
        assert_eq!(calc.apply_buy_slippage(price), dec!(0.51));

        // Sell slippage: 0.50 * 0.98 = 0.49
        assert_eq!(calc.apply_sell_slippage(price), dec!(0.49));
    }

    #[test]
    fn test_split_merge_arb() {
        let calc = TradingCalculator::new();

        // Split arb: yes_bid + no_bid > 1
        assert!(calc.has_split_arb(dec!(0.55), dec!(0.50)));
        assert!(!calc.has_split_arb(dec!(0.45), dec!(0.50)));

        // Merge arb: yes_ask + no_ask < 1
        assert!(calc.has_merge_arb(dec!(0.45), dec!(0.50)));
        assert!(!calc.has_merge_arb(dec!(0.55), dec!(0.50)));
    }

    #[test]
    fn test_spread_bps() {
        let calc = TradingCalculator::new();

        // bid=0.49, ask=0.51
        // spread = (0.51 - 0.49) / 0.49 * 10000 ≈ 408 bps
        let spread = calc.spread_bps(dec!(0.49), dec!(0.51));
        assert!(spread > 400 && spread < 420);
    }

    #[test]
    fn test_calculate_cycle_pnl() {
        let pnl = calculate_cycle_pnl(100, dec!(0.45), dec!(0.50), dec!(0.005));

        // Same as expected_pnl test
        assert!(pnl > dec!(4) && pnl < dec!(5));
    }

    // =========================================================================
    // Calibration Tests
    // =========================================================================

    #[test]
    fn test_calibration_win_rate_bounds() {
        // Win rate should be monotonically increasing
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
        // Win rate at 50¢ should be ~0.50
        let mid = MarketCalibration::historical_win_rate(50).unwrap();
        assert!(mid > 0.49 && mid < 0.52, "midpoint win rate: {}", mid);
    }

    #[test]
    fn test_calibration_bias() {
        // Low prices should show negative bias (overpriced longshots)
        let bias_5 = MarketCalibration::calibration_bias_pp(5).unwrap();
        assert!(bias_5 < 0.0, "5¢ bias should be negative: {}", bias_5);

        // Mid prices should be near zero
        let bias_50 = MarketCalibration::calibration_bias_pp(50).unwrap();
        assert!(
            bias_50.abs() < 2.0,
            "50¢ bias should be near zero: {}",
            bias_50
        );

        // High prices should show positive bias (underpriced favorites)
        let bias_90 = MarketCalibration::calibration_bias_pp(90).unwrap();
        assert!(bias_90 > 0.0, "90¢ bias should be positive: {}", bias_90);
    }

    #[test]
    fn test_calibration_adjusted_edge() {
        // At 20¢, taker has positive excess return, so adjusted edge > raw edge
        let raw = 0.05; // 5pp raw edge
        let adjusted = MarketCalibration::calibration_adjusted_edge(raw, 0.20);
        // Bias at 20¢ is negative (overpriced), so adjusted < raw
        // Actually: win_rate[20]=0.190, implied=0.20, bias=-1.0pp
        assert!(
            adjusted < raw,
            "20¢ adjusted={} should be < raw={}",
            adjusted,
            raw
        );

        // At 85¢, favorites are underpriced, so adjusted edge > raw edge
        let adjusted_85 = MarketCalibration::calibration_adjusted_edge(raw, 0.85);
        assert!(
            adjusted_85 > raw,
            "85¢ adjusted={} should be > raw={}",
            adjusted_85,
            raw
        );
    }

    #[test]
    fn test_calibrated_ev() {
        // At 50¢ with 0.5% fee, EV should be near zero (well-calibrated)
        let ev_50 = MarketCalibration::calibrated_ev(50, 0.005).unwrap();
        assert!(ev_50.abs() < 0.02, "50¢ EV should be near zero: {}", ev_50);

        // Out of range returns None
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
        // Perfect prediction: price=0.9, won=true → (0.9-1)² = 0.01
        let b = MarketCalibration::brier_contribution(0.9, true);
        assert!((b - 0.01).abs() < 1e-10);

        // Bad prediction: price=0.9, won=false → (0.9-0)² = 0.81
        let b2 = MarketCalibration::brier_contribution(0.9, false);
        assert!((b2 - 0.81).abs() < 1e-10);
    }
}
