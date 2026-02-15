//! NBA Live Win Probability Model
//!
//! Implements a logistic regression model to predict win probability
//! based on live game state (score, time, possession, team strength).
//!
//! The model outputs:
//! - win_prob: Predicted probability of winning (0.0 to 1.0)
//! - uncertainty: Model uncertainty (0.0 to 1.0, higher = less confident)
//! - confidence: 1.0 - uncertainty
//!
//! This is the core "edge" source for the strategy. The model must be:
//! 1. Calibrated (predicted probabilities match actual outcomes)
//! 2. More accurate than market prices
//! 3. Fast enough for real-time trading

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Live win probability model using logistic regression
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveWinProbModel {
    /// Model coefficients (trained from historical data)
    pub coefficients: WinProbCoefficients,

    /// Model metadata
    pub metadata: ModelMetadata,
}

/// Model coefficients for logistic regression
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinProbCoefficients {
    // Intercept
    pub intercept: f64,

    // Core game state features
    pub point_diff: f64,     // Points ahead (positive) or behind (negative)
    pub time_remaining: f64, // Minutes remaining in game
    pub possession: f64,     // 1.0 = team has ball, 0.0 = opponent has ball

    // Pre-game strength features
    pub pregame_spread: f64, // Pre-game point spread (positive = favored)
    pub elo_diff: f64,       // Elo rating difference

    // Quarter indicators (dummy variables)
    pub quarter_2: f64,
    pub quarter_3: f64,
    pub quarter_4: f64,
    // quarter_1 is the reference category (omitted)

    // Interaction terms (capture non-linear effects)
    pub point_diff_x_time: f64,     // Score matters more with less time
    pub point_diff_x_quarter4: f64, // Score matters even more in Q4

    // Comeback features (optional — used by NBA comeback strategy)
    #[serde(default)]
    pub comeback_rate: f64, // team's historical comeback rate
    #[serde(default)]
    pub comeback_rate_x_deficit: f64, // interaction: comeback_rate * |deficit|
}

/// Model metadata for tracking and validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub version: String,
    pub trained_on: String,       // Date range of training data
    pub n_samples: usize,         // Number of training samples
    pub brier_score: Option<f64>, // Calibration metric (lower is better)
    pub log_loss: Option<f64>,    // Accuracy metric (lower is better)
    pub calibrated: bool,         // Whether isotonic calibration was applied
}

/// Game features for prediction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameFeatures {
    /// Point differential (positive = team is ahead)
    pub point_diff: f64,

    /// Time remaining in game (minutes)
    pub time_remaining: f64,

    /// Current quarter (1-4)
    pub quarter: u8,

    /// Possession indicator (1.0 = team has ball, 0.0 = opponent has ball)
    pub possession: f64,

    /// Pre-game point spread (positive = team was favored)
    pub pregame_spread: f64,

    /// Elo rating difference (positive = team has higher Elo)
    pub elo_diff: f64,

    /// Historical comeback rate for this team (None = don't use this feature)
    #[serde(default)]
    pub comeback_rate: Option<f64>,
}

/// Win probability prediction with uncertainty
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinProbPrediction {
    /// Predicted win probability (0.0 to 1.0)
    pub win_prob: f64,

    /// Model uncertainty (0.0 to 1.0, higher = less confident)
    pub uncertainty: f64,

    /// Confidence (1.0 - uncertainty)
    pub confidence: f64,

    /// Input features used for prediction
    pub features: GameFeatures,

    /// Raw logit value (before sigmoid)
    pub logit: f64,
}

impl LiveWinProbModel {
    /// Create a new model with given coefficients
    pub fn new(coefficients: WinProbCoefficients, metadata: ModelMetadata) -> Self {
        Self {
            coefficients,
            metadata,
        }
    }

