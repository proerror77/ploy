//! Critic Network (Value Function)
//!
//! Neural networks for value estimation in actor-critic methods.

use burn::nn::{Linear, LinearConfig, Relu};
use burn::prelude::*;

use super::encoder::{StateEncoderConfig, StateEncoderNetwork, ENCODER_OUTPUT_DIM};

/// Critic network configuration
#[derive(Config, Debug)]
pub struct CriticConfig {
    /// State encoder configuration
    pub encoder: StateEncoderConfig,
    /// Hidden dimension for critic head
    #[config(default = "128")]
    pub hidden_dim: usize,
}

impl Default for CriticConfig {
    fn default() -> Self {
        Self {
            encoder: StateEncoderConfig::new(),
            hidden_dim: 128,
        }
    }
}

/// Value Critic (V-function)
///
/// Estimates the expected return from a state.
/// Used in PPO for advantage estimation.
#[derive(Module, Debug)]
pub struct Critic<B: Backend> {
    encoder: StateEncoderNetwork<B>,
    fc_hidden: Linear<B>,
    value_head: Linear<B>,
    activation: Relu,
}

impl CriticConfig {
    /// Initialize critic network
    pub fn init<B: Backend>(&self, device: &B::Device) -> Critic<B> {
        let encoder = self.encoder.init(device);
        let fc_hidden = LinearConfig::new(ENCODER_OUTPUT_DIM, self.hidden_dim).init(device);
        let value_head = LinearConfig::new(self.hidden_dim, 1).init(device);

        Critic {
            encoder,
            fc_hidden,
            value_head,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> Critic<B> {
    /// Forward pass returning state value
    pub fn forward(&self, state: Tensor<B, 2>) -> Tensor<B, 2> {
        let encoded = self.encoder.forward(state);
        let hidden = self.fc_hidden.forward(encoded);
        let hidden = self.activation.forward(hidden);
        self.value_head.forward(hidden)
    }

    /// Get value as scalar per batch element
    pub fn value(&self, state: Tensor<B, 2>) -> Tensor<B, 1> {
        self.forward(state).squeeze(1)
    }
}

/// Q-Critic (Q-function)
///
/// Estimates the expected return from a state-action pair.
/// Used in DQN and SAC.
#[derive(Module, Debug)]
pub struct QCritic<B: Backend> {
    encoder: StateEncoderNetwork<B>,
    fc_hidden: Linear<B>,
    q_head: Linear<B>,
    activation: Relu,
}

/// Q-Critic configuration
#[derive(Config, Debug)]
pub struct QCriticConfig {
    /// State encoder configuration
    pub encoder: StateEncoderConfig,
    /// Action dimension
    #[config(default = "5")]
    pub action_dim: usize,
    /// Hidden dimension
    #[config(default = "128")]
    pub hidden_dim: usize,
}

impl Default for QCriticConfig {
    fn default() -> Self {
        Self {
            encoder: StateEncoderConfig::new(),
            action_dim: 5,
            hidden_dim: 128,
        }
    }
}

impl QCriticConfig {
    /// Initialize Q-critic network
    pub fn init<B: Backend>(&self, device: &B::Device) -> QCritic<B> {
        let encoder = self.encoder.init(device);
        let fc_hidden = LinearConfig::new(ENCODER_OUTPUT_DIM + self.action_dim, self.hidden_dim)
            .init(device);
        let q_head = LinearConfig::new(self.hidden_dim, 1).init(device);

        QCritic {
            encoder,
            fc_hidden,
            q_head,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> QCritic<B> {
    /// Forward pass returning Q-value for state-action pair
    pub fn forward(&self, state: Tensor<B, 2>, action: Tensor<B, 2>) -> Tensor<B, 2> {
        let encoded = self.encoder.forward(state);
        let combined = Tensor::cat(vec![encoded, action], 1);
        let hidden = self.fc_hidden.forward(combined);
        let hidden = self.activation.forward(hidden);
        self.q_head.forward(hidden)
    }

    /// Get Q-value as scalar per batch element
    pub fn q_value(&self, state: Tensor<B, 2>, action: Tensor<B, 2>) -> Tensor<B, 1> {
        self.forward(state, action).squeeze(1)
    }
}

/// Dueling DQN Critic
///
/// Separates state value from action advantage for better learning.
#[derive(Module, Debug)]
pub struct DuelingCritic<B: Backend> {
    encoder: StateEncoderNetwork<B>,
    fc_hidden: Linear<B>,
    value_stream: Linear<B>,
    advantage_stream: Linear<B>,
    activation: Relu,
}

/// Dueling critic configuration
#[derive(Config, Debug)]
pub struct DuelingCriticConfig {
    /// State encoder configuration
    pub encoder: StateEncoderConfig,
    /// Number of discrete actions
    #[config(default = "5")]
    pub num_actions: usize,
    /// Hidden dimension
    #[config(default = "128")]
    pub hidden_dim: usize,
}

impl Default for DuelingCriticConfig {
    fn default() -> Self {
        Self {
            encoder: StateEncoderConfig::new(),
            num_actions: 5,
            hidden_dim: 128,
        }
    }
}

impl DuelingCriticConfig {
    /// Initialize dueling critic network
    pub fn init<B: Backend>(&self, device: &B::Device) -> DuelingCritic<B> {
        let encoder = self.encoder.init(device);
        let fc_hidden = LinearConfig::new(ENCODER_OUTPUT_DIM, self.hidden_dim).init(device);
        let value_stream = LinearConfig::new(self.hidden_dim, 1).init(device);
        let advantage_stream = LinearConfig::new(self.hidden_dim, self.num_actions).init(device);

        DuelingCritic {
            encoder,
            fc_hidden,
            value_stream,
            advantage_stream,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> DuelingCritic<B> {
    /// Forward pass returning Q-values for all actions
    ///
    /// Q(s, a) = V(s) + A(s, a) - mean(A(s, .))
    pub fn forward(&self, state: Tensor<B, 2>) -> Tensor<B, 2> {
        let encoded = self.encoder.forward(state);
        let hidden = self.fc_hidden.forward(encoded);
        let hidden = self.activation.forward(hidden);

        let value = self.value_stream.forward(hidden.clone());
        let advantage = self.advantage_stream.forward(hidden);

        // Center advantages (subtract mean)
        let advantage_mean = advantage.clone().mean_dim(1);
        let centered_advantage = advantage - advantage_mean;

        // Q = V + (A - mean(A))
        value + centered_advantage
    }
}
