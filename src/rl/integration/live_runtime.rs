//! Live Runtime for RL Strategy
//!
//! Provides runtime support for running RL strategies in live trading mode.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::rl::config::RLConfig;
use crate::rl::core::{ContinuousAction, DiscreteAction, HybridAction, RawObservation};
use crate::rl::memory::replay_buffer::Transition;
use crate::rl::memory::ReplayBuffer;

/// Runtime configuration for live RL trading
#[derive(Debug, Clone)]
pub struct RLCryptoRuntimeConfig {
    /// RL agent configuration
    pub agent_config: RLConfig,
    /// Initial replay buffer capacity
    pub replay_capacity: usize,
    /// Update frequency in steps
    pub update_frequency: u32,
}

impl Default for RLCryptoRuntimeConfig {
    fn default() -> Self {
        Self {
            agent_config: RLConfig::default(),
            replay_capacity: 10000,
            update_frequency: 100,
        }
    }
}

/// Live runtime for RL-based crypto trading
pub struct RLCryptoRuntime {
    config: RLCryptoRuntimeConfig,
    replay_buffer: Arc<RwLock<ReplayBuffer>>,
    step_count: u64,
}

impl RLCryptoRuntime {
    /// Create a new live runtime
    pub fn new(config: RLCryptoRuntimeConfig) -> Self {
        info!("Initializing RLCryptoRuntime");
        Self {
            config,
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(10000))),
            step_count: 0,
        }
    }

    /// Record a transition in the replay buffer
    pub async fn record_transition(
        &self,
        state: RawObservation,
        action: HybridAction,
        reward: f32,
        next_state: RawObservation,
        done: bool,
    ) {
        // Convert RawObservation to Vec<f32> for state encoding
        let state_vec = vec![0.0f32; 64]; // Placeholder - proper encoding needed
        let next_state_vec = vec![0.0f32; 64]; // Placeholder - proper encoding needed

        // Extract continuous action values
        let action_vec = match action {
            HybridAction::Continuous(c) => vec![c.position_size, c.stop_loss, c.take_profit],
            HybridAction::Discrete(_) => vec![0.0; 3],
            HybridAction::Hybrid(c, _) => vec![c.position_size, c.stop_loss, c.take_profit],
        };

        // Extract discrete action if present
        let discrete_action = match action {
            HybridAction::Discrete(d) => Some(d),
            HybridAction::Hybrid(_, d) => Some(d),
            _ => None,
        };

        let transition = Transition {
            state: state_vec,
            action: action_vec,
            discrete_action,
            reward,
            reward_signal: crate::rl::core::RewardSignal::PnL(reward),
            next_state: next_state_vec,
            done,
            log_prob: 0.0,
        };

        self.replay_buffer.write().await.push(transition);
    }

    /// Get current step count
    pub fn step_count(&self) -> u64 {
        self.step_count
    }
}

impl Default for RLCryptoRuntime {
    fn default() -> Self {
        Self::new(RLCryptoRuntimeConfig::default())
    }
}
