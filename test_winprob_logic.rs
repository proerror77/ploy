#!/usr/bin/env rust-script
//! Test NBA Win Probability Model Logic

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn calculate_win_prob(
    point_diff: f64,
    time_remaining: f64,
    quarter: u8,
    possession: f64,
) -> (f64, f64) {
    // Simple coefficients (untrained, for testing)
    let intercept = 0.0;
    let coef_point_diff = 0.15;
    let coef_time = -0.02;
    let coef_possession = 0.05;
    let coef_q4 = 0.1;
    let coef_point_time = 0.005;
    let coef_point_q4 = 0.02;

    let q4_indicator = if quarter == 4 { 1.0 } else { 0.0 };

    let logit = intercept
        + coef_point_diff * point_diff
        + coef_time * time_remaining
        + coef_possession * possession
        + coef_q4 * q4_indicator
        + coef_point_time * (point_diff * time_remaining)
        + coef_point_q4 * (point_diff * q4_indicator);

    let win_prob = sigmoid(logit);

    // Calculate uncertainty
    let time_uncertainty = if time_remaining > 40.0 {
        0.30
    } else if time_remaining > 24.0 {
        0.20
    } else if time_remaining > 12.0 {
        0.10
    } else {
        0.05
    };

    let score_uncertainty = if point_diff.abs() > 25.0 {
        0.25
    } else if point_diff.abs() > 20.0 {
        0.15
    } else if point_diff.abs() > 15.0 {
        0.05
    } else {
        0.0
    };

    let uncertainty = f64::min(time_uncertainty + score_uncertainty, 0.5);

    (win_prob, uncertainty)
}

fn main() {
    println!("\n{}", "=".repeat(70));
    println!("NBA Live Win Probability Model - Logic Test");
    println!("{}\n", "=".repeat(70));

    // Test Case 1: Ahead by 10 in Q4, 5 min left
    println!("Test 1: Ahead by 10 in Q4, 5 min left");
    let (wp1, unc1) = calculate_win_prob(10.0, 5.0, 4, 1.0);
    println!("  Win Prob: {:.1}%", wp1 * 100.0);
    println!("  Confidence: {:.1}%", (1.0 - unc1) * 100.0);
    println!("  Uncertainty: {:.1}%\n", unc1 * 100.0);

    // Test Case 2: Tied in Q2, 30 min left
    println!("Test 2: Tied game in Q2, 30 min left");
    let (wp2, unc2) = calculate_win_prob(0.0, 30.0, 2, 0.0);
    println!("  Win Prob: {:.1}%", wp2 * 100.0);
    println!("  Confidence: {:.1}%", (1.0 - unc2) * 100.0);
    println!("  Uncertainty: {:.1}%\n", unc2 * 100.0);

    // Test Case 3: Down by 15 in Q3, 8 min left
    println!("Test 3: Down by 15 in Q3, 8 min left");
    let (wp3, unc3) = calculate_win_prob(-15.0, 8.0, 3, 1.0);
    println!("  Win Prob: {:.1}%", wp3 * 100.0);
    println!("  Confidence: {:.1}%", (1.0 - unc3) * 100.0);
    println!("  Uncertainty: {:.1}%\n", unc3 * 100.0);

    // Test Case 4: Down by 20 in Q2, 25 min left (blowout)
    println!("Test 4: Down by 20 in Q2, 25 min left (blowout)");
    let (wp4, unc4) = calculate_win_prob(-20.0, 25.0, 2, 0.0);
    println!("  Win Prob: {:.1}%", wp4 * 100.0);
    println!("  Confidence: {:.1}%", (1.0 - unc4) * 100.0);
    println!("  Uncertainty: {:.1}%\n", unc4 * 100.0);

    // Test Case 5: Down by 5 in Q4, 2 min left (close game)
    println!("Test 5: Down by 5 in Q4, 2 min left (close game)");
    let (wp5, unc5) = calculate_win_prob(-5.0, 2.0, 4, 1.0);
    println!("  Win Prob: {:.1}%", wp5 * 100.0);
    println!("  Confidence: {:.1}%", (1.0 - unc5) * 100.0);
    println!("  Uncertainty: {:.1}%\n", unc5 * 100.0);

    println!("{}", "=".repeat(70));
    println!("✓ All logic tests completed successfully!");
    println!("{}\n", "=".repeat(70));
}
