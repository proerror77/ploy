//! Trading Costs Module
//!
//! Comprehensive trading cost calculation for accurate PnL accounting.
//! Includes maker/taker fees, gas costs, and slippage estimation.
//!
//! # CRITICAL FIX
//! Previously, PnL calculations only considered price differences without
//! deducting any trading costs. This led to inflated PnL figures and
//! unrealistic backtesting results.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Trading cost configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingCostConfig {
    /// Maker fee rate (e.g., 0.002 = 0.2%)
    pub maker_fee_rate: Decimal,
    /// Taker fee rate (e.g., 0.002 = 0.2%)
    pub taker_fee_rate: Decimal,
    /// Average gas cost per transaction in USD
    pub gas_cost_usd: Decimal,
    /// Slippage tolerance (e.g., 0.01 = 1%)
    pub slippage_tolerance: Decimal,
}

impl Default for TradingCostConfig {
    fn default() -> Self {
        Self {
            // Polymarket typical fees: ~0.2% maker/taker
            maker_fee_rate: dec!(0.002),
            taker_fee_rate: dec!(0.002),
            // Polygon gas is cheap, typically $0.01-0.05 per tx
            gas_cost_usd: dec!(0.02),
            // Default 1% slippage tolerance
            slippage_tolerance: dec!(0.01),
        }
    }
}

/// Order type for fee calculation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    /// Maker order (adds liquidity)
    Maker,
    /// Taker order (removes liquidity)
    Taker,
}

/// Detailed breakdown of trading costs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingCostBreakdown {
    /// Entry fee (maker or taker)
    pub entry_fee: Decimal,
    /// Exit fee (maker or taker)
    pub exit_fee: Decimal,
    /// Total gas costs (entry + exit)
    pub gas_costs: Decimal,
    /// Estimated slippage cost
    pub slippage_cost: Decimal,
    /// Total trading costs
    pub total_cost: Decimal,
}

impl TradingCostBreakdown {
    /// Create a zero-cost breakdown
    pub fn zero() -> Self {
        Self {
            entry_fee: Decimal::ZERO,
            exit_fee: Decimal::ZERO,
            gas_costs: Decimal::ZERO,
            slippage_cost: Decimal::ZERO,
            total_cost: Decimal::ZERO,
        }
    }
}

/// Trading cost calculator
pub struct TradingCostCalculator {
    config: TradingCostConfig,
}

impl TradingCostCalculator {
    /// Create a new calculator with default config
    pub fn new() -> Self {
        Self {
            config: TradingCostConfig::default(),
        }
    }

    /// Create a calculator with custom config
    pub fn with_config(config: TradingCostConfig) -> Self {
        Self { config }
    }

    /// Calculate entry fee
    ///
    /// # Arguments
    /// * `notional_value` - Position size in USD
    /// * `order_type` - Maker or taker order
    ///
    /// # Returns
    /// Fee amount in USD
    pub fn calculate_entry_fee(&self, notional_value: Decimal, order_type: OrderType) -> Decimal {
        let fee_rate = match order_type {
            OrderType::Maker => self.config.maker_fee_rate,
            OrderType::Taker => self.config.taker_fee_rate,
        };
        notional_value * fee_rate
    }

    /// Calculate exit fee
    ///
    /// # Arguments
    /// * `notional_value` - Position size in USD
    /// * `order_type` - Maker or taker order
    ///
    /// # Returns
    /// Fee amount in USD
    pub fn calculate_exit_fee(&self, notional_value: Decimal, order_type: OrderType) -> Decimal {
        let fee_rate = match order_type {
            OrderType::Maker => self.config.maker_fee_rate,
            OrderType::Taker => self.config.taker_fee_rate,
        };
        notional_value * fee_rate
    }

    /// Calculate gas costs for a round trip (entry + exit)
    ///
    /// # Returns
    /// Total gas cost in USD
    pub fn calculate_gas_costs(&self) -> Decimal {
        // Round trip = 2 transactions
        self.config.gas_cost_usd * dec!(2)
    }

