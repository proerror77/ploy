//! Realistic Execution Simulator for Backtesting
//!
//! Provides realistic order execution simulation including:
//! - Partial fills based on market depth
//! - Realistic fill timing (not instant)
//! - Bid-ask spread impact
//! - Market impact on large orders
//!
//! # CRITICAL FIX
//! Previously, backtesting assumed instant full fills at signal price,
//! leading to unrealistically optimistic results. This simulator models
//! real-world execution constraints.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc, Duration};

/// Execution simulation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSimConfig {
    /// Use bid-ask spread (true) or mid price (false)
    pub use_spread: bool,
    /// Spread width as percentage of mid (e.g., 0.02 = 2%)
    pub spread_pct: Decimal,
    /// Enable partial fills based on market depth
    pub enable_partial_fills: bool,
    /// Market depth as multiple of typical order size
    pub depth_multiple: Decimal,
    /// Minimum fill percentage (e.g., 0.5 = 50% minimum)
    pub min_fill_pct: Decimal,
    /// Enable realistic fill timing
    pub enable_fill_delay: bool,
    /// Average fill delay in seconds
    pub avg_fill_delay_secs: u64,
    /// Enable market impact modeling
    pub enable_market_impact: bool,
    /// Market impact coefficient (price moves by this * order_size/depth)
    pub impact_coefficient: Decimal,
}

impl Default for ExecutionSimConfig {
    fn default() -> Self {
        Self {
            use_spread: true,
            spread_pct: dec!(0.02), // 2% spread
            enable_partial_fills: true,
            depth_multiple: dec!(5.0), // 5x typical order
            min_fill_pct: dec!(0.5), // At least 50% fill
            enable_fill_delay: true,
            avg_fill_delay_secs: 5, // 5 second average delay
            enable_market_impact: true,
            impact_coefficient: dec!(0.1), // 10% impact per depth ratio
        }
    }
}

/// Execution result
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Actual fill price (may differ from signal price)
    pub fill_price: Decimal,
    /// Number of shares filled
    pub filled_shares: u64,
    /// Number of shares requested
    pub requested_shares: u64,
    /// Fill percentage (0.0 - 1.0)
    pub fill_pct: Decimal,
    /// Actual fill time (may be delayed from signal time)
    pub fill_time: DateTime<Utc>,
    /// Price slippage from signal price
    pub slippage: Decimal,
    /// Market impact on price
    pub market_impact: Decimal,
    /// Whether this was a partial fill
    pub is_partial: bool,
}

/// Realistic execution simulator
pub struct ExecutionSimulator {
    config: ExecutionSimConfig,
}

impl ExecutionSimulator {
    /// Create a new simulator with default config
    pub fn new() -> Self {
        Self {
            config: ExecutionSimConfig::default(),
        }
    }

    /// Create a simulator with custom config
    pub fn with_config(config: ExecutionSimConfig) -> Self {
        Self { config }
    }

