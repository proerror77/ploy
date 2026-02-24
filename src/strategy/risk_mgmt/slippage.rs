//! Slippage Protection Module
//!
//! Provides slippage estimation and protection mechanisms for order execution.
//! Prevents trades from executing at unfavorable prices due to market impact.
//!
//! # CRITICAL FIX
//! Previously, orders were submitted without any slippage protection, allowing
//! trades to execute at any price. This could result in significant losses,
//! especially for large orders or in illiquid markets.

use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Slippage protection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageConfig {
    /// Maximum allowed slippage as a percentage (e.g., 0.01 = 1%)
    pub max_slippage_pct: Decimal,
    /// Enable dynamic slippage based on market conditions
    pub dynamic_slippage: bool,
    /// Minimum market depth required (as multiple of order size)
    pub min_depth_multiple: Decimal,
}

impl Default for SlippageConfig {
    fn default() -> Self {
        Self {
            // Default 1% maximum slippage
            max_slippage_pct: dec!(0.01),
            // Enable dynamic slippage by default
            dynamic_slippage: true,
            // Require at least 5x order size in market depth
            min_depth_multiple: dec!(5.0),
        }
    }
}

/// Market depth information
#[derive(Debug, Clone)]
pub struct MarketDepth {
    /// Best bid price
    pub best_bid: Decimal,
    /// Best ask price
    pub best_ask: Decimal,
    /// Total bid size (shares)
    pub bid_size: Decimal,
    /// Total ask size (shares)
    pub ask_size: Decimal,
}

impl MarketDepth {
    /// Calculate bid-ask spread
    pub fn spread(&self) -> Decimal {
        self.best_ask - self.best_bid
    }

    /// Calculate spread as percentage of mid price
    pub fn spread_pct(&self) -> Decimal {
        let mid = (self.best_bid + self.best_ask) / dec!(2);
        if mid > Decimal::ZERO {
            self.spread() / mid
        } else {
            Decimal::ZERO
        }
    }

    /// Calculate mid price
    pub fn mid_price(&self) -> Decimal {
        (self.best_bid + self.best_ask) / dec!(2)
    }
}

/// Slippage protection result
#[derive(Debug, Clone)]
pub enum SlippageCheck {
    /// Order can proceed with given limit price
    Approved {
        /// Recommended limit price
        limit_price: Decimal,
        /// Estimated slippage percentage
        estimated_slippage_pct: Decimal,
    },
    /// Order rejected due to excessive slippage
    Rejected {
        /// Reason for rejection
        reason: String,
        /// Estimated slippage percentage
        estimated_slippage_pct: Decimal,
    },
}

/// Slippage protection engine
pub struct SlippageProtection {
    config: SlippageConfig,
}