    /// Estimate slippage cost
    ///
    /// # Arguments
    /// * `notional_value` - Position size in USD
    /// * `market_depth_ratio` - Ratio of order size to market depth (0-1)
    ///
    /// # Returns
    /// Estimated slippage cost in USD
    ///
    /// # Notes
    /// Slippage increases non-linearly with order size relative to market depth.
    /// For small orders (<1% of depth), slippage is minimal.
    /// For large orders (>10% of depth), slippage can be significant.
    pub fn estimate_slippage(&self, notional_value: Decimal, market_depth_ratio: Decimal) -> Decimal {
        // Slippage model: quadratic function of depth ratio
        // Small orders: ~0.1% slippage
        // Medium orders (5% depth): ~0.5% slippage
        // Large orders (10% depth): ~1% slippage
        let base_slippage = dec!(0.001); // 0.1% base
        let depth_factor = market_depth_ratio * market_depth_ratio * dec!(1.6);
        let slippage_rate = base_slippage + depth_factor;

        // Cap at configured tolerance
        let capped_rate = slippage_rate.min(self.config.slippage_tolerance);

        notional_value * capped_rate
    }

    /// Calculate complete trading cost breakdown
    ///
    /// # Arguments
    /// * `entry_notional` - Entry position size in USD
    /// * `exit_notional` - Exit position size in USD
    /// * `entry_order_type` - Entry order type (maker/taker)
    /// * `exit_order_type` - Exit order type (maker/taker)
    /// * `market_depth_ratio` - Order size relative to market depth
    ///
    /// # Returns
    /// Detailed cost breakdown
    pub fn calculate_full_costs(
        &self,
        entry_notional: Decimal,
        exit_notional: Decimal,
        entry_order_type: OrderType,
        exit_order_type: OrderType,
        market_depth_ratio: Decimal,
    ) -> TradingCostBreakdown {
        let entry_fee = self.calculate_entry_fee(entry_notional, entry_order_type);
        let exit_fee = self.calculate_exit_fee(exit_notional, exit_order_type);
        let gas_costs = self.calculate_gas_costs();

        // Slippage on both entry and exit
        let entry_slippage = self.estimate_slippage(entry_notional, market_depth_ratio);
        let exit_slippage = self.estimate_slippage(exit_notional, market_depth_ratio);
        let slippage_cost = entry_slippage + exit_slippage;

        let total_cost = entry_fee + exit_fee + gas_costs + slippage_cost;

        TradingCostBreakdown {
            entry_fee,
            exit_fee,
            gas_costs,
            slippage_cost,
            total_cost,
        }
    }

    /// Calculate net PnL after all trading costs
    ///
    /// # Arguments
    /// * `gross_pnl` - Gross PnL (exit_price - entry_price) * shares
    /// * `entry_notional` - Entry position size in USD
    /// * `exit_notional` - Exit position size in USD
    /// * `entry_order_type` - Entry order type
    /// * `exit_order_type` - Exit order type
    /// * `market_depth_ratio` - Order size relative to market depth
    ///
    /// # Returns
    /// Net PnL after deducting all costs
    pub fn calculate_net_pnl(
        &self,
        gross_pnl: Decimal,
        entry_notional: Decimal,
        exit_notional: Decimal,
        entry_order_type: OrderType,
        exit_order_type: OrderType,
        market_depth_ratio: Decimal,
    ) -> Decimal {
        let costs = self.calculate_full_costs(
            entry_notional,
            exit_notional,
            entry_order_type,
            exit_order_type,
            market_depth_ratio,
        );

        gross_pnl - costs.total_cost
    }

    /// Get the configuration
    pub fn config(&self) -> &TradingCostConfig {
        &self.config
    }
}

impl Default for TradingCostCalculator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maker_fee_calculation() {
        let calc = TradingCostCalculator::new();
        let notional = dec!(1000); // $1000 position
        let fee = calc.calculate_entry_fee(notional, OrderType::Maker);

