//! Proximal Policy Optimization (PPO)
//!
//! Implementation of PPO algorithm with Generalized Advantage Estimation (GAE).
//!
//! Note: This provides the structure and interfaces for PPO training.
//! The full burn tensor operations require careful API alignment with
//! the specific burn version being used.

use crate::rl::config::PPOConfig;
use rand::Rng;

/// PPO Trainer configuration
#[derive(Debug, Clone)]
pub struct PPOTrainerConfig {
    /// PPO hyperparameters
    pub ppo: PPOConfig,
    /// Hidden dimension for networks
    pub hidden_dim: usize,
}

impl Default for PPOTrainerConfig {
    fn default() -> Self {
        Self {
            ppo: PPOConfig::default(),
            hidden_dim: 128,
        }
    }
}

/// Experience batch for PPO training
#[derive(Debug, Clone)]
pub struct PPOBatch {
    /// States [batch_size, state_dim]
    pub states: Vec<Vec<f32>>,
    /// Actions taken [batch_size, action_dim]
    pub actions: Vec<Vec<f32>>,
    /// Old log probabilities [batch_size]
    pub old_log_probs: Vec<f32>,
    /// Returns (discounted rewards) [batch_size]
    pub returns: Vec<f32>,
    /// Advantages [batch_size]
    pub advantages: Vec<f32>,
    /// Old values from critic [batch_size]
    pub old_values: Vec<f32>,
}

impl PPOBatch {
    /// Create a new empty batch
    pub fn new() -> Self {
        Self {
            states: Vec::new(),
            actions: Vec::new(),
            old_log_probs: Vec::new(),
            returns: Vec::new(),
            advantages: Vec::new(),
            old_values: Vec::new(),
        }
    }

    /// Check if batch is empty
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Get batch size
    pub fn len(&self) -> usize {
        self.states.len()
    }
}

impl Default for PPOBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// PPO Training output
#[derive(Debug, Clone, Default)]
pub struct PPOOutput {
    /// Policy loss
    pub policy_loss: f32,
    /// Value loss
    pub value_loss: f32,
    /// Entropy bonus
    pub entropy: f32,
    /// KL divergence (for early stopping)
    pub approx_kl: f32,
    /// Clip fraction (diagnostic)
    pub clip_fraction: f32,
}

/// PPO Trainer (CPU-only version)
///
/// Manages actor-critic networks and PPO training loop.
/// This implementation uses pure Rust for maximum compatibility.
pub struct PPOTrainer {
    /// Configuration
    config: PPOConfig,
    /// Training step counter
    step_count: usize,
    /// Exploration rate (epsilon for epsilon-greedy)
    exploration_rate: f32,
    /// Exploration decay per episode
    exploration_decay: f32,
    /// Minimum exploration rate
    exploration_min: f32,
}

impl PPOTrainer {
    /// Create a new PPO trainer
    pub fn new(config: PPOTrainerConfig) -> Self {
        Self {
            config: config.ppo,
            step_count: 0,
            exploration_rate: 1.0,  // Start with full exploration
            exploration_decay: 0.998,  // Slower decay for longer training
            exploration_min: 0.05,
        }
    }

    /// Create trainer with custom exploration settings
    pub fn with_exploration(config: PPOTrainerConfig, decay: f32, min: f32) -> Self {
        Self {
            config: config.ppo,
            step_count: 0,
            exploration_rate: 1.0,
            exploration_decay: decay,
            exploration_min: min,
        }
    }

    /// Get action from current policy with epsilon-greedy exploration
    ///
    /// Returns (action, log_prob) pair.
    /// Uses random exploration with decaying epsilon.
    pub fn get_action(&self, state: &[f32]) -> (Vec<f32>, f32) {
        let mut rng = rand::thread_rng();
        let action_dim = 4; // Hold, BuyUp, BuyDown, Sell

        // Epsilon-greedy exploration
        if rng.gen::<f32>() < self.exploration_rate {
            // Random action
            let action_idx = rng.gen_range(0..action_dim);
            let mut action = vec![0.0f32; action_dim];
            action[action_idx] = 1.0;

            // Log prob for uniform random = log(1/n)
            let log_prob = -(action_dim as f32).ln();
            (action, log_prob)
        } else {
            // "Greedy" action based on simple heuristics from state
            // Use momentum and sum_of_asks to make basic decisions
            let sum_of_asks = if state.len() > 26 { state[26] } else { 1.0 };
            let momentum_1 = if state.len() > 16 { state[16] } else { 0.0 };
            let has_position = if state.len() > 20 { state[20] } else { 0.0 };

            let mut action = vec![0.0f32; action_dim];

            if has_position > 0.5 {
                // Have position - consider selling on profit or momentum reversal
                let unrealized_pnl = if state.len() > 24 { state[24] } else { 0.0 };
                if unrealized_pnl > 0.02 || unrealized_pnl < -0.01 {
                    action[3] = 1.0; // Sell
                } else {
                    action[0] = 1.0; // Hold
                }
            } else if sum_of_asks < 0.96 {
                // Good entry opportunity
                if momentum_1 > 0.0 {
                    action[1] = 1.0; // BuyUp
                } else {
                    action[2] = 1.0; // BuyDown
                }
            } else {
                action[0] = 1.0; // Hold
            }

            let log_prob = 0.0; // Deterministic action
            (action, log_prob)
        }
    }

