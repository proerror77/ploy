//! Live Runtime for RL Strategy
//!
//! Provides runtime support for running RL strategies in live trading mode.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::rl::config::RLConfig;
use crate::rl::memory::ReplayBuffer;
use crate::rl::networks::{Actor, Critic};

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
        state: crate::rl::core::RawObservation,
        action: crate::rl::core::HybridAction,
        reward: f64,
        next_state: crate::rl::core::RawObservation,
        done: bool,
    ) {
        let transition = crate::rl::core::RewardTransition {
            state,
            action,
            reward,
            next_state,
            done,
        };
        self.replay_buffer.write().await.push(transition);
        self.step_count += 1;
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