    /// Load model from JSON file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let model: Self = serde_json::from_str(&content)?;
        Ok(model)
    }

    /// Save model to JSON file
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Predict win probability for given game state
    pub fn predict(&self, features: &GameFeatures) -> WinProbPrediction {
        let coef = &self.coefficients;

        // Calculate logit (linear combination of features)
        let logit = coef.intercept
            + coef.point_diff * features.point_diff
            + coef.time_remaining * features.time_remaining
            + coef.possession * features.possession
            + coef.pregame_spread * features.pregame_spread
            + coef.elo_diff * features.elo_diff
            + coef.quarter_2 * (if features.quarter == 2 { 1.0 } else { 0.0 })
            + coef.quarter_3 * (if features.quarter == 3 { 1.0 } else { 0.0 })
            + coef.quarter_4 * (if features.quarter == 4 { 1.0 } else { 0.0 })
            + coef.point_diff_x_time * (features.point_diff * features.time_remaining)
            + coef.point_diff_x_quarter4
                * (features.point_diff * if features.quarter == 4 { 1.0 } else { 0.0 })
            // Comeback rate features (only active when provided)
            + features.comeback_rate.map_or(0.0, |cr| {
                coef.comeback_rate * cr
                    + coef.comeback_rate_x_deficit * cr * features.point_diff.abs()
            });

        // Apply sigmoid function to get probability
        let win_prob = Self::sigmoid(logit);

        // Calculate uncertainty
        let uncertainty = self.calculate_uncertainty(features);

        WinProbPrediction {
            win_prob,
            uncertainty,
            confidence: 1.0 - uncertainty,
            features: features.clone(),
            logit,
        }
    }

    /// Sigmoid function: 1 / (1 + exp(-x))
    fn sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    /// Calculate model uncertainty based on feature values
    ///
    /// Uncertainty is higher when:
    /// - Game is early (less predictive information)
    /// - Score differential is extreme (fewer training samples)
    /// - Features are outside typical range (extrapolation)
    fn calculate_uncertainty(&self, features: &GameFeatures) -> f64 {
        let mut uncertainty: f64 = 0.0;

        // 1. Time-based uncertainty (early game is less predictable)
        let time_uncertainty = if features.time_remaining > 40.0 {
            0.30 // Q1: High uncertainty
        } else if features.time_remaining > 24.0 {
            0.20 // Q2: Moderate uncertainty
        } else if features.time_remaining > 12.0 {
            0.10 // Q3: Low uncertainty
        } else {
            0.05 // Q4: Very low uncertainty
        };
        uncertainty += time_uncertainty;

        // 2. Score differential uncertainty (extreme scores are rare)
        let score_uncertainty = if features.point_diff.abs() > 25.0 {
            0.25 // Blowout: rare in training data
        } else if features.point_diff.abs() > 20.0 {
            0.15
        } else if features.point_diff.abs() > 15.0 {
            0.05
        } else {
            0.0 // Normal range
        };
        uncertainty += score_uncertainty;

        // 3. Pregame spread uncertainty (extreme favorites/underdogs)
        let spread_uncertainty = if features.pregame_spread.abs() > 15.0 {
            0.10 // Extreme mismatch
        } else {
            0.0
        };
        uncertainty += spread_uncertainty;

        // Cap uncertainty at 0.5 (50% uncertain = coin flip)
        f64::min(uncertainty, 0.5)
    }

    /// Create a default model with placeholder coefficients
    ///
    /// WARNING: This is NOT a trained model. Use only for testing.
    /// Real coefficients must be trained on historical data.
    pub fn default_untrained() -> Self {
        Self {
            coefficients: WinProbCoefficients {
                intercept: 0.0,
                point_diff: 0.15,      // ~15% per point (rough estimate)
                time_remaining: -0.02, // Less time = more certain
                possession: 0.05,      // Small advantage for possession
                pregame_spread: 0.03,  // Pre-game strength matters
                elo_diff: 0.001,       // Elo difference (per point)
                quarter_2: 0.0,
                quarter_3: 0.0,
                quarter_4: 0.1,                // Q4 is more decisive
                point_diff_x_time: 0.005,      // Score matters more late
                point_diff_x_quarter4: 0.02,   // Score matters even more in Q4
                comeback_rate: 1.5,            // Comeback rate boost
                comeback_rate_x_deficit: 0.05, // Interaction: rate × deficit
            },
            metadata: ModelMetadata {
                version: "0.1.0-untrained".to_string(),
                trained_on: "N/A".to_string(),
                n_samples: 0,
                brier_score: None,
                log_loss: None,
                calibrated: false,
            },
        }
    }
}