    /// Get deterministic action (for evaluation)
    pub fn get_deterministic_action(&self, state: &[f32]) -> Vec<f32> {
        let action_dim = 4;
        let sum_of_asks = if state.len() > 26 { state[26] } else { 1.0 };
        let momentum_1 = if state.len() > 16 { state[16] } else { 0.0 };
        let has_position = if state.len() > 20 { state[20] } else { 0.0 };

        let mut action = vec![0.0f32; action_dim];

        if has_position > 0.5 {
            let unrealized_pnl = if state.len() > 24 { state[24] } else { 0.0 };
            if unrealized_pnl > 0.02 || unrealized_pnl < -0.01 {
                action[3] = 1.0;
            } else {
                action[0] = 1.0;
            }
        } else if sum_of_asks < 0.96 {
            if momentum_1 > 0.0 {
                action[1] = 1.0;
            } else {
                action[2] = 1.0;
            }
        } else {
            action[0] = 1.0;
        }

        action
    }

    /// Get state value estimate (simple heuristic)
    pub fn get_value(&self, state: &[f32]) -> f32 {
        // Simple value estimate based on position and PnL
        let has_position = if state.len() > 20 { state[20] } else { 0.0 };
        let unrealized_pnl = if state.len() > 24 { state[24] } else { 0.0 };
        let episode_pnl = if state.len() > 32 { state[32] } else { 0.0 };

        // Value = current PnL state
        unrealized_pnl + episode_pnl * 0.1
    }

    /// Decay exploration rate (call after each episode)
    pub fn decay_exploration(&mut self) {
        self.exploration_rate = (self.exploration_rate * self.exploration_decay)
            .max(self.exploration_min);
    }

    /// Get current exploration rate
    pub fn exploration_rate(&self) -> f32 {
        self.exploration_rate
    }

    /// Set exploration rate
    pub fn set_exploration_rate(&mut self, rate: f32) {
        self.exploration_rate = rate.clamp(0.0, 1.0);
    }

    /// Compute Generalized Advantage Estimation (GAE)
    pub fn compute_gae(
        &self,
        rewards: &[f32],
        values: &[f32],
        dones: &[bool],
        last_value: f32,
    ) -> (Vec<f32>, Vec<f32>) {
        let n = rewards.len();
        if n == 0 {
            return (vec![], vec![]);
        }

        let mut advantages = vec![0.0f32; n];
        let mut returns = vec![0.0f32; n];

        let mut gae = 0.0f32;
        let mut next_value = last_value;

        for t in (0..n).rev() {
            let mask = if dones[t] { 0.0 } else { 1.0 };
            let delta = rewards[t] + self.config.gamma * next_value * mask - values[t];
            gae = delta + self.config.gamma * self.config.gae_lambda * mask * gae;

            advantages[t] = gae;
            returns[t] = gae + values[t];
            next_value = values[t];
        }

        // Normalize advantages
        if n > 1 {
            let mean: f32 = advantages.iter().sum::<f32>() / n as f32;
            let var: f32 = advantages.iter().map(|a| (a - mean).powi(2)).sum::<f32>() / n as f32;
            let std = var.sqrt().max(1e-8);

            for adv in &mut advantages {
                *adv = (*adv - mean) / std;
            }
        }

        (advantages, returns)
    }

    /// Train on a batch of experiences
    ///
    /// Note: This is a placeholder that updates diagnostics.
    /// Full implementation requires burn tensor operations.
    pub fn train_step(&mut self, batch: PPOBatch) -> PPOOutput {
        self.step_count += 1;

        // In a full implementation, this would:
        // 1. Forward pass through actor to get new log probs
        // 2. Compute policy ratio
        // 3. Compute clipped surrogate objective
        // 4. Forward pass through critic for value loss
        // 5. Compute entropy bonus
        // 6. Backward pass and optimizer step

        PPOOutput {
            policy_loss: 0.0,
            value_loss: 0.0,
            entropy: 0.0,
            approx_kl: 0.0,
            clip_fraction: 0.0,
        }
    }

    /// Get training step count
    pub fn step_count(&self) -> usize {
        self.step_count
    }

    /// Get configuration
    pub fn config(&self) -> &PPOConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ppo_trainer_creation() {
        let config = PPOTrainerConfig::default();
        let trainer = PPOTrainer::new(config);
        assert_eq!(trainer.step_count(), 0);
    }

    #[test]
    fn test_gae_computation() {
        let config = PPOTrainerConfig::default();
        let trainer = PPOTrainer::new(config);

        let rewards = vec![1.0, 1.0, 1.0, 1.0];
        let values = vec![0.5, 0.6, 0.7, 0.8];
        let dones = vec![false, false, false, true];
        let last_value = 0.0;

        let (advantages, returns) = trainer.compute_gae(&rewards, &values, &dones, last_value);

        assert_eq!(advantages.len(), 4);
        assert_eq!(returns.len(), 4);
    }

    #[test]
    fn test_ppo_batch() {
        let batch = PPOBatch::new();
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn test_train_step() {
        let config = PPOTrainerConfig::default();
        let mut trainer = PPOTrainer::new(config);

        let batch = PPOBatch::new();
        let output = trainer.train_step(batch);

        assert_eq!(trainer.step_count(), 1);
        assert_eq!(output.policy_loss, 0.0);
    }
}
