//! Market Microstructure Filters
//!
//! Defensive filters to avoid trading in poor market conditions.
//! These filters protect against:
//! - Wide spreads (high transaction costs)
//! - Thin liquidity (can't exit position)
//! - Fast price movements (chasing / information cascade)
//! - Stale data (latency issues)
//!
//! Philosophy: These are NOT alpha sources. They are risk controls.
//! Even if the model predicts high edge, we don't trade if market structure is poor.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Market microstructure filters
pub struct MarketFilters {
    config: FilterConfig,
}

/// Filter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    // Spread filters
    pub max_spread_bps: i32, // Maximum allowed spread in basis points (e.g., 200 = 2%)
    pub max_spread_absolute: Decimal, // Maximum absolute spread (e.g., 0.05)

    // Depth filters
    pub min_book_depth_usd: Decimal, // Minimum total book depth in USD (e.g., 1000)
    pub min_best_depth_usd: Decimal, // Minimum depth at best price (e.g., 100)

    // Price velocity filters
    pub max_price_velocity: f64, // Max price change per second (e.g., 0.01 = 1%/sec)
    pub velocity_window_secs: u64, // Window for velocity calculation (e.g., 10 seconds)

    // Data quality filters
    pub max_data_latency_ms: u64, // Maximum acceptable data latency (e.g., 2000ms)
    pub max_quote_age_secs: u64,  // Maximum quote age (e.g., 5 seconds)

    // Order flow filters
    pub max_consecutive_same_side: usize, // Max consecutive trades in same direction (e.g., 5)
    pub min_trade_count: usize,           // Minimum trades in recent window (e.g., 3)

    // Imbalance filters
    pub max_depth_imbalance: f64, // Max bid/ask imbalance (e.g., 0.8 = 80% on one side)
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            // Conservative defaults for MVP
            max_spread_bps: 200,                       // 2% max spread
            max_spread_absolute: Decimal::new(5, 2),   // 0.05 absolute
            min_book_depth_usd: Decimal::new(1000, 0), // $1000 total depth
            min_best_depth_usd: Decimal::new(100, 0),  // $100 at best
            max_price_velocity: 0.01,                  // 1% per second
            velocity_window_secs: 10,
            max_data_latency_ms: 2000, // 2 seconds
            max_quote_age_secs: 5,
            max_consecutive_same_side: 5,
            min_trade_count: 3,
            max_depth_imbalance: 0.8, // 80/20 max
        }
    }
}

/// Market context for filter evaluation
#[derive(Debug, Clone)]
pub struct MarketContext {
    // Spread data
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub mid_price: Option<Decimal>,
    pub spread_bps: Option<i32>,

    // Depth data
    pub bid_depth: Decimal,
    pub ask_depth: Decimal,
    pub best_bid_depth: Decimal,
    pub best_ask_depth: Decimal,

    // Price velocity
    pub price_velocity: Option<f64>,        // Price change per second
    pub recent_prices: Vec<(i64, Decimal)>, // (timestamp_ms, price)

    // Data quality
    pub data_latency_ms: u64,
    pub quote_age_secs: u64,
    pub last_update_timestamp: i64,

    // Order flow
    pub consecutive_same_side_trades: usize,
    pub last_trade_side: String, // "buy" or "sell"
    pub recent_trade_count: usize,

    // Depth imbalance
    pub depth_imbalance: f64, // (bid - ask) / (bid + ask), range [-1, 1]
}

/// Filter evaluation result
#[derive(Debug, Clone)]
pub struct FilterResult {
    pub passed: bool,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
}

impl FilterResult {
    pub fn pass() -> Self {
        Self {
            passed: true,
            reasons: vec![],
            warnings: vec![],
        }
    }

    pub fn fail(reason: String) -> Self {
        Self {
            passed: false,
            reasons: vec![reason],
            warnings: vec![],
        }
    }

    pub fn add_reason(&mut self, reason: String) {
        self.passed = false;
        self.reasons.push(reason);
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }
}

impl MarketFilters {
    pub fn new(config: FilterConfig) -> Self {
        Self { config }
    }

    /// Check if market conditions allow entry
    pub fn can_enter(&self, context: &MarketContext) -> FilterResult {
        let mut result = FilterResult::pass();

        // 1. Spread checks
        self.check_spread(context, &mut result);

        // 2. Depth checks
        self.check_depth(context, &mut result);

        // 3. Price velocity checks
        self.check_price_velocity(context, &mut result);

        // 4. Data quality checks
        self.check_data_quality(context, &mut result);

        // 5. Order flow checks
        self.check_order_flow(context, &mut result);

        // 6. Depth imbalance checks
        self.check_depth_imbalance(context, &mut result);

        result
    }

