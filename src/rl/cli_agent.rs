//! RL-powered crypto agent used by the legacy RL CLI runtime.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::coordinator::OrderPriority;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
#[cfg(feature = "onnx")]
use crate::rl::core::TOTAL_FEATURES;
use crate::rl::core::{
    CONTINUOUS_ACTION_DIM, ContinuousAction, DefaultStateEncoder, DiscreteAction,
    NUM_DISCRETE_ACTIONS, PnLRewardFunction, RawObservation, RewardFunction,
};
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport};
use crate::{AgentStatus, Domain, OrderIntent};

mod config;
mod execution_feedback;
mod intent_mapping;
mod market_state;
mod policy;
mod policy_output;
mod runtime;
#[cfg(test)]
mod tests;

pub use config::RLCryptoAgentConfig;

/// Internal position tracking
#[derive(Debug, Clone)]
struct InternalPosition {
    token_id: String,
    side: Side,
    shares: u64,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    unrealized_pnl: Decimal,
}

/// RL-Powered Crypto Agent
///
/// Uses reinforcement learning to make trading decisions for crypto markets.
/// The legacy RL CLI drives it directly instead of routing through a shared agent runtime.
pub struct RLCryptoAgent {
    config: RLCryptoAgentConfig,
    status: AgentStatus,

    // RL components
    encoder: Arc<DefaultStateEncoder>,
    reward_fn: Box<dyn RewardFunction + Send + Sync>,
    replay_buffer: Arc<RwLock<ReplayBuffer>>,

    // State
    current_obs: RawObservation,
    prev_obs: Option<RawObservation>,
    position: Option<InternalPosition>,

    // Metrics
    daily_pnl: Decimal,
    total_exposure: Decimal,
    step_count: u64,
    last_action: Option<ContinuousAction>,
    last_action_source: Option<String>,
    exploration_rate: f32,
    consecutive_failures: u32,

    #[cfg(feature = "onnx")]
    policy_model: Option<OnnxModel>,
}

impl RLCryptoAgent {
    /// Create a new RL Crypto Agent
    pub fn new(config: RLCryptoAgentConfig) -> Self {
        info!("Creating RLCryptoAgent: {} ({})", config.name, config.id);

        let buffer_size = config.rl_config.training.buffer_size;
        let exploration = config.exploration_rate;

        #[cfg(feature = "onnx")]
        let policy_model: Option<OnnxModel> = match config.policy_model_path.as_deref() {
            Some(path) if !path.trim().is_empty() => {
                match OnnxModel::load_for_vec_input(path, TOTAL_FEATURES) {
                    Ok(m) => {
                        info!(
                            agent = %config.id,
                            policy_path = %path,
                            input_dim = m.input_dim(),
                            output_dim = m.output_dim(),
                            policy_output = %config.policy_output,
                            "loaded RL policy ONNX model"
                        );
                        Some(m)
                    }
                    Err(e) => {
                        warn!(
                            agent = %config.id,
                            policy_path = %path,
                            error = %e,
                            "failed to load RL policy ONNX model; falling back to rule-based policy"
                        );
                        None
                    }
                }
            }
            _ => None,
        };

        #[cfg(not(feature = "onnx"))]
        if let Some(path) = config.policy_model_path.as_deref() {
            if !path.trim().is_empty() {
                warn!(
                    agent = %config.id,
                    policy_path = %path,
                    "policy_model_path is set but binary is built without --features onnx; using rule-based policy"
                );
            }
        }

        Self {
            config,
            status: AgentStatus::Initializing,
            encoder: Arc::new(DefaultStateEncoder::new()),
            reward_fn: Box::new(PnLRewardFunction::new()),
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(buffer_size))),
            current_obs: RawObservation::new(),
            prev_obs: None,
            position: None,
            daily_pnl: Decimal::ZERO,
            total_exposure: Decimal::ZERO,
            step_count: 0,
            last_action: None,
            last_action_source: None,
            exploration_rate: exploration,
            consecutive_failures: 0,
            #[cfg(feature = "onnx")]
            policy_model,
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(RLCryptoAgentConfig::default())
    }
}
