//! Reward Functions
//!
//! Defines reward signals and functions for RL training.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// Re-export config
pub use crate::rl::config::RewardConfig;

/// Reward signal components
///
/// Breaking down the reward into components helps with:
/// - Debugging (which component is driving behavior)
/// - Hyperparameter tuning (adjust weights)
/// - Reward shaping (add/remove components)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RewardSignal {
    /// PnL-based reward (primary)
    pub pnl_reward: f32,
    /// Risk penalty (negative for high risk)
    pub risk_penalty: f32,
    /// Timing bonus (positive for good entry signals)
    pub timing_bonus: f32,
    /// Transaction cost penalty (negative)
    pub cost_penalty: f32,
    /// Step penalty (small negative to encourage action)
    pub step_penalty: f32,
    /// Total reward (weighted sum)
    pub total: f32,
}

impl RewardSignal {
    /// Create a zero reward signal
    pub fn zero() -> Self {
        Self::default()
    }

    /// Create from just PnL
    pub fn from_pnl(pnl: f32) -> Self {
        Self {
            pnl_reward: pnl,
            total: pnl,
            ..Default::default()
        }
    }

    /// Calculate total from components using config weights
    pub fn calculate_total(&mut self, config: &RewardConfig) {
        self.total = self.pnl_reward * config.pnl_weight
            - self.risk_penalty * config.risk_weight
            + self.timing_bonus * config.timing_weight
            - self.cost_penalty * config.cost_weight
            - self.step_penalty;
    }
}

/// Trait for computing rewards
pub trait RewardFunction: Send + Sync {
    /// Compute reward from a state transition
    fn compute(&self, transition: &RewardTransition) -> RewardSignal;
}

/// Information needed to compute rewards
#[derive(Debug, Clone)]
pub struct RewardTransition {
    /// Realized PnL from this step (if any)
    pub realized_pnl: Option<Decimal>,
    /// Unrealized PnL change
    pub unrealized_pnl_delta: Option<Decimal>,
    /// Transaction costs incurred
    pub transaction_costs: Option<Decimal>,
    /// Sum of asks at entry (for timing evaluation)
    pub sum_of_asks_at_entry: Option<Decimal>,
    /// Current risk exposure (0.0 to 1.0)
    pub risk_exposure: f32,
    /// Whether position was closed
    pub position_closed: bool,
    /// Whether this was a winning trade
    pub is_winning_trade: Option<bool>,
    /// Time held in seconds
    pub hold_duration_secs: Option<i64>,
}

impl Default for RewardTransition {
    fn default() -> Self {
        Self {
            realized_pnl: None,
            unrealized_pnl_delta: None,
            transaction_costs: None,
            sum_of_asks_at_entry: None,
            risk_exposure: 0.0,
            position_closed: false,
            is_winning_trade: None,
            hold_duration_secs: None,
        }
    }
}

/// PnL-based reward function
///
/// Primary reward signal based on trading profit/loss.
#[derive(Debug, Clone)]
pub struct PnLRewardFunction {
    config: RewardConfig,
}

impl PnLRewardFunction {
    /// Create with default config
    pub fn new() -> Self {
        Self {
            config: RewardConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: RewardConfig) -> Self {
        Self { config }
    }

    fn decimal_to_f32(d: Option<Decimal>) -> f32 {
        d.and_then(|v| v.to_string().parse().ok()).unwrap_or(0.0)
    }
}

impl Default for PnLRewardFunction {
    fn default() -> Self {
        Self::new()
    }
}

impl RewardFunction for PnLRewardFunction {
    fn compute(&self, transition: &RewardTransition) -> RewardSignal {
        let mut signal = RewardSignal::zero();

        // PnL reward
        if let Some(pnl) = transition.realized_pnl {
            let pnl_f32 = Self::decimal_to_f32(Some(pnl));

            // Apply asymmetric scaling: losses hurt more
            signal.pnl_reward = if pnl_f32 >= 0.0 {
                pnl_f32 + self.config.profit_bonus
            } else {
                pnl_f32 * self.config.loss_penalty_multiplier
            };
        }

        // Unrealized PnL (smaller weight)
        if let Some(delta) = transition.unrealized_pnl_delta {
            signal.pnl_reward += Self::decimal_to_f32(Some(delta)) * 0.1;
        }

        // Risk penalty (quadratic to penalize high risk more)
        signal.risk_penalty = transition.risk_exposure.powi(2);

        // Timing bonus: reward entering when sum < 0.96 (good arb opportunity)
        if let Some(sum) = transition.sum_of_asks_at_entry {
            let sum_f32 = Self::decimal_to_f32(Some(sum));
            if sum_f32 < 0.96 {
                signal.timing_bonus = (0.96 - sum_f32) * 10.0; // Scale up
            } else if sum_f32 > 1.0 {
                signal.timing_bonus = -0.1; // Small penalty for bad timing
            }
        }

        // Transaction cost penalty
        if let Some(costs) = transition.transaction_costs {
            signal.cost_penalty = Self::decimal_to_f32(Some(costs));
        }

        // Step penalty (encourages taking action rather than waiting forever)
        signal.step_penalty = self.config.step_penalty;

        // Calculate weighted total
        signal.calculate_total(&self.config);

        signal
    }
}

/// Risk-adjusted reward function
///
/// Extends PnL reward with Sharpe-like adjustments.
#[derive(Debug, Clone)]
pub struct RiskAdjustedRewardFunction {
    base: PnLRewardFunction,
    /// Running mean of rewards for Sharpe calculation
    reward_mean: f32,
    /// Running variance of rewards
    reward_var: f32,
    /// Sample count
    count: usize,
}

impl RiskAdjustedRewardFunction {
    /// Create with default config
    pub fn new() -> Self {
        Self {
            base: PnLRewardFunction::new(),
            reward_mean: 0.0,
            reward_var: 1.0,
            count: 0,
        }
    }

