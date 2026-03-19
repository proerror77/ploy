//! Live Runtime for RL Strategy
//!
//! Provides a small compatibility layer for the CLI-driven RL agent while
//! retaining the replay-buffer runtime helpers added during the refactor.

use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::info;

use crate::error::Result;
use crate::rl::cli_agent::{RLCryptoAgent, RLCryptoAgentConfig};
use crate::rl::core::{HybridAction, RawObservation};
use crate::rl::memory::replay_buffer::Transition;
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport};
use crate::OrderIntent;

/// Runtime configuration for live RL trading.
#[derive(Debug, Clone)]
pub struct RLCryptoRuntimeConfig {
    /// RL agent configuration used by the CLI compatibility surface.
    pub agent_config: RLCryptoAgentConfig,
    /// Initial replay buffer capacity.
    pub replay_capacity: usize,
    /// Update frequency in steps.
    pub update_frequency: u32,
}

impl Default for RLCryptoRuntimeConfig {
    fn default() -> Self {
        Self {
            agent_config: RLCryptoAgentConfig::default(),
            replay_capacity: 10_000,
            update_frequency: 100,
        }
    }
}

/// Live runtime for RL-based crypto trading.
pub struct RLCryptoRuntime {
    agent: RLCryptoAgent,
    replay_buffer: Arc<RwLock<ReplayBuffer>>,
    step_count: u64,
}

impl RLCryptoRuntime {
    /// Create a new live runtime.
    pub fn new(config: RLCryptoRuntimeConfig) -> Self {
        info!("Initializing RLCryptoRuntime");
        Self {
            agent: RLCryptoAgent::new(config.agent_config),
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(config.replay_capacity))),
            step_count: 0,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        self.agent.start().await
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.agent.stop().await
    }

    pub async fn on_crypto_event(&mut self, event: &CryptoEvent) -> Result<Vec<OrderIntent>> {
        self.step_count += 1;
        self.agent
            .on_event(DomainEvent::Crypto(event.clone()))
            .await
    }

    pub async fn on_execution(&mut self, report: ExecutionReport) {
        self.agent.on_execution(report).await;
    }

    /// Record a transition in the replay buffer.
    pub async fn record_transition(
        &self,
        _state: RawObservation,
        action: HybridAction,
        reward: f32,
        _next_state: RawObservation,
        done: bool,
    ) {
        // The replay buffer still expects flattened vectors. Keep the current
        // placeholder encoding until the refactor wires a shared state encoder.
        let state_vec = vec![0.0f32; 64];
        let next_state_vec = vec![0.0f32; 64];

        let action_vec = match action {
            HybridAction::Continuous(c) => vec![c.position_size, c.stop_loss, c.take_profit],
            HybridAction::Discrete(_) => vec![0.0; 3],
            HybridAction::Hybrid(c, _) => vec![c.position_size, c.stop_loss, c.take_profit],
        };

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

    /// Get current step count.
    pub fn step_count(&self) -> u64 {
        self.step_count
    }
}

impl Default for RLCryptoRuntime {
    fn default() -> Self {
        Self::new(RLCryptoRuntimeConfig::default())
    }
}
