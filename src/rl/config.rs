//! RL Configuration
//!
//! Configuration structs for reinforcement learning components.

use serde::{Deserialize, Serialize};

/// Main RL configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RLConfig {
    /// PPO algorithm configuration
    pub ppo: PPOConfig,
    /// Training configuration
    pub training: TrainingConfig,
    /// State encoder configuration
    pub state: StateConfig,
    /// Reward function configuration
    pub reward: RewardConfig,
}

impl Default for RLConfig {
    fn default() -> Self {
        Self {
            ppo: PPOConfig::default(),
            training: TrainingConfig::default(),
            state: StateConfig::default(),
            reward: RewardConfig::default(),
        }
    }
}

/// PPO algorithm hyperparameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PPOConfig {
    /// Learning rate
    pub lr: f64,
    /// Discount factor (gamma)
    pub gamma: f32,
    /// GAE lambda
    pub gae_lambda: f32,
    /// PPO clip range
    pub clip_range: f32,
    /// Value function coefficient
    pub vf_coef: f32,
    /// Entropy bonus coefficient
    pub ent_coef: f32,
    /// Number of PPO epochs per update
    pub n_epochs: usize,
    /// Mini-batch size
    pub batch_size: usize,
    /// Target KL divergence for early stopping
    pub target_kl: Option<f32>,
    /// Maximum gradient norm for clipping
    pub max_grad_norm: f32,
}

impl Default for PPOConfig {
    fn default() -> Self {
        Self {
            lr: 3e-4,
            gamma: 0.99,
            gae_lambda: 0.95,
            clip_range: 0.2,
            vf_coef: 0.5,
            ent_coef: 0.01,
            n_epochs: 10,
            batch_size: 64,
            target_kl: Some(0.015),
            max_grad_norm: 0.5,
        }
    }
}

/// Training loop configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// Enable online learning during live trading
    pub online_learning: bool,
    /// Number of steps between policy updates
    pub update_frequency: usize,
    /// Replay buffer capacity
    pub buffer_size: usize,
    /// Minimum samples before first update
    pub warmup_steps: usize,
    /// Checkpoint save frequency (episodes)
    pub checkpoint_frequency: usize,
    /// Path for saving checkpoints
    pub checkpoint_dir: String,
    /// Exploration rate (epsilon for epsilon-greedy)
    pub exploration_rate: f32,
    /// Exploration decay rate
    pub exploration_decay: f32,
    /// Minimum exploration rate
    pub exploration_min: f32,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            online_learning: true,
            update_frequency: 128,
            buffer_size: 10_000,
            warmup_steps: 256,
            checkpoint_frequency: 100,
            checkpoint_dir: "./checkpoints".to_string(),
            exploration_rate: 1.0,
            exploration_decay: 0.995,
            exploration_min: 0.05,
        }
    }
}

/// State encoder configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfig {
    /// Number of historical prices to track
    pub price_history_len: usize,
    /// Normalization method
    pub normalization: NormalizationMethod,
    /// Whether to include time features
    pub include_time_features: bool,
    /// Whether to include position features
    pub include_position_features: bool,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            price_history_len: 15,
            normalization: NormalizationMethod::ZScore,
            include_time_features: true,
            include_position_features: true,
        }
    }
}

/// Normalization methods for state features
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizationMethod {
    /// No normalization
    None,
    /// Min-max scaling to [0, 1]
    MinMax,
    /// Z-score standardization
    ZScore,
    /// Robust scaling using median and IQR
    Robust,
}

/// Reward function configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardConfig {
    /// Weight for PnL reward component
    pub pnl_weight: f32,
    /// Weight for risk penalty component
    pub risk_weight: f32,
    /// Weight for timing bonus component
    pub timing_weight: f32,
    /// Weight for transaction cost penalty
    pub cost_weight: f32,
    /// Reward shaping: penalty per step to encourage action
    pub step_penalty: f32,
    /// Bonus for profitable trades
    pub profit_bonus: f32,
    /// Penalty multiplier for losses
    pub loss_penalty_multiplier: f32,
}

impl Default for RewardConfig {
    fn default() -> Self {
        Self {
            pnl_weight: 1.0,
            risk_weight: 0.1,
            timing_weight: 0.2,
            cost_weight: 0.05,
            step_penalty: 0.0,
            profit_bonus: 0.1,
            loss_penalty_multiplier: 1.5,
        }
    }
}
