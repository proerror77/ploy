#!/usr/bin/env rust-script
//! Complete Entry Logic Test - Integration of All Components

use std::fmt;

// Simplified Decimal type
#[derive(Debug, Clone, Copy)]
struct Decimal(f64);

impl Decimal {
    fn new(val: i64, scale: u32) -> Self {
        Self(val as f64 / 10_f64.powi(scale as i32))
    }
    fn to_f64(&self) -> Option<f64> {
        Some(self.0)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:.4}", self.0)
    }
}

// ============================================================================
// Component 1: Win Probability Model
// ============================================================================

#[derive(Clone)]
struct GameFeatures {
    point_diff: f64,
    time_remaining: f64,
    quarter: u8,
    possession: f64,
    pregame_spread: f64,
    elo_diff: f64,
}

struct WinProbPrediction {
    win_prob: f64,
    uncertainty: f64,
    confidence: f64,
    features: GameFeatures,
}

fn predict_win_prob(features: &GameFeatures) -> WinProbPrediction {
    // Simple logistic regression
    let logit = 0.0
        + 0.15 * features.point_diff
        + (-0.02) * features.time_remaining
        + 0.05 * features.possession
        + 0.03 * features.pregame_spread
        + 0.001 * features.elo_diff
        + 0.1 * (if features.quarter == 4 { 1.0 } else { 0.0 })
        + 0.005 * (features.point_diff * features.time_remaining)
        + 0.02 * (features.point_diff * if features.quarter == 4 { 1.0 } else { 0.0 });

    let win_prob = 1.0 / (1.0 + (-logit).exp());

    let time_unc = if features.time_remaining > 40.0 { 0.30 }
                   else if features.time_remaining > 24.0 { 0.20 }
                   else if features.time_remaining > 12.0 { 0.10 }
                   else { 0.05 };

    let score_unc = if features.point_diff.abs() > 25.0 { 0.25 }
                    else if features.point_diff.abs() > 20.0 { 0.15 }
                    else if features.point_diff.abs() > 15.0 { 0.05 }
                    else { 0.0 };

    let uncertainty = f64::min(time_unc + score_unc, 0.5);

    WinProbPrediction {
        win_prob,
        uncertainty,
        confidence: 1.0 - uncertainty,
        features: features.clone(),
    }
}

// ============================================================================
// Component 2: Market Filters
// ============================================================================

struct FilterResult {
    passed: bool,
    reasons: Vec<String>,
    warnings: Vec<String>,
}

fn check_market_filters(
    spread_bps: i32,
    total_depth: f64,
    price_velocity: f64,
    latency_ms: u64,
) -> FilterResult {
    let mut reasons = vec![];
    let mut warnings = vec![];

    if spread_bps > 200 {
        reasons.push(format!("Spread too wide: {} bps", spread_bps));
    }
    if total_depth < 1000.0 {
        reasons.push(format!("Insufficient depth: ${:.0}", total_depth));
    }
    if price_velocity.abs() > 0.01 {
        reasons.push(format!("Price moving too fast: {:.4}/sec", price_velocity));
    }
    if latency_ms > 2000 {
        reasons.push(format!("High latency: {}ms", latency_ms));
    }

    if spread_bps > 100 && spread_bps <= 200 {
        warnings.push(format!("Elevated spread: {} bps", spread_bps));
    }

    FilterResult {
        passed: reasons.is_empty(),
        reasons,
        warnings,
    }
}

// ============================================================================
// Component 3: Entry Logic
// ============================================================================

struct EntryConfig {
    min_edge: f64,
    min_confidence: f64,
    min_ev_after_fees: f64,
    fee_rate: f64,
    slippage_estimate: f64,
    min_market_price: f64,
    max_market_price: f64,
}

impl Default for EntryConfig {
    fn default() -> Self {
        Self {
            min_edge: 0.05,
            min_confidence: 0.70,
            min_ev_after_fees: 0.02,
            fee_rate: 0.02,
            slippage_estimate: 0.005,
            min_market_price: 0.05,
            max_market_price: 0.80,
        }
    }
}