impl SlippageProtection {
    /// Create a new slippage protection engine
    pub fn new(config: SlippageConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration
    pub fn default() -> Self {
        Self::new(SlippageConfig::default())
    }

    /// Check if a buy order is acceptable
    ///
    /// # Arguments
    /// * `market_depth` - Current market depth
    /// * `order_size` - Number of shares to buy
    /// * `reference_price` - Reference price for slippage calculation (e.g., signal price)
    ///
    /// # Returns
    /// SlippageCheck result with recommended limit price or rejection reason
    pub fn check_buy_order(
        &self,
        market_depth: &MarketDepth,
        order_size: Decimal,
        reference_price: Decimal,
    ) -> SlippageCheck {
        // For buy orders, we care about the ask side
        let best_ask = market_depth.best_ask;
        let ask_size = market_depth.ask_size;

        // Check market depth
        if ask_size < order_size * self.config.min_depth_multiple {
            return SlippageCheck::Rejected {
                reason: format!(
                    "Insufficient market depth: {} shares available, need {} ({}x order size)",
                    ask_size,
                    order_size * self.config.min_depth_multiple,
                    self.config.min_depth_multiple
                ),
                estimated_slippage_pct: Decimal::MAX,
            };
        }

        // Calculate slippage from reference price (clamp to zero to prevent negative bypass)
        let slippage = best_ask - reference_price;
        let slippage_pct = if reference_price > Decimal::ZERO {
            (slippage / reference_price).max(Decimal::ZERO)
        } else {
            Decimal::ZERO
        };

        // Check if slippage exceeds maximum
        if slippage_pct > self.config.max_slippage_pct {
            return SlippageCheck::Rejected {
                reason: format!(
                    "Slippage too high: {:.2}% (max: {:.2}%)",
                    slippage_pct * dec!(100),
                    self.config.max_slippage_pct * dec!(100)
                ),
                estimated_slippage_pct: slippage_pct,
            };
        }

        // Calculate limit price with buffer
        // Add 0.1% buffer to ensure fill
        let limit_price = best_ask * (dec!(1) + dec!(0.001));

        SlippageCheck::Approved {
            limit_price,
            estimated_slippage_pct: slippage_pct,
        }
    }

    /// Check if a sell order is acceptable
    ///
    /// # Arguments
    /// * `market_depth` - Current market depth
    /// * `order_size` - Number of shares to sell
    /// * `reference_price` - Reference price for slippage calculation
    ///
    /// # Returns
    /// SlippageCheck result with recommended limit price or rejection reason
    pub fn check_sell_order(
        &self,
        market_depth: &MarketDepth,
        order_size: Decimal,
        reference_price: Decimal,
    ) -> SlippageCheck {
        // For sell orders, we care about the bid side
        let best_bid = market_depth.best_bid;
        let bid_size = market_depth.bid_size;

        // Check market depth
        if bid_size < order_size * self.config.min_depth_multiple {
            return SlippageCheck::Rejected {
                reason: format!(
                    "Insufficient market depth: {} shares available, need {} ({}x order size)",
                    bid_size,
                    order_size * self.config.min_depth_multiple,
                    self.config.min_depth_multiple
                ),
                estimated_slippage_pct: Decimal::MAX,
            };
        }

        // Calculate slippage from reference price (clamp to zero to prevent negative bypass)
        let slippage = reference_price - best_bid;
        let slippage_pct = if reference_price > Decimal::ZERO {
            (slippage / reference_price).max(Decimal::ZERO)
        } else {
            Decimal::ZERO
        };

        // Check if slippage exceeds maximum
        if slippage_pct > self.config.max_slippage_pct {
            return SlippageCheck::Rejected {
                reason: format!(
                    "Slippage too high: {:.2}% (max: {:.2}%)",
                    slippage_pct * dec!(100),
                    self.config.max_slippage_pct * dec!(100)
                ),
                estimated_slippage_pct: slippage_pct,
            };
        }

        // Calculate limit price with buffer
        // Subtract 0.1% buffer to ensure fill
        let limit_price = best_bid * (dec!(1) - dec!(0.001));

        SlippageCheck::Approved {
            limit_price,
            estimated_slippage_pct: slippage_pct,
        }
    }

    /// Estimate slippage for a given order size
    ///
    /// # Arguments
    /// * `market_depth` - Current market depth
    /// * `order_size` - Number of shares
    /// * `is_buy` - True for buy orders, false for sell orders
    ///
    /// # Returns
    /// Estimated slippage as percentage
    pub fn estimate_slippage(
        &self,
        market_depth: &MarketDepth,
        order_size: Decimal,
        is_buy: bool,
    ) -> Decimal {
        let (_price, depth) = if is_buy {
            (market_depth.best_ask, market_depth.ask_size)
        } else {
            (market_depth.best_bid, market_depth.bid_size)
        };

        if depth <= Decimal::ZERO {
            return Decimal::MAX;
        }

        // Simple linear model: slippage increases with order size / depth ratio
        let depth_ratio = order_size / depth;

        // Base slippage from spread
        let spread_slippage = market_depth.spread_pct() / dec!(2);

        // Impact slippage (quadratic in depth ratio)
        let impact_slippage = depth_ratio * depth_ratio * dec!(0.1);

        spread_slippage + impact_slippage
    }

    /// Validate order against slippage limits
    ///
    /// # Arguments
    /// * `market_depth` - Current market depth
    /// * `order_size` - Number of shares
    /// * `is_buy` - True for buy orders, false for sell orders
    /// * `reference_price` - Reference price for slippage calculation
    ///
    /// # Returns
    /// Ok(limit_price) if order is acceptable, Err otherwise
    pub fn validate_order(
        &self,
        market_depth: &MarketDepth,
        order_size: Decimal,
        is_buy: bool,
        reference_price: Decimal,
    ) -> Result<Decimal> {
        let check = if is_buy {
            self.check_buy_order(market_depth, order_size, reference_price)
        } else {
            self.check_sell_order(market_depth, order_size, reference_price)
        };

        match check {
            SlippageCheck::Approved { limit_price, .. } => Ok(limit_price),
            SlippageCheck::Rejected { reason, .. } => Err(PloyError::Validation(format!(
                "Slippage check failed: {}",
                reason
            ))),
        }
    }

    /// Get configuration
    pub fn config(&self) -> &SlippageConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_depth() -> MarketDepth {
        MarketDepth {
            best_bid: dec!(0.48),
            best_ask: dec!(0.52),
            bid_size: dec!(1000),
            ask_size: dec!(1000),
        }
    }

    #[test]
    fn test_market_depth_calculations() {
        let depth = create_test_depth();

        assert_eq!(depth.spread(), dec!(0.04));
        assert_eq!(depth.mid_price(), dec!(0.50));

        // Spread is 8% of mid price (0.04 / 0.50 = 0.08)
        let spread_pct = depth.spread_pct();
        assert!(spread_pct > dec!(0.079) && spread_pct < dec!(0.081));
    }

    #[test]
    fn test_buy_order_approved() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Small order with acceptable slippage (reference price close to ask)
        let check = protection.check_buy_order(&depth, dec!(100), dec!(0.515));

        match check {
            SlippageCheck::Approved {
                limit_price,
                estimated_slippage_pct,
            } => {
                // Limit price should be slightly above best ask
                assert!(limit_price >= depth.best_ask);
                assert!(limit_price < depth.best_ask * dec!(1.01));

                // Slippage should be small (less than 1%)
                assert!(estimated_slippage_pct < dec!(0.01));
            }
            SlippageCheck::Rejected { reason, .. } => {
                panic!("Order should be approved: {}", reason)
            }
        }
    }

