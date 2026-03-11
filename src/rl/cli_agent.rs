//! RL-powered crypto agent used by the legacy RL CLI runtime.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::coordinator::OrderPriority;
#[cfg(feature = "onnx")]
use crate::rl::core::TOTAL_FEATURES;
use crate::rl::core::{
    ContinuousAction, DefaultStateEncoder, DiscreteAction, PnLRewardFunction, RawObservation,
    RewardFunction, CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS,
};
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport};
use crate::{AgentStatus, Domain, OrderIntent};

mod config;
mod execution_feedback;
mod market_state;
mod policy;
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

use chrono::Datelike;
use chrono::Timelike;

impl RLCryptoAgent {
    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn domain(&self) -> Domain {
        Domain::Crypto
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn risk_params(&self) -> &AgentRiskParams {
        &self.config.risk_params
    }

    pub async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        if !self.status.can_trade() {
            return Ok(vec![]);
        }

        match event {
            DomainEvent::Crypto(crypto_event) => Ok(self.process_crypto_event(&crypto_event)),
            DomainEvent::Tick(now) => {
                self.current_obs
                    .update_time_features(now.hour(), now.weekday().num_days_from_monday());
                self.update_position_prices();
                self.update_position_features();
                Ok(vec![])
            }
        }
    }

    pub async fn on_execution(&mut self, report: ExecutionReport) {
        self.handle_execution(&report);
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("[{}] Starting RL Crypto Agent...", self.config.id);
        self.status = AgentStatus::Running;
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        info!("[{}] Stopping RL Crypto Agent...", self.config.id);
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    pub fn pause(&mut self) {
        info!("[{}] Pausing...", self.config.id);
        self.status = AgentStatus::Paused;
    }

    pub fn resume(&mut self) {
        info!("[{}] Resuming...", self.config.id);
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    pub fn position_count(&self) -> usize {
        if self.position.is_some() {
            1
        } else {
            0
        }
    }

    pub fn total_exposure(&self) -> Decimal {
        self.total_exposure
    }

    pub fn daily_pnl(&self) -> Decimal {
        self.daily_pnl
    }
}
