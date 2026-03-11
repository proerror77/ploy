use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
#[cfg(feature = "onnx")]
use crate::rl::core::TOTAL_FEATURES;
use crate::rl::core::{
    ContinuousAction, DefaultStateEncoder, PnLRewardFunction, RawObservation, RewardFunction,
};
use crate::rl::memory::ReplayBuffer;
use crate::AgentStatus;

use super::RLCryptoAgentConfig;

/// Internal position tracking
#[derive(Debug, Clone)]
pub(super) struct InternalPosition {
    pub(super) token_id: String,
    pub(super) side: crate::domain::Side,
    pub(super) shares: u64,
    pub(super) entry_price: Decimal,
    pub(super) entry_time: DateTime<Utc>,
    pub(super) unrealized_pnl: Decimal,
}

/// RL-Powered Crypto Agent
///
/// Uses reinforcement learning to make trading decisions for crypto markets.
/// The legacy RL CLI drives it directly instead of routing through a shared agent runtime.
pub struct RLCryptoAgent {
    pub(super) config: RLCryptoAgentConfig,
    pub(super) status: AgentStatus,

    pub(super) encoder: Arc<DefaultStateEncoder>,
    pub(super) reward_fn: Box<dyn RewardFunction + Send + Sync>,
    pub(super) replay_buffer: Arc<RwLock<ReplayBuffer>>,

    pub(super) current_obs: RawObservation,
    pub(super) prev_obs: Option<RawObservation>,
    pub(super) position: Option<InternalPosition>,

    pub(super) daily_pnl: Decimal,
    pub(super) total_exposure: Decimal,
    pub(super) step_count: u64,
    pub(super) last_action: Option<ContinuousAction>,
    pub(super) last_action_source: Option<String>,
    pub(super) exploration_rate: f32,
    pub(super) consecutive_failures: u32,

    #[cfg(feature = "onnx")]
    pub(super) policy_model: Option<OnnxModel>,
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
                    Ok(model) => {
                        info!(
                            agent = %config.id,
                            policy_path = %path,
                            input_dim = model.input_dim(),
                            output_dim = model.output_dim(),
                            policy_output = %config.policy_output,
                            "loaded RL policy ONNX model"
                        );
                        Some(model)
                    }
                    Err(error) => {
                        warn!(
                            agent = %config.id,
                            policy_path = %path,
                            error = %error,
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
