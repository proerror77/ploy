//! Test the NBA live win probability model

use ploy::strategy::{LiveWinProbModel, GameFeatures};

fn main() {
    println!("Testing NBA Live Win Probability Model\n");
    println!("=" .repeat(60));

    // Create a default model (untrained, for testing only)
    let model = LiveWinProbModel::default_untrained();

    println!("\nModel Info:");
    println!("  Version: {}", model.metadata.version);
    println!("  Trained on: {}", model.metadata.trained_on);
    println!("  Calibrated: {}", model.metadata.calibrated);

    // Test Case 1: Team ahead by 10 in Q4 with 5 minutes left
    println!("\n{}", "=".repeat(60));
    println!("Test Case 1: Ahead by 10 in Q4, 5 min left");
    println!("{}", "=".repeat(60));

    let features1 = GameFeatures {
        point_diff: 10.0,
        time_remaining: 5.0,
        quarter: 4,
        possession: 1.0,
        pregame_spread: 0.0,
        elo_diff: 0.0,
    };

    let pred1 = model.predict(&features1);
    println!("  Win Probability: {:.1}%", pred1.win_prob * 100.0);
    println!("  Confidence: {:.1}%", pred1.confidence * 100.0);
    println!("  Uncertainty: {:.1}%", pred1.uncertainty * 100.0);
    println!("  Logit: {:.4}", pred1.logit);

    // Test Case 2: Tied game in Q2
    println!("\n{}", "=".repeat(60));
    println!("Test Case 2: Tied game in Q2, 30 min left");
    println!("{}", "=".repeat(60));

    let features2 = GameFeatures {
        point_diff: 0.0,
        time_remaining: 30.0,
        quarter: 2,
        possession: 0.0,
        pregame_spread: 0.0,
        elo_diff: 0.0,
    };

    let pred2 = model.predict(&features2);
    println!("  Win Probability: {:.1}%", pred2.win_prob * 100.0);
    println!("  Confidence: {:.1}%", pred2.confidence * 100.0);
    println!("  Uncertainty: {:.1}%", pred2.uncertainty * 100.0);
    println!("  Logit: {:.4}", pred2.logit);

    // Test Case 3: Down by 15 in Q3 with 8 minutes left
    println!("\n{}", "=".repeat(60));
    println!("Test Case 3: Down by 15 in Q3, 8 min left");
    println!("{}", "=".repeat(60));

    let features3 = GameFeatures {
        point_diff: -15.0,
        time_remaining: 8.0,
        quarter: 3,
        possession: 1.0,
        pregame_spread: 5.0,  // Was favored pre-game
        elo_diff: 50.0,       // Higher Elo
    };

    let pred3 = model.predict(&features3);
    println!("  Win Probability: {:.1}%", pred3.win_prob * 100.0);
    println!("  Confidence: {:.1}%", pred3.confidence * 100.0);
    println!("  Uncertainty: {:.1}%", pred3.uncertainty * 100.0);
    println!("  Logit: {:.4}", pred3.logit);

    // Test Case 4: Down by 20 in Q2 (extreme blowout)
    println!("\n{}", "=".repeat(60));
    println!("Test Case 4: Down by 20 in Q2, 25 min left (blowout)");
    println!("{}", "=".repeat(60));

    let features4 = GameFeatures {
        point_diff: -20.0,
        time_remaining: 25.0,
        quarter: 2,
        possession: 0.0,
        pregame_spread: -3.0,  // Was underdog
        elo_diff: -30.0,
    };

    let pred4 = model.predict(&features4);
    println!("  Win Probability: {:.1}%", pred4.win_prob * 100.0);
    println!("  Confidence: {:.1}%", pred4.confidence * 100.0);
    println!("  Uncertainty: {:.1}%", pred4.uncertainty * 100.0);
    println!("  Logit: {:.4}", pred4.logit);

    // Test saving and loading
    println!("\n{}", "=".repeat(60));
    println!("Testing Model Serialization");
    println!("{}", "=".repeat(60));

    let temp_path = "/tmp/test_winprob_model.json";
    match model.to_file(temp_path) {
        Ok(_) => println!("  ✓ Model saved to {}", temp_path),
        Err(e) => println!("  ✗ Failed to save: {}", e),
    }

    match LiveWinProbModel::from_file(temp_path) {
        Ok(loaded) => {
            println!("  ✓ Model loaded successfully");
            println!("  Version: {}", loaded.metadata.version);

            // Verify predictions match
            let pred_original = model.predict(&features1);
            let pred_loaded = loaded.predict(&features1);

            if (pred_original.win_prob - pred_loaded.win_prob).abs() < 1e-10 {
                println!("  ✓ Predictions match after load");
            } else {
                println!("  ✗ Predictions differ after load");
            }
        },
        Err(e) => println!("  ✗ Failed to load: {}", e),
    }

    // Cleanup
    std::fs::remove_file(temp_path).ok();

    println!("\n{}", "=".repeat(60));
    println!("All tests completed!");
    println!("{}", "=".repeat(60));
}