    #[test]
    fn test_buy_order_rejected_high_slippage() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Reference price much lower than ask - high slippage
        let check = protection.check_buy_order(&depth, dec!(100), dec!(0.40));

        match check {
            SlippageCheck::Approved { .. } => panic!("Order should be rejected"),
            SlippageCheck::Rejected {
                reason,
                estimated_slippage_pct,
            } => {
                assert!(reason.contains("Slippage too high"));
                assert!(estimated_slippage_pct > dec!(0.01)); // More than 1%
            }
        }
    }

    #[test]
    fn test_buy_order_rejected_insufficient_depth() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Order size too large relative to market depth
        let check = protection.check_buy_order(&depth, dec!(500), dec!(0.51));

        match check {
            SlippageCheck::Approved { .. } => panic!("Order should be rejected"),
            SlippageCheck::Rejected { reason, .. } => {
                assert!(reason.contains("Insufficient market depth"));
            }
        }
    }

    #[test]
    fn test_sell_order_approved() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Small order with acceptable slippage (reference price close to bid)
        let check = protection.check_sell_order(&depth, dec!(100), dec!(0.483));

        match check {
            SlippageCheck::Approved {
                limit_price,
                estimated_slippage_pct,
            } => {
                // Limit price should be slightly below best bid
                assert!(limit_price <= depth.best_bid);
                assert!(limit_price > depth.best_bid * dec!(0.99));

                // Slippage should be small (less than 1%)
                assert!(estimated_slippage_pct < dec!(0.01));
            }
            SlippageCheck::Rejected { reason, .. } => {
                panic!("Order should be approved: {}", reason)
            }
        }
    }

    #[test]
    fn test_sell_order_rejected_high_slippage() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Reference price much higher than bid - high slippage
        let check = protection.check_sell_order(&depth, dec!(100), dec!(0.60));

        match check {
            SlippageCheck::Approved { .. } => panic!("Order should be rejected"),
            SlippageCheck::Rejected {
                reason,
                estimated_slippage_pct,
            } => {
                assert!(reason.contains("Slippage too high"));
                assert!(estimated_slippage_pct > dec!(0.01)); // More than 1%
            }
        }
    }

    #[test]
    fn test_slippage_estimation() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // Small order - low slippage
        let slippage_small = protection.estimate_slippage(&depth, dec!(50), true);
        assert!(slippage_small < dec!(0.05)); // Less than 5%

        // Large order - higher slippage
        let slippage_large = protection.estimate_slippage(&depth, dec!(500), true);
        assert!(slippage_large > slippage_small);
    }

    #[test]
    fn test_validate_order_success() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        let result = protection.validate_order(&depth, dec!(100), true, dec!(0.515));
        assert!(result.is_ok());

        let limit_price = result.unwrap();
        assert!(limit_price >= depth.best_ask);
    }

    #[test]
    fn test_validate_order_failure() {
        let protection = SlippageProtection::default();
        let depth = create_test_depth();

        // High slippage should fail
        let result = protection.validate_order(&depth, dec!(100), true, dec!(0.40));
        assert!(result.is_err());
    }

    #[test]
    fn test_custom_config() {
        let config = SlippageConfig {
            max_slippage_pct: dec!(0.05), // 5% max slippage
            dynamic_slippage: true,
            min_depth_multiple: dec!(3.0), // 3x order size
        };

        let protection = SlippageProtection::new(config);
        let depth = create_test_depth();

        // This would be rejected with default config but approved with 5% tolerance
        // Reference price 0.47 vs ask 0.52 = 10.6% slippage, still too high
        // Let's use 0.50 vs 0.52 = 4% slippage, which should pass
        let check = protection.check_buy_order(&depth, dec!(100), dec!(0.50));

        match check {
            SlippageCheck::Approved { .. } => {
                // Should be approved with higher tolerance
            }
            SlippageCheck::Rejected { reason, .. } => {
                panic!("Should be approved with 5% tolerance: {}", reason)
            }
        }
    }
}