    /// Create with custom config
    pub fn with_config(config: RewardConfig) -> Self {
        Self {
            base: PnLRewardFunction::with_config(config),
            reward_mean: 0.0,
            reward_var: 1.0,
            count: 0,
        }
    }

    /// Update running statistics
    pub fn update_stats(&mut self, reward: f32) {
        self.count += 1;
        let n = self.count as f32;
        let delta = reward - self.reward_mean;
        self.reward_mean += delta / n;
        let delta2 = reward - self.reward_mean;
        self.reward_var += (delta * delta2 - self.reward_var) / n;
    }

    /// Get Sharpe-like ratio
    pub fn sharpe_ratio(&self) -> f32 {
        let std = self.reward_var.sqrt().max(1e-8);
        self.reward_mean / std
    }
}

impl Default for RiskAdjustedRewardFunction {
    fn default() -> Self {
        Self::new()
    }
}

impl RewardFunction for RiskAdjustedRewardFunction {
    fn compute(&self, transition: &RewardTransition) -> RewardSignal {
        let mut signal = self.base.compute(transition);

        // Normalize by running statistics for stability
        if self.count > 10 {
            let std = self.reward_var.sqrt().max(1e-8);
            signal.total = (signal.total - self.reward_mean) / std;
        }

        signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_pnl_reward_positive() {
        let reward_fn = PnLRewardFunction::new();
        let transition = RewardTransition {
            realized_pnl: Some(dec!(10.0)),
            ..Default::default()
        };

        let signal = reward_fn.compute(&transition);
        assert!(signal.pnl_reward > 0.0);
        assert!(signal.total > 0.0);
    }

    #[test]
    fn test_pnl_reward_negative_amplified() {
        let reward_fn = PnLRewardFunction::new();
        let positive = RewardTransition {
            realized_pnl: Some(dec!(10.0)),
            ..Default::default()
        };
        let negative = RewardTransition {
            realized_pnl: Some(dec!(-10.0)),
            ..Default::default()
        };

        let pos_signal = reward_fn.compute(&positive);
        let neg_signal = reward_fn.compute(&negative);

        // Loss should hurt more than gain helps
        assert!(neg_signal.pnl_reward.abs() > pos_signal.pnl_reward);
    }

    #[test]
    fn test_timing_bonus() {
        let reward_fn = PnLRewardFunction::new();

        // Good timing (low sum of asks)
        let good = RewardTransition {
            sum_of_asks_at_entry: Some(dec!(0.94)),
            ..Default::default()
        };

        // Bad timing (sum > 1)
        let bad = RewardTransition {
            sum_of_asks_at_entry: Some(dec!(1.02)),
            ..Default::default()
        };

        let good_signal = reward_fn.compute(&good);
        let bad_signal = reward_fn.compute(&bad);

        assert!(good_signal.timing_bonus > 0.0);
        assert!(bad_signal.timing_bonus < 0.0);
    }

    #[test]
    fn test_risk_penalty_quadratic() {
        let reward_fn = PnLRewardFunction::new();

        let low_risk = RewardTransition {
            risk_exposure: 0.3,
            ..Default::default()
        };
        let high_risk = RewardTransition {
            risk_exposure: 0.9,
            ..Default::default()
        };

        let low_signal = reward_fn.compute(&low_risk);
        let high_signal = reward_fn.compute(&high_risk);

        // High risk should have much higher penalty (quadratic)
        assert!(high_signal.risk_penalty > low_signal.risk_penalty * 3.0);
    }
}