    /// Check spread conditions
    fn check_spread(&self, context: &MarketContext, result: &mut FilterResult) {
        // Check if we have spread data
        if context.best_bid.is_none() || context.best_ask.is_none() {
            result.add_reason("Missing bid/ask data".to_string());
            return;
        }

        let bid = context.best_bid.unwrap();
        let ask = context.best_ask.unwrap();
        let spread = ask - bid;

        // Check absolute spread
        if spread > self.config.max_spread_absolute {
            result.add_reason(format!(
                "Spread too wide: {:.4} > {:.4}",
                spread, self.config.max_spread_absolute
            ));
        }

        // Check spread in basis points
        if let Some(spread_bps) = context.spread_bps {
            if spread_bps > self.config.max_spread_bps {
                result.add_reason(format!(
                    "Spread too wide: {} bps > {} bps",
                    spread_bps, self.config.max_spread_bps
                ));
            }
        }

        // Warning for wide but acceptable spread
        if let Some(spread_bps) = context.spread_bps {
            if spread_bps > self.config.max_spread_bps / 2 {
                result.add_warning(format!(
                    "Wide spread: {} bps (threshold: {} bps)",
                    spread_bps, self.config.max_spread_bps
                ));
            }
        }
    }

    /// Check depth conditions
    fn check_depth(&self, context: &MarketContext, result: &mut FilterResult) {
        let total_depth = context.bid_depth + context.ask_depth;

        // Check total depth
        if total_depth < self.config.min_book_depth_usd {
            result.add_reason(format!(
                "Insufficient total depth: ${:.2} < ${:.2}",
                total_depth, self.config.min_book_depth_usd
            ));
        }

        // Check best price depth
        let best_depth = context.best_bid_depth + context.best_ask_depth;
        if best_depth < self.config.min_best_depth_usd {
            result.add_reason(format!(
                "Insufficient depth at best: ${:.2} < ${:.2}",
                best_depth, self.config.min_best_depth_usd
            ));
        }

        // Warning for low but acceptable depth
        if total_depth < self.config.min_book_depth_usd * Decimal::from(2) {
            result.add_warning(format!(
                "Low liquidity: ${:.2} (threshold: ${:.2})",
                total_depth, self.config.min_book_depth_usd
            ));
        }
    }

    /// Check price velocity (prevent chasing)
    fn check_price_velocity(&self, context: &MarketContext, result: &mut FilterResult) {
        if let Some(velocity) = context.price_velocity {
            if velocity.abs() > self.config.max_price_velocity {
                result.add_reason(format!(
                    "Price moving too fast: {:.4}/sec > {:.4}/sec",
                    velocity, self.config.max_price_velocity
                ));
            }

            // Warning for fast but acceptable movement
            if velocity.abs() > self.config.max_price_velocity / 2.0 {
                result.add_warning(format!("Fast price movement: {:.4}/sec", velocity));
            }
        }
    }

    /// Check data quality (latency, staleness)
    fn check_data_quality(&self, context: &MarketContext, result: &mut FilterResult) {
        // Check data latency
        if context.data_latency_ms > self.config.max_data_latency_ms {
            result.add_reason(format!(
                "Data latency too high: {}ms > {}ms",
                context.data_latency_ms, self.config.max_data_latency_ms
            ));
        }

        // Check quote age
        if context.quote_age_secs > self.config.max_quote_age_secs {
            result.add_reason(format!(
                "Quote too stale: {}s > {}s",
                context.quote_age_secs, self.config.max_quote_age_secs
            ));
        }

        // Warning for elevated latency
        if context.data_latency_ms > self.config.max_data_latency_ms / 2 {
            result.add_warning(format!("Elevated latency: {}ms", context.data_latency_ms));
        }
    }

    /// Check order flow (detect information cascades)
    fn check_order_flow(&self, context: &MarketContext, result: &mut FilterResult) {
        // Check consecutive same-side trades
        if context.consecutive_same_side_trades > self.config.max_consecutive_same_side {
            result.add_reason(format!(
                "Potential information cascade: {} consecutive {} trades",
                context.consecutive_same_side_trades, context.last_trade_side
            ));
        }

        // Check minimum trade activity
        if context.recent_trade_count < self.config.min_trade_count {
            result.add_warning(format!(
                "Low trade activity: {} trades (min: {})",
                context.recent_trade_count, self.config.min_trade_count
            ));
        }

        // Warning for elevated consecutive trades
        if context.consecutive_same_side_trades > self.config.max_consecutive_same_side / 2 {
            result.add_warning(format!(
                "Elevated one-sided flow: {} consecutive {} trades",
                context.consecutive_same_side_trades, context.last_trade_side
            ));
        }
    }