    /// Simulate a buy order execution
    ///
    /// # Arguments
    /// * `signal_price` - Price at which signal was generated (mid price)
    /// * `signal_time` - Time when signal was generated
    /// * `shares` - Number of shares to buy
    /// * `market_depth_shares` - Available market depth in shares
    ///
    /// # Returns
    /// Execution result with realistic fill details
    pub fn simulate_buy(
        &self,
        signal_price: Decimal,
        signal_time: DateTime<Utc>,
        shares: u64,
        market_depth_shares: u64,
    ) -> ExecutionResult {
        // Calculate ask price (buy side)
        let half_spread = if self.config.use_spread {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let ask_price = signal_price + half_spread;

        // Determine fill quantity
        let (filled_shares, is_partial) = if self.config.enable_partial_fills {
            self.calculate_fill_quantity(shares, market_depth_shares)
        } else {
            (shares, false)
        };

        // Calculate market impact
        let market_impact = if self.config.enable_market_impact && market_depth_shares > 0 {
            let depth_ratio = Decimal::from(filled_shares) / Decimal::from(market_depth_shares);
            ask_price * self.config.impact_coefficient * depth_ratio
        } else {
            Decimal::ZERO
        };

        // Final fill price includes spread and market impact
        let fill_price = ask_price + market_impact;

        // Calculate fill time
        let fill_time = if self.config.enable_fill_delay {
            self.calculate_fill_time(signal_time, filled_shares, shares)
        } else {
            signal_time
        };

        // Calculate slippage from signal price
        let slippage = fill_price - signal_price;
        let fill_pct = if shares > 0 {
            Decimal::from(filled_shares) / Decimal::from(shares)
        } else {
            Decimal::ZERO
        };

        ExecutionResult {
            fill_price,
            filled_shares,
            requested_shares: shares,
            fill_pct,
            fill_time,
            slippage,
            market_impact,
            is_partial,
        }
    }

    /// Simulate a sell order execution
    ///
    /// # Arguments
    /// * `signal_price` - Price at which signal was generated (mid price)
    /// * `signal_time` - Time when signal was generated
    /// * `shares` - Number of shares to sell
    /// * `market_depth_shares` - Available market depth in shares
    ///
    /// # Returns
    /// Execution result with realistic fill details
    pub fn simulate_sell(
        &self,
        signal_price: Decimal,
        signal_time: DateTime<Utc>,
        shares: u64,
        market_depth_shares: u64,
    ) -> ExecutionResult {
        // Calculate bid price (sell side)
        let half_spread = if self.config.use_spread {
            signal_price * self.config.spread_pct / dec!(2)
        } else {
            Decimal::ZERO
        };
        let bid_price = signal_price - half_spread;

        // Determine fill quantity
        let (filled_shares, is_partial) = if self.config.enable_partial_fills {
            self.calculate_fill_quantity(shares, market_depth_shares)
        } else {
            (shares, false)
        };

        // Calculate market impact (negative for sells)
        let market_impact = if self.config.enable_market_impact && market_depth_shares > 0 {
            let depth_ratio = Decimal::from(filled_shares) / Decimal::from(market_depth_shares);
            bid_price * self.config.impact_coefficient * depth_ratio
        } else {
            Decimal::ZERO
        };

        // Final fill price includes spread and market impact
        let fill_price = bid_price - market_impact;

        // Calculate fill time
        let fill_time = if self.config.enable_fill_delay {
            self.calculate_fill_time(signal_time, filled_shares, shares)
        } else {
            signal_time
        };

        // Calculate slippage from signal price (negative for sells)
        let slippage = signal_price - fill_price;
        let fill_pct = if shares > 0 {
            Decimal::from(filled_shares) / Decimal::from(shares)
        } else {
            Decimal::ZERO
        };

        ExecutionResult {
            fill_price,
            filled_shares,
            requested_shares: shares,
            fill_pct,
            fill_time,
            slippage,
            market_impact,
            is_partial,
        }
    }

    /// Calculate realistic fill quantity based on market depth
    fn calculate_fill_quantity(&self, requested: u64, depth: u64) -> (u64, bool) {
        if depth == 0 {
            // No liquidity - no fill
            return (0, true);
        }

        // Typical order size for depth calculation
        let typical_order = (depth as f64 / self.config.depth_multiple.to_f64().unwrap_or(5.0)) as u64;
        let typical_order = typical_order.max(100); // At least 100 shares

        if requested <= typical_order {
            // Small order - full fill
            (requested, false)
        } else if requested <= depth {
            // Medium order - partial fill based on depth
            let fill_ratio = (depth as f64 / requested as f64).min(1.0);
            let min_fill = (requested as f64 * self.config.min_fill_pct.to_f64().unwrap_or(0.5)) as u64;
            let filled = ((requested as f64 * fill_ratio) as u64).max(min_fill);
            (filled.min(requested), filled < requested)
        } else {
            // Large order - fill up to depth
            let filled = depth.max((requested as f64 * self.config.min_fill_pct.to_f64().unwrap_or(0.5)) as u64);
            (filled.min(requested), true)
        }
    }

    /// Calculate realistic fill time with delay
    fn calculate_fill_time(
        &self,
        signal_time: DateTime<Utc>,
        filled: u64,
        requested: u64,
    ) -> DateTime<Utc> {
        // Base delay
        let base_delay = self.config.avg_fill_delay_secs as i64;

        // Additional delay for partial fills (need to wait for liquidity)
        let partial_delay = if filled < requested {
            base_delay * 2 // Double delay for partial fills
        } else {
            0
        };

        // Add some randomness (±50%)
        let total_delay = base_delay + partial_delay;
        let jitter = (total_delay as f64 * 0.5) as i64;
        let final_delay = total_delay - jitter / 2; // Simplified: no real randomness in backtest

        signal_time + Duration::seconds(final_delay)
    }

    /// Get configuration
    pub fn config(&self) -> &ExecutionSimConfig {
        &self.config
    }
}

impl Default for ExecutionSimulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_buy_with_spread() {
        let mut config = ExecutionSimConfig::default();
        config.enable_market_impact = false;
        let sim = ExecutionSimulator::with_config(config);
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        let result = sim.simulate_buy(signal_price, signal_time, 100, 1000);

