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
            fee_buffer: Decimal::new(5, 3),     // 0.005 = 0.5%
            slippage_buffer: Decimal::new(2, 2), // 0.02 = 2%
            profit_buffer: Decimal::new(1, 2),   // 0.01 = 1%
            base_sum_target: Decimal::ONE,
        }
    }

    /// Create calculator with custom buffers
    pub fn with_buffers(
        fee: Decimal,
        slippage: Decimal,
        profit: Decimal,
    ) -> Self {
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
    pub fn expected_pnl(
        &self,
        shares: u64,
        leg1_price: Decimal,
        leg2_price: Decimal,
    ) -> Decimal {
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
pub fn check_leg2_condition(
    leg1_price: Decimal,
    opposite_ask: Decimal,
    target: Decimal,
) -> bool {
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
        let calc = TradingCalculator::with_buffers(
            dec!(0.005),
            dec!(0.02),
            dec!(0.01),
        );

        // 0.45 + 0.50 = 0.95 <= 0.965 ✓
        assert!(calc.meets_sum_target(dec!(0.45), dec!(0.50)));

        // 0.45 + 0.55 = 1.00 > 0.965 ✗
        assert!(!calc.meets_sum_target(dec!(0.45), dec!(0.55)));
    }

    #[test]
    fn test_expected_pnl() {
        let calc = TradingCalculator::with_buffers(
            dec!(0.005),
            dec!(0.02),
            dec!(0.01),
        );

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
        let calc = TradingCalculator::with_buffers(
            dec!(0.005),
            dec!(0.02),
            dec!(0.01),
        );

        let leg1 = dec!(0.45);
        let break_even = calc.break_even_leg2(leg1);

        // break_even = 0.965 - 0.45 = 0.515
        assert_eq!(break_even, dec!(0.515));
    }

    #[test]
    fn test_slippage() {
        let calc = TradingCalculator::with_buffers(
            dec!(0.005),
            dec!(0.02),
            dec!(0.01),
        );

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
        let pnl = calculate_cycle_pnl(
            100,
            dec!(0.45),
            dec!(0.50),
            dec!(0.005),
        );

        // Same as expected_pnl test
        assert!(pnl > dec!(4) && pnl < dec!(5));
    }
}