        // 0.2% of $1000 = $2
        assert_eq!(fee, dec!(2));
    }

    #[test]
    fn test_taker_fee_calculation() {
        let calc = TradingCostCalculator::new();
        let notional = dec!(1000);
        let fee = calc.calculate_entry_fee(notional, OrderType::Taker);

        // 0.2% of $1000 = $2
        assert_eq!(fee, dec!(2));
    }

    #[test]
    fn test_gas_costs() {
        let calc = TradingCostCalculator::new();
        let gas = calc.calculate_gas_costs();

        // Default: $0.02 * 2 = $0.04
        assert_eq!(gas, dec!(0.04));
    }

    #[test]
    fn test_slippage_small_order() {
        let calc = TradingCostCalculator::new();
        let notional = dec!(1000);
        let depth_ratio = dec!(0.01); // 1% of market depth

        let slippage = calc.estimate_slippage(notional, depth_ratio);

        // Should be minimal for small orders
        assert!(slippage < dec!(2)); // Less than $2 on $1000
    }

    #[test]
    fn test_slippage_large_order() {
        let calc = TradingCostCalculator::new();
        let notional = dec!(1000);
        let depth_ratio = dec!(0.1); // 10% of market depth

        let slippage = calc.estimate_slippage(notional, depth_ratio);

        // Should be significant for large orders
        assert!(slippage > dec!(5)); // More than $5 on $1000
    }

    #[test]
    fn test_full_cost_breakdown() {
        let calc = TradingCostCalculator::new();
        let entry_notional = dec!(1000);
        let exit_notional = dec!(1050); // Profitable trade
        let depth_ratio = dec!(0.05); // 5% of market depth

        let costs = calc.calculate_full_costs(
            entry_notional,
            exit_notional,
            OrderType::Taker,
            OrderType::Taker,
            depth_ratio,
        );

        // Entry fee: $1000 * 0.002 = $2
        assert_eq!(costs.entry_fee, dec!(2));

        // Exit fee: $1050 * 0.002 = $2.10
        assert_eq!(costs.exit_fee, dec!(2.10));

        // Gas: $0.04
        assert_eq!(costs.gas_costs, dec!(0.04));

        // Total should be sum of all components
        assert_eq!(
            costs.total_cost,
            costs.entry_fee + costs.exit_fee + costs.gas_costs + costs.slippage_cost
        );
    }

    #[test]
    fn test_net_pnl_calculation() {
        let calc = TradingCostCalculator::new();

        // Scenario: Buy 1000 shares @ $0.50, sell @ $0.55
        // Gross PnL: ($0.55 - $0.50) * 1000 = $50
        let gross_pnl = dec!(50);
        let entry_notional = dec!(500); // 1000 * $0.50
        let exit_notional = dec!(550); // 1000 * $0.55
        let depth_ratio = dec!(0.02); // 2% of market depth

        let net_pnl = calc.calculate_net_pnl(
            gross_pnl,
            entry_notional,
            exit_notional,
            OrderType::Taker,
            OrderType::Taker,
            depth_ratio,
        );

        // Net PnL should be less than gross PnL
        assert!(net_pnl < gross_pnl);

        // Net PnL should be positive (profitable trade after costs)
        assert!(net_pnl > Decimal::ZERO);

        // Approximate expected costs:
        // Entry fee: $500 * 0.002 = $1
        // Exit fee: $550 * 0.002 = $1.10
        // Gas: $0.04
        // Slippage: ~$0.50 (small order)
        // Total costs: ~$2.64
        // Net PnL: $50 - $2.64 = ~$47.36
        assert!(net_pnl > dec!(45) && net_pnl < dec!(49));
    }

    #[test]
    fn test_losing_trade_with_costs() {
        let calc = TradingCostCalculator::new();

        // Scenario: Buy 1000 shares @ $0.50, sell @ $0.48 (loss)
        // Gross PnL: ($0.48 - $0.50) * 1000 = -$20
        let gross_pnl = dec!(-20);
        let entry_notional = dec!(500);
        let exit_notional = dec!(480);
        let depth_ratio = dec!(0.02);

        let net_pnl = calc.calculate_net_pnl(
            gross_pnl,
            entry_notional,
            exit_notional,
            OrderType::Taker,
            OrderType::Taker,
            depth_ratio,
        );

        // Net PnL should be more negative than gross PnL
        assert!(net_pnl < gross_pnl);

        // Should be approximately -$22 to -$23 after costs
        assert!(net_pnl < dec!(-22) && net_pnl > dec!(-24));
    }

    #[test]
    fn test_custom_config() {
        let config = TradingCostConfig {
            maker_fee_rate: dec!(0.001), // 0.1%
            taker_fee_rate: dec!(0.003), // 0.3%
            gas_cost_usd: dec!(0.05),
            slippage_tolerance: dec!(0.02), // 2%
        };

        let calc = TradingCostCalculator::with_config(config);

        let maker_fee = calc.calculate_entry_fee(dec!(1000), OrderType::Maker);
        let taker_fee = calc.calculate_entry_fee(dec!(1000), OrderType::Taker);

        assert_eq!(maker_fee, dec!(1)); // 0.1% of $1000
        assert_eq!(taker_fee, dec!(3)); // 0.3% of $1000
    }
}