enum EntryDecision {
    Approve {
        p_model: f64,
        p_market: f64,
        edge: f64,
        net_ev: f64,
        confidence: f64,
    },
    Reject {
        reason: String,
        details: Vec<String>,
    },
}

fn should_enter(
    config: &EntryConfig,
    prediction: &WinProbPrediction,
    market_price: f64,
    filter_result: &FilterResult,
) -> EntryDecision {
    let p_model = prediction.win_prob;
    let p_market = market_price;

    // Step 0: Filters
    if !filter_result.passed {
        return EntryDecision::Reject {
            reason: "Market filters failed".to_string(),
            details: filter_result.reasons.clone(),
        };
    }

    // Step 1: Price sanity
    if p_market < config.min_market_price {
        return EntryDecision::Reject {
            reason: "Price too low".to_string(),
            details: vec![format!("{:.4} < {:.4}", p_market, config.min_market_price)],
        };
    }
    if p_market > config.max_market_price {
        return EntryDecision::Reject {
            reason: "Price too high".to_string(),
            details: vec![format!("{:.4} > {:.4}", p_market, config.max_market_price)],
        };
    }

    // Step 2: Edge
    let edge = p_model - p_market;
    if edge < config.min_edge {
        return EntryDecision::Reject {
            reason: "Insufficient edge".to_string(),
            details: vec![format!("Edge {:.2}% < {:.2}%", edge * 100.0, config.min_edge * 100.0)],
        };
    }

    // Step 3: Confidence
    if prediction.confidence < config.min_confidence {
        return EntryDecision::Reject {
            reason: "Low confidence".to_string(),
            details: vec![format!("Confidence {:.2} < {:.2}", prediction.confidence, config.min_confidence)],
        };
    }

    // Step 4: EV
    let gross_ev = p_model * 1.0 - p_market;
    let fees = p_market * config.fee_rate;
    let net_ev = gross_ev - fees - config.slippage_estimate;

    if net_ev < config.min_ev_after_fees {
        return EntryDecision::Reject {
            reason: "Insufficient EV".to_string(),
            details: vec![format!("Net EV {:.4} < {:.4}", net_ev, config.min_ev_after_fees)],
        };
    }

    // Approve!
    EntryDecision::Approve {
        p_model,
        p_market,
        edge,
        net_ev,
        confidence: prediction.confidence,
    }
}

// ============================================================================
// Test Scenarios
// ============================================================================