impl Default for LiveWinProbModel {
    fn default() -> Self {
        Self::default_untrained()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid() {
        assert!((LiveWinProbModel::sigmoid(0.0) - 0.5).abs() < 1e-10);
        assert!(LiveWinProbModel::sigmoid(10.0) > 0.99);
        assert!(LiveWinProbModel::sigmoid(-10.0) < 0.01);
    }

    #[test]
    fn test_predict_basic() {
        let model = LiveWinProbModel::default_untrained();

        // Test case: Team ahead by 10 points in Q4 with 5 minutes left
        let features = GameFeatures {
            point_diff: 10.0,
            time_remaining: 5.0,
            quarter: 4,
            possession: 1.0,
            pregame_spread: 0.0,
            elo_diff: 0.0,
            comeback_rate: None,
        };

        let prediction = model.predict(&features);

        // Should predict high win probability
        assert!(
            prediction.win_prob > 0.7,
            "Win prob should be > 70% when ahead by 10 in Q4"
        );
        assert!(
            prediction.confidence > 0.5,
            "Confidence should be reasonable in Q4"
        );
    }

    #[test]
    fn test_predict_close_game() {
        let model = LiveWinProbModel::default_untrained();

        // Test case: Tied game in Q2
        let features = GameFeatures {
            point_diff: 0.0,
            time_remaining: 30.0,
            quarter: 2,
            possession: 0.0,
            pregame_spread: 0.0,
            elo_diff: 0.0,
            comeback_rate: None,
        };

        let prediction = model.predict(&features);

        // Should predict close to 50%
        assert!(
            (prediction.win_prob - 0.5).abs() < 0.2,
            "Tied game should be close to 50%"
        );
        assert!(
            prediction.uncertainty > 0.15,
            "Early game should have higher uncertainty"
        );
    }

    #[test]
    fn test_uncertainty_increases_early() {
        let model = LiveWinProbModel::default_untrained();

        let features_q1 = GameFeatures {
            point_diff: 5.0,
            time_remaining: 45.0,
            quarter: 1,
            possession: 1.0,
            pregame_spread: 0.0,
            elo_diff: 0.0,
            comeback_rate: None,
        };

        let features_q4 = GameFeatures {
            point_diff: 5.0,
            time_remaining: 5.0,
            quarter: 4,
            possession: 1.0,
            pregame_spread: 0.0,
            elo_diff: 0.0,
            comeback_rate: None,
        };

        let pred_q1 = model.predict(&features_q1);
        let pred_q4 = model.predict(&features_q4);

        assert!(
            pred_q1.uncertainty > pred_q4.uncertainty,
            "Q1 should have higher uncertainty than Q4"
        );
    }

    #[test]
    fn test_save_load_model() {
        let model = LiveWinProbModel::default_untrained();
        let temp_path = "/tmp/test_winprob_model.json";

        // Save
        model.to_file(temp_path).expect("Failed to save model");

        // Load
        let loaded = LiveWinProbModel::from_file(temp_path).expect("Failed to load model");

        // Verify
        assert_eq!(model.coefficients.intercept, loaded.coefficients.intercept);
        assert_eq!(model.metadata.version, loaded.metadata.version);

        // Cleanup
        std::fs::remove_file(temp_path).ok();
    }

    #[test]
    fn test_comeback_rate_boosts_win_prob() {
        let model = LiveWinProbModel::default_untrained();

        // Team trailing by 8 in Q3, WITHOUT comeback rate
        let features_no_cr = GameFeatures {
            point_diff: -8.0,
            time_remaining: 17.0,
            quarter: 3,
            possession: 0.5,
            pregame_spread: 0.0,
            elo_diff: 0.0,
            comeback_rate: None,
        };

        // Same situation, WITH 30% comeback rate
        let features_high_cr = GameFeatures {
            comeback_rate: Some(0.30),
            ..features_no_cr.clone()
        };

        // Same situation, WITH 10% comeback rate
        let features_low_cr = GameFeatures {
            comeback_rate: Some(0.10),
            ..features_no_cr.clone()
        };

        let pred_none = model.predict(&features_no_cr);
        let pred_high = model.predict(&features_high_cr);
        let pred_low = model.predict(&features_low_cr);

        // High comeback rate should produce higher win_prob than no rate
        assert!(
            pred_high.win_prob > pred_none.win_prob,
            "High comeback rate ({:.3}) should boost win_prob above no-rate ({:.3})",
            pred_high.win_prob,
            pred_none.win_prob
        );

        // High comeback rate should produce higher win_prob than low rate
        assert!(
            pred_high.win_prob > pred_low.win_prob,
            "30% comeback ({:.3}) should beat 10% comeback ({:.3})",
            pred_high.win_prob,
            pred_low.win_prob
        );
    }
}
