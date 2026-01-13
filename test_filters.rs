#!/usr/bin/env rust-script
//! Test Market Microstructure Filters

use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Decimal(f64);

impl Decimal {
    fn new(val: i64, scale: u32) -> Self {
        Self(val as f64 / 10_f64.powi(scale as i32))
    }

    fn to_f64(&self) -> Option<f64> {
        Some(self.0)
    }
}

impl std::ops::Add for Decimal {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl std::ops::Sub for Decimal {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl std::cmp::PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl std::cmp::PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

#[derive(Debug, Clone)]
struct FilterConfig {
    max_spread_bps: i32,
    max_spread_absolute: Decimal,
    min_book_depth_usd: Decimal,
    min_best_depth_usd: Decimal,
    max_price_velocity: f64,
    max_data_latency_ms: u64,
    max_quote_age_secs: u64,
    max_consecutive_same_side: usize,
    max_depth_imbalance: f64,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            max_spread_bps: 200,
            max_spread_absolute: Decimal::new(5, 2),
            min_book_depth_usd: Decimal::new(1000, 0),
            min_best_depth_usd: Decimal::new(100, 0),
            max_price_velocity: 0.01,
            max_data_latency_ms: 2000,
            max_quote_age_secs: 5,
            max_consecutive_same_side: 5,
            max_depth_imbalance: 0.8,
        }
    }
}

#[derive(Debug, Clone)]
struct MarketContext {
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    spread_bps: Option<i32>,
    bid_depth: Decimal,
    ask_depth: Decimal,
    best_bid_depth: Decimal,
    best_ask_depth: Decimal,
    price_velocity: Option<f64>,
    data_latency_ms: u64,
    quote_age_secs: u64,
    consecutive_same_side_trades: usize,
    last_trade_side: String,
    depth_imbalance: f64,
}

struct FilterResult {
    passed: bool,
    reasons: Vec<String>,
}

struct MarketFilters {
    config: FilterConfig,
}

impl MarketFilters {
    fn new(config: FilterConfig) -> Self {
        Self { config }
    }

    fn can_enter(&self, context: &MarketContext) -> FilterResult {
        let mut reasons = vec![];

        // Spread check
        if let Some(spread_bps) = context.spread_bps {
            if spread_bps > self.config.max_spread_bps {
                reasons.push(format!("Spread too wide: {} bps", spread_bps));
            }
        }

        // Depth check
        let total_depth = context.bid_depth.0 + context.ask_depth.0;
        if total_depth < self.config.min_book_depth_usd.0 {
            reasons.push(format!("Insufficient depth: ${:.0}", total_depth));
        }

        // Velocity check
        if let Some(velocity) = context.price_velocity {
            if velocity.abs() > self.config.max_price_velocity {
                reasons.push(format!("Price moving too fast: {:.4}/sec", velocity));
            }
        }

        // Latency check
        if context.data_latency_ms > self.config.max_data_latency_ms {
            reasons.push(format!("High latency: {}ms", context.data_latency_ms));
        }

        // Order flow check
        if context.consecutive_same_side_trades > self.config.max_consecutive_same_side {
            reasons.push(format!(
                "Information cascade: {} consecutive {} trades",
                context.consecutive_same_side_trades,
                context.last_trade_side
            ));
        }

        // Imbalance check
        if context.depth_imbalance.abs() > self.config.max_depth_imbalance {
            reasons.push(format!("Extreme imbalance: {:.1}%", context.depth_imbalance * 100.0));
        }

        FilterResult {
            passed: reasons.is_empty(),
            reasons,
        }
    }
}

fn create_good_context() -> MarketContext {
    MarketContext {
        best_bid: Some(Decimal::new(45, 2)),
        best_ask: Some(Decimal::new(46, 2)),
        spread_bps: Some(22),
        bid_depth: Decimal::new(2000, 0),
        ask_depth: Decimal::new(1800, 0),
        best_bid_depth: Decimal::new(500, 0),
        best_ask_depth: Decimal::new(450, 0),
        price_velocity: Some(0.001),
        data_latency_ms: 500,
        quote_age_secs: 2,
        consecutive_same_side_trades: 2,
        last_trade_side: "buy".to_string(),
        depth_imbalance: 0.05,
    }
}

fn main() {
    println!("\n{}", "=".repeat(70));
    println!("Market Microstructure Filters - Test Suite");
    println!("{}\n", "=".repeat(70));

    let filters = MarketFilters::new(FilterConfig::default());

    // Test 1: Good market conditions
    println!("Test 1: Good Market Conditions");
    let context1 = create_good_context();
    let result1 = filters.can_enter(&context1);
    println!("  Result: {}", if result1.passed { "✓ PASS" } else { "✗ FAIL" });
    if !result1.passed {
        for reason in &result1.reasons {
            println!("    - {}", reason);
        }
    }
    println!();

    // Test 2: Wide spread
    println!("Test 2: Wide Spread (500 bps)");
    let mut context2 = create_good_context();
    context2.spread_bps = Some(500);
    let result2 = filters.can_enter(&context2);
    println!("  Result: {}", if result2.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result2.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 3: Low liquidity
    println!("Test 3: Low Liquidity ($500 total)");
    let mut context3 = create_good_context();
    context3.bid_depth = Decimal::new(300, 0);
    context3.ask_depth = Decimal::new(200, 0);
    let result3 = filters.can_enter(&context3);
    println!("  Result: {}", if result3.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result3.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 4: Fast price movement
    println!("Test 4: Fast Price Movement (5%/sec)");
    let mut context4 = create_good_context();
    context4.price_velocity = Some(0.05);
    let result4 = filters.can_enter(&context4);
    println!("  Result: {}", if result4.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result4.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 5: High latency
    println!("Test 5: High Data Latency (5000ms)");
    let mut context5 = create_good_context();
    context5.data_latency_ms = 5000;
    let result5 = filters.can_enter(&context5);
    println!("  Result: {}", if result5.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result5.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 6: Information cascade
    println!("Test 6: Information Cascade (10 consecutive sells)");
    let mut context6 = create_good_context();
    context6.consecutive_same_side_trades = 10;
    context6.last_trade_side = "sell".to_string();
    let result6 = filters.can_enter(&context6);
    println!("  Result: {}", if result6.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result6.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 7: Extreme imbalance
    println!("Test 7: Extreme Depth Imbalance (90%)");
    let mut context7 = create_good_context();
    context7.depth_imbalance = 0.9;
    let result7 = filters.can_enter(&context7);
    println!("  Result: {}", if result7.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result7.reasons {
        println!("    - {}", reason);
    }
    println!();

    // Test 8: Multiple failures
    println!("Test 8: Multiple Failures (wide spread + low depth + high latency)");
    let mut context8 = create_good_context();
    context8.spread_bps = Some(400);
    context8.bid_depth = Decimal::new(200, 0);
    context8.ask_depth = Decimal::new(150, 0);
    context8.data_latency_ms = 3000;
    let result8 = filters.can_enter(&context8);
    println!("  Result: {}", if result8.passed { "✓ PASS" } else { "✗ FAIL (expected)" });
    for reason in &result8.reasons {
        println!("    - {}", reason);
    }
    println!();

    println!("{}", "=".repeat(70));
    println!("✓ All filter tests completed!");
    println!("{}\n", "=".repeat(70));
}