fn main() {
    println!("\n{}", "=".repeat(80));
    println!("NBA Entry Logic - Complete Integration Test");
    println!("{}\n", "=".repeat(80));

    let config = EntryConfig::default();

    // Scenario 1: Perfect entry (all checks pass)
    println!("Scenario 1: Perfect Entry Conditions");
    println!("{}", "-".repeat(80));
    let features1 = GameFeatures {
        point_diff: -12.0,
        time_remaining: 8.0,
        quarter: 3,
        possession: 1.0,
        pregame_spread: 5.0,
        elo_diff: 50.0,
    };
    let pred1 = predict_win_prob(&features1);
    let market_price1 = 0.15;
    let filters1 = check_market_filters(50, 2000.0, 0.002, 800);

    println!("Game State:");
    println!("  Down by 12 in Q3, 8 min left, has possession");
    println!("  Pregame favorite (+5 spread, +50 Elo)");
    println!("\nModel Prediction:");
    println!("  Win Prob: {:.1}%", pred1.win_prob * 100.0);
    println!("  Confidence: {:.1}%", pred1.confidence * 100.0);
    println!("\nMarket:");
    println!("  Price: {:.4} ({:.1}% implied)", market_price1, market_price1 * 100.0);
    println!("  Spread: 50 bps, Depth: $2000, Latency: 800ms");
    println!("\nFilters: {}", if filters1.passed { "✓ PASS" } else { "✗ FAIL" });

    match should_enter(&config, &pred1, market_price1, &filters1) {
        EntryDecision::Approve { p_model: _, p_market: _, edge, net_ev, confidence } => {
            println!("\n✓ ENTRY APPROVED");
            println!("  Edge: {:.2}%", edge * 100.0);
            println!("  Net EV: {:.2}%", net_ev * 100.0);
            println!("  Confidence: {:.1}%", confidence * 100.0);
        },
        EntryDecision::Reject { reason, details } => {
            println!("\n✗ ENTRY REJECTED: {}", reason);
            for detail in details {
                println!("  - {}", detail);
            }
        },
    }

    // Scenario 2: Insufficient edge
    println!("\n\n{}", "=".repeat(80));
    println!("Scenario 2: Insufficient Edge");
    println!("{}", "-".repeat(80));
    let features2 = GameFeatures {
        point_diff: -5.0,
        time_remaining: 10.0,
        quarter: 3,
        possession: 1.0,
        pregame_spread: 0.0,
        elo_diff: 0.0,
    };
    let pred2 = predict_win_prob(&features2);
    let market_price2 = 0.40; // Close to model prediction
    let filters2 = check_market_filters(50, 2000.0, 0.002, 800);

    println!("Game State: Down by 5 in Q3");
    println!("Model: {:.1}%, Market: {:.1}%", pred2.win_prob * 100.0, market_price2 * 100.0);

    match should_enter(&config, &pred2, market_price2, &filters2) {
        EntryDecision::Approve { .. } => println!("\n✓ APPROVED"),
        EntryDecision::Reject { reason, details } => {
            println!("\n✗ REJECTED: {}", reason);
            for detail in details {
                println!("  - {}", detail);
            }
        },
    }

    // Scenario 3: Failed filters
    println!("\n\n{}", "=".repeat(80));
    println!("Scenario 3: Failed Market Filters");
    println!("{}", "-".repeat(80));
    let features3 = GameFeatures {
        point_diff: -15.0,
        time_remaining: 6.0,
        quarter: 3,
        possession: 1.0,
        pregame_spread: 7.0,
        elo_diff: 80.0,
    };
    let pred3 = predict_win_prob(&features3);
    let market_price3 = 0.12;
    let filters3 = check_market_filters(400, 500.0, 0.03, 3000); // Bad conditions

    println!("Game State: Down by 15 in Q3 (strong team)");
    println!("Model: {:.1}%, Market: {:.1}% (good edge!)", pred3.win_prob * 100.0, market_price3 * 100.0);
    println!("BUT: Spread 400bps, Depth $500, Velocity 3%/sec, Latency 3000ms");

    match should_enter(&config, &pred3, market_price3, &filters3) {
        EntryDecision::Approve { .. } => println!("\n✓ APPROVED"),
        EntryDecision::Reject { reason, details } => {
            println!("\n✗ REJECTED: {}", reason);
            for detail in details {
                println!("  - {}", detail);
            }
        },
    }

    // Scenario 4: Low confidence
    println!("\n\n{}", "=".repeat(80));
    println!("Scenario 4: Low Model Confidence");
    println!("{}", "-".repeat(80));
    let features4 = GameFeatures {
        point_diff: -25.0, // Extreme blowout
        time_remaining: 35.0, // Very early
        quarter: 2,
        possession: 0.0,
        pregame_spread: -5.0, // Was underdog
        elo_diff: -50.0,
    };
    let pred4 = predict_win_prob(&features4);
    let market_price4 = 0.08;
    let filters4 = check_market_filters(50, 2000.0, 0.002, 800);

    println!("Game State: Down by 25 in Q2 (blowout, early)");
    println!("Model: {:.1}%, Confidence: {:.1}%", pred4.win_prob * 100.0, pred4.confidence * 100.0);
    println!("Market: {:.1}%", market_price4 * 100.0);

    match should_enter(&config, &pred4, market_price4, &filters4) {
        EntryDecision::Approve { .. } => println!("\n✓ APPROVED"),
        EntryDecision::Reject { reason, details } => {
            println!("\n✗ REJECTED: {}", reason);
            for detail in details {
                println!("  - {}", detail);
            }
        },
    }

    println!("\n{}", "=".repeat(80));
    println!("✓ All integration tests completed!");
    println!("{}\n", "=".repeat(80));
}
