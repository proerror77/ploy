//! State Encoder Network
//!
//! Neural network for encoding raw state features into latent representations.

use burn::nn::{Linear, LinearConfig, Relu};
use burn::prelude::*;

use crate::rl::core::TOTAL_FEATURES;

/// Hidden dimension for encoder layers
pub const ENCODER_HIDDEN_DIM: usize = 256;

/// Output dimension of encoder (latent state)
pub const ENCODER_OUTPUT_DIM: usize = 64;

/// State encoder network configuration
#[derive(Config, Debug)]
pub struct StateEncoderConfig {
    /// Input dimension (raw features)
    #[config(default = "TOTAL_FEATURES")]
    pub input_dim: usize,
    /// Hidden layer dimension
    #[config(default = "ENCODER_HIDDEN_DIM")]
    pub hidden_dim: usize,
    /// Output dimension (latent representation)
    #[config(default = "ENCODER_OUTPUT_DIM")]
    pub output_dim: usize,
}

/// State encoder neural network
///
/// Transforms raw state features into a latent representation
/// suitable for the policy and value networks.
#[derive(Module, Debug)]
pub struct StateEncoderNetwork<B: Backend> {
    fc1: Linear<B>,
    fc2: Linear<B>,
    fc3: Linear<B>,
    activation: Relu,
}

impl StateEncoderConfig {
    /// Initialize the encoder network
    pub fn init<B: Backend>(&self, device: &B::Device) -> StateEncoderNetwork<B> {
        let fc1 = LinearConfig::new(self.input_dim, self.hidden_dim).init(device);
        let fc2 = LinearConfig::new(self.hidden_dim, self.hidden_dim / 2).init(device);
        let fc3 = LinearConfig::new(self.hidden_dim / 2, self.output_dim).init(device);

        StateEncoderNetwork {
            fc1,
            fc2,
            fc3,
            activation: Relu::new(),
        }
    }
}

impl<B: Backend> StateEncoderNetwork<B> {
    /// Forward pass through the encoder
    pub fn forward(&self, x: Tensor<B, 2>) -> Tensor<B, 2> {
        let x = self.fc1.forward(x);
        let x = self.activation.forward(x);
        let x = self.fc2.forward(x);
        let x = self.activation.forward(x);
        let x = self.fc3.forward(x);
        self.activation.forward(x)
    }

    /// Get the output dimension
    pub fn output_dim(&self) -> usize {
        ENCODER_OUTPUT_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn_ndarray::NdArray;

    type TestBackend = NdArray<f32>;

    #[test]
    fn test_encoder_forward() {
        let device = Default::default();
        let config = StateEncoderConfig::new();
        let encoder = config.init::<TestBackend>(&device);

        // Batch of 4 observations
        let input = Tensor::<TestBackend, 2>::zeros([4, TOTAL_FEATURES], &device);
        let output = encoder.forward(input);

        assert_eq!(output.dims(), [4, ENCODER_OUTPUT_DIM]);
    }
}
