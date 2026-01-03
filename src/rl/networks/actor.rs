//! Actor Network (Policy)
//!
//! Neural network for action selection in policy gradient methods.
//!
//! Note: This module provides the network architecture interfaces.
//! The actual burn tensor operations need adjustment based on the
//! specific burn version and backend being used.

use burn::nn::{Linear, LinearConfig, Relu};
use burn::prelude::*;

use crate::rl::core::{CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS};
use super::encoder::{StateEncoderConfig, StateEncoderNetwork, ENCODER_OUTPUT_DIM};

/// Actor network configuration
#[derive(Config, Debug)]
pub struct ActorConfig {
    /// State encoder configuration
    pub encoder: StateEncoderConfig,
    /// Hidden dimension for actor head
    #[config(default = "128")]
    pub hidden_dim: usize,
    /// Whether to use continuous actions
    #[config(default = "true")]
    pub continuous: bool,
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            encoder: StateEncoderConfig::new(),
            hidden_dim: 128,
            continuous: true,
        }
    }
}

/// Actor network for continuous action spaces (PPO)
///
/// Outputs mean and log_std for a Gaussian policy.
#[derive(Module, Debug)]
pub struct Actor<B: Backend> {
    encoder: StateEncoderNetwork<B>,
    fc_hidden: Linear<B>,
    mean_head: Linear<B>,
    log_std_head: Linear<B>,
    activation: Relu,
}

impl ActorConfig {
    /// Initialize actor network for continuous actions
    pub fn init<B: Backend>(&self, device: &B::Device) -> Actor<B> {
        let encoder = self.encoder.init(device);

        let fc_hidden = LinearConfig::new(ENCODER_OUTPUT_DIM, self.hidden_dim).init(device);
        let mean_head = LinearConfig::new(self.hidden_dim, CONTINUOUS_ACTION_DIM).init(device);
        let log_std_head = LinearConfig::new(self.hidden_dim, CONTINUOUS_ACTION_DIM).init(device);

        Actor {
            encoder,
            fc_hidden,
            mean_head,
            log_std_head,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> Actor<B> {
    /// Forward pass returning (mean, log_std) for Gaussian policy
    pub fn forward(&self, state: Tensor<B, 2>) -> (Tensor<B, 2>, Tensor<B, 2>) {
        let encoded = self.encoder.forward(state);
        let hidden = self.fc_hidden.forward(encoded);
        let hidden = self.activation.forward(hidden);

        let mean = self.mean_head.forward(hidden.clone());
        let log_std = self.log_std_head.forward(hidden);

        // Clamp log_std for numerical stability
        let log_std = log_std.clamp(-20.0, 2.0);

        (mean, log_std)
    }

    /// Get deterministic action (use mean, no sampling)
    pub fn get_deterministic_action(&self, state: Tensor<B, 2>) -> Tensor<B, 2> {
        let (mean, _) = self.forward(state);
        mean.tanh()
    }
}

/// Discrete actor network for DQN-style agents
#[derive(Module, Debug)]
pub struct DiscreteActor<B: Backend> {
    encoder: StateEncoderNetwork<B>,
    fc_hidden: Linear<B>,
    action_head: Linear<B>,
    activation: Relu,
}

/// Configuration for discrete actor
#[derive(Config, Debug)]
pub struct DiscreteActorConfig {
    /// State encoder configuration
    pub encoder: StateEncoderConfig,
    /// Hidden dimension
    #[config(default = "128")]
    pub hidden_dim: usize,
}

impl Default for DiscreteActorConfig {
    fn default() -> Self {
        Self {
            encoder: StateEncoderConfig::new(),
            hidden_dim: 128,
        }
    }
}

impl DiscreteActorConfig {
    /// Initialize discrete actor network
    pub fn init<B: Backend>(&self, device: &B::Device) -> DiscreteActor<B> {
        let encoder = self.encoder.init(device);
        let fc_hidden = LinearConfig::new(ENCODER_OUTPUT_DIM, self.hidden_dim).init(device);
        let action_head = LinearConfig::new(self.hidden_dim, NUM_DISCRETE_ACTIONS).init(device);

        DiscreteActor {
            encoder,
            fc_hidden,
            action_head,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> DiscreteActor<B> {
    /// Forward pass returning action logits
    pub fn forward(&self, state: Tensor<B, 2>) -> Tensor<B, 2> {
        let encoded = self.encoder.forward(state);
        let hidden = self.fc_hidden.forward(encoded);
        let hidden = self.activation.forward(hidden);
        self.action_head.forward(hidden)
    }

    /// Get action probabilities (softmax over logits)
    pub fn action_probs(&self, state: Tensor<B, 2>) -> Tensor<B, 2> {
        let logits = self.forward(state);
        burn::tensor::activation::softmax(logits, 1)
    }
}