        // Should pay ask price (mid + half spread)
        // Spread is 2%, so half spread is 1%
        assert!(result.fill_price > signal_price);
        assert!(result.fill_price <= signal_price * dec!(1.02)); // Max 2% above mid

        // Small order should fill completely
        assert_eq!(result.filled_shares, 100);
        assert!(!result.is_partial);
    }

    #[test]
    fn test_sell_with_spread() {
        let mut config = ExecutionSimConfig::default();
        config.enable_market_impact = false;
        let sim = ExecutionSimulator::with_config(config);
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        let result = sim.simulate_sell(signal_price, signal_time, 100, 1000);

        // Should receive bid price (mid - half spread)
        assert!(result.fill_price < signal_price);
        assert!(result.fill_price >= signal_price * dec!(0.98)); // Max 2% below mid

        // Small order should fill completely
        assert_eq!(result.filled_shares, 100);
        assert!(!result.is_partial);
    }

    #[test]
    fn test_partial_fill_large_order() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        // Order larger than depth
        let result = sim.simulate_buy(signal_price, signal_time, 2000, 1000);

        // Should be partial fill
        assert!(result.is_partial);
        assert!(result.filled_shares < 2000);
        assert!(result.filled_shares >= 1000); // At least 50% (min_fill_pct)
    }

    #[test]
    fn test_market_impact() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        // Large order relative to depth
        let result = sim.simulate_buy(signal_price, signal_time, 500, 1000);

        // Should have market impact
        assert!(result.market_impact > Decimal::ZERO);

        // Fill price should include impact
        let expected_min = signal_price * dec!(1.01); // At least spread
        assert!(result.fill_price >= expected_min);
    }

    #[test]
    fn test_fill_delay() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        let result = sim.simulate_buy(signal_price, signal_time, 100, 1000);

        // Fill time should be after signal time
        assert!(result.fill_time >= signal_time);

        // Should have some delay (at least 1 second)
        let delay = (result.fill_time - signal_time).num_seconds();
        assert!(delay >= 1);
    }

    #[test]
    fn test_no_liquidity() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        // No market depth
        let result = sim.simulate_buy(signal_price, signal_time, 100, 0);

        // Should not fill
        assert_eq!(result.filled_shares, 0);
        assert!(result.is_partial);
    }

    #[test]
    fn test_disabled_features() {
        let config = ExecutionSimConfig {
            use_spread: false,
            enable_partial_fills: false,
            enable_fill_delay: false,
            enable_market_impact: false,
            ..Default::default()
        };

        let sim = ExecutionSimulator::with_config(config);
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        let result = sim.simulate_buy(signal_price, signal_time, 100, 50);

        // Should fill at signal price (no spread, no impact)
        assert_eq!(result.fill_price, signal_price);

        // Should fill completely (no partial fills)
        assert_eq!(result.filled_shares, 100);

        // Should fill instantly (no delay)
        assert_eq!(result.fill_time, signal_time);

        // No market impact
        assert_eq!(result.market_impact, Decimal::ZERO);
    }

    #[test]
    fn test_slippage_calculation() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        let result = sim.simulate_buy(signal_price, signal_time, 100, 1000);

        // Slippage should be positive for buys (paying more than mid)
        assert!(result.slippage > Decimal::ZERO);

        // Slippage should equal fill_price - signal_price
        assert_eq!(result.slippage, result.fill_price - signal_price);
    }

    #[test]
    fn test_fill_percentage() {
        let sim = ExecutionSimulator::new();
        let signal_price = dec!(0.50);
        let signal_time = Utc::now();

        // Partial fill scenario
        let result = sim.simulate_buy(signal_price, signal_time, 1000, 500);

        // Fill percentage should be correct
        let expected_pct = Decimal::from(result.filled_shares) / dec!(1000);
        assert_eq!(result.fill_pct, expected_pct);

        // Should be less than 100%
        assert!(result.fill_pct < Decimal::ONE);
        assert!(result.fill_pct >= dec!(0.5)); // At least min_fill_pct
    }
}