    /// Check depth imbalance
    fn check_depth_imbalance(&self, context: &MarketContext, result: &mut FilterResult) {
        let imbalance = context.depth_imbalance.abs();

        if imbalance > self.config.max_depth_imbalance {
            result.add_reason(format!(
                "Extreme depth imbalance: {:.1}% (max: {:.1}%)",
                imbalance * 100.0,
                self.config.max_depth_imbalance * 100.0
            ));
        }

        // Warning for moderate imbalance
        if imbalance > self.config.max_depth_imbalance / 2.0 {
            result.add_warning(format!("Depth imbalance: {:.1}%", imbalance * 100.0));
        }
    }

    /// Calculate price velocity from recent prices
    pub fn calculate_price_velocity(
        recent_prices: &[(i64, Decimal)],
        window_secs: u64,
    ) -> Option<f64> {
        if recent_prices.len() < 2 {
            return None;
        }

        let now = recent_prices.last()?.0;
        let cutoff = now - (window_secs as i64 * 1000);

        // Filter prices within window
        let window_prices: Vec<_> = recent_prices
            .iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .collect();

        if window_prices.len() < 2 {
            return None;
        }

        let first = window_prices.first()?;
        let last = window_prices.last()?;

        let price_change = (last.1 - first.1).to_f64()?;
        let time_diff_secs = (last.0 - first.0) as f64 / 1000.0;

        if time_diff_secs < 0.1 {
            return None;
        }

        let velocity = price_change / time_diff_secs;
        Some(velocity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_good_context() -> MarketContext {
        MarketContext {
            best_bid: Some(Decimal::new(45, 2)),
            best_ask: Some(Decimal::new(46, 2)),
            mid_price: Some(Decimal::new(455, 3)),
            spread_bps: Some(22), // 0.22%
            bid_depth: Decimal::new(2000, 0),
            ask_depth: Decimal::new(1800, 0),
            best_bid_depth: Decimal::new(500, 0),
            best_ask_depth: Decimal::new(450, 0),
            price_velocity: Some(0.001), // 0.1% per second
            recent_prices: vec![],
            data_latency_ms: 500,
            quote_age_secs: 2,
            last_update_timestamp: 0,
            consecutive_same_side_trades: 2,
            last_trade_side: "buy".to_string(),
            recent_trade_count: 10,
            depth_imbalance: 0.05, // 5% imbalance
        }
    }

    #[test]
    fn test_good_market_passes() {
        let filters = MarketFilters::new(FilterConfig::default());
        let context = create_good_context();

        let result = filters.can_enter(&context);
        assert!(
            result.passed,
            "Good market should pass: {:?}",
            result.reasons
        );
    }

    #[test]
    fn test_wide_spread_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.spread_bps = Some(500); // 5% spread

        let result = filters.can_enter(&context);
        assert!(!result.passed, "Wide spread should fail");
        assert!(result.reasons.iter().any(|r| r.contains("Spread too wide")));
    }

    #[test]
    fn test_low_depth_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.bid_depth = Decimal::new(300, 0);
        context.ask_depth = Decimal::new(200, 0);

        let result = filters.can_enter(&context);
        assert!(!result.passed, "Low depth should fail");
        assert!(result
            .reasons
            .iter()
            .any(|r| r.contains("Insufficient total depth")));
    }

    #[test]
    fn test_fast_price_movement_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.price_velocity = Some(0.05); // 5% per second

        let result = filters.can_enter(&context);
        assert!(!result.passed, "Fast price movement should fail");
        assert!(result
            .reasons
            .iter()
            .any(|r| r.contains("Price moving too fast")));
    }

    #[test]
    fn test_high_latency_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.data_latency_ms = 5000; // 5 seconds

        let result = filters.can_enter(&context);
        assert!(!result.passed, "High latency should fail");
        assert!(result
            .reasons
            .iter()
            .any(|r| r.contains("Data latency too high")));
    }

    #[test]
    fn test_information_cascade_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.consecutive_same_side_trades = 10;

        let result = filters.can_enter(&context);
        assert!(!result.passed, "Information cascade should fail");
        assert!(result
            .reasons
            .iter()
            .any(|r| r.contains("information cascade")));
    }

    #[test]
    fn test_extreme_imbalance_fails() {
        let filters = MarketFilters::new(FilterConfig::default());
        let mut context = create_good_context();
        context.depth_imbalance = 0.9; // 90% imbalance

        let result = filters.can_enter(&context);
        assert!(!result.passed, "Extreme imbalance should fail");
        assert!(result.reasons.iter().any(|r| r.contains("imbalance")));
    }

    #[test]
    fn test_price_velocity_calculation() {
        let prices = vec![
            (1000, Decimal::new(100, 0)),
            (2000, Decimal::new(105, 0)),
            (3000, Decimal::new(110, 0)),
        ];

        let velocity = MarketFilters::calculate_price_velocity(&prices, 10);
        assert!(velocity.is_some());

        let v = velocity.unwrap();
        // 10 point change over 2 seconds = 5 points/sec
        assert!((v - 5.0).abs() < 0.1, "Velocity should be ~5.0, got {}", v);
    }
}
