//! RL-powered crypto agent used by the legacy RL CLI runtime.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::agent_runtime::AgentRiskParams;
use crate::domain::Side;
use crate::error::Result;
#[cfg(feature = "onnx")]
use crate::ml::OnnxModel;
use crate::coordinator::OrderPriority;
use crate::rl::config::RLConfig;
#[cfg(feature = "onnx")]
use crate::rl::core::TOTAL_FEATURES;
use crate::rl::core::{
    ContinuousAction, DefaultStateEncoder, DiscreteAction, PnLRewardFunction, RawObservation,
    RewardFunction, CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS,
};
use crate::rl::memory::ReplayBuffer;
use crate::rl::{CryptoEvent, DomainEvent, ExecutionReport, ExecutionStatus};
use crate::{AgentStatus, Domain, OrderIntent};

mod execution_feedback;
mod market_state;
mod policy;

fn default_policy_output() -> String {
    "continuous".to_string()
}

/// RL Crypto Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RLCryptoAgentConfig {
    /// Agent ID
    pub id: String,
    /// Agent name
    pub name: String,
    /// Coins to monitor (e.g., "BTC", "ETH", "SOL")
    pub coins: Vec<String>,
    /// UP token ID
    pub up_token_id: String,
    /// DOWN token ID
    pub down_token_id: String,
    /// Binance symbol (e.g., "BTCUSDT")
    pub binance_symbol: String,
    /// Market slug
    pub market_slug: String,
    /// Default order size (shares)
    pub default_shares: u64,
    /// Risk parameters
    pub risk_params: AgentRiskParams,
    /// RL configuration
    pub rl_config: RLConfig,
    /// Enable online learning
    pub online_learning: bool,
    /// Initial exploration rate
    pub exploration_rate: f32,

    /// Optional ONNX policy model path for action selection.
    ///
    /// If set, the agent will use this model (when built with `--features onnx`) instead of the
    /// rule-based baseline policy.
    #[serde(default)]
    pub policy_model_path: Option<String>,

    /// How to interpret the policy model output.
    ///
    /// Supported values:
    /// - "continuous" (default): expects >= 5 floats: position_delta, side_preference, urgency, tp_adjustment, sl_adjustment
    /// - "continuous_mean_logstd": expects >= 10 floats: mean(5) then log_std(5), uses mean only
    /// - "discrete_logits": expects 5 floats, logits for [Hold, BuyUp, BuyDown, SellPosition, EnterHedge]
    /// - "discrete_probs": expects 5 floats, probabilities for the same discrete actions
    #[serde(default = "default_policy_output")]
    pub policy_output: String,

    /// Optional policy model version label recorded in order metadata.
    #[serde(default)]
    pub policy_model_version: Option<String>,
}

impl Default for RLCryptoAgentConfig {
    fn default() -> Self {
        Self {
            id: "rl-crypto-agent-1".to_string(),
            name: "RL Crypto Agent".to_string(),
            coins: vec!["BTC".to_string()],
            up_token_id: String::new(),
            down_token_id: String::new(),
            binance_symbol: "BTCUSDT".to_string(),
            market_slug: String::new(),
            default_shares: 100,
            risk_params: AgentRiskParams::default(),
            rl_config: RLConfig::default(),
            online_learning: true,
            exploration_rate: 0.1,
            policy_model_path: None,
            policy_output: default_policy_output(),
            policy_model_version: None,
        }
    }
}

impl RLCryptoAgentConfig {
    /// Create config for a specific market
    pub fn for_market(
        id: &str,
        market_slug: &str,
        up_token: &str,
        down_token: &str,
        symbol: &str,
    ) -> Self {
        Self {
            id: id.to_string(),
            name: format!("RL Agent - {}", symbol),
            coins: vec![symbol.replace("USDT", "")],
            up_token_id: up_token.to_string(),
            down_token_id: down_token.to_string(),
            binance_symbol: symbol.to_string(),
            market_slug: market_slug.to_string(),
            ..Default::default()
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rl::{ExecutionStatus, QuoteData};

    fn make_crypto_event(
        symbol: &str,
        spot: Decimal,
        up_ask: Decimal,
        down_ask: Decimal,
    ) -> CryptoEvent {
        CryptoEvent {
            symbol: symbol.to_string(),
            spot_price: spot,
            round_slug: None,
            quotes: Some(QuoteData {
                up_bid: up_ask - dec!(0.01),
                up_ask,
                down_bid: down_ask - dec!(0.01),
                down_ask,
                timestamp: Utc::now(),
            }),
            momentum: Some([0.002, 0.001, 0.0005, 0.0001]),
        }
    }

    #[tokio::test]
    async fn test_rl_agent_creation() {
        let agent = RLCryptoAgent::with_defaults();
        assert_eq!(agent.id(), "rl-crypto-agent-1");
        assert_eq!(agent.status(), AgentStatus::Initializing);
        assert_eq!(agent.domain(), Domain::Crypto);
    }

    #[tokio::test]
    async fn test_rl_agent_lifecycle() {
        let mut agent = RLCryptoAgent::with_defaults();

        agent.start().await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Running);
        assert!(agent.status().can_trade());

        agent.pause();
        assert_eq!(agent.status(), AgentStatus::Paused);
        assert!(!agent.status().can_trade());

        agent.resume();
        assert_eq!(agent.status(), AgentStatus::Running);

        agent.stop().await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn test_rl_signal_on_good_sum() {
        let config = RLCryptoAgentConfig {
            coins: vec!["BTC".to_string()],
            exploration_rate: 0.0,
            ..Default::default()
        };
        let mut agent = RLCryptoAgent::new(config);
        agent.start().await.unwrap();

        // Create favorable signal (sum = 0.95 < 0.96)
        let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.47), dec!(0.48));
        let domain_event = DomainEvent::Crypto(event);

        let intents = agent.on_event(domain_event).await.unwrap();

        // Should generate a buy signal
        assert!(!intents.is_empty(), "Should generate intent on good sum");
        assert!(intents[0].is_buy);
        assert_eq!(intents[0].domain, Domain::Crypto);
    }

    #[tokio::test]
    async fn test_rl_no_signal_on_high_sum() {
        let config = RLCryptoAgentConfig {
            coins: vec!["BTC".to_string()],
            exploration_rate: 0.0,
            ..Default::default()
        };
        let mut agent = RLCryptoAgent::new(config);
        agent.start().await.unwrap();

        // Create unfavorable signal (sum = 1.0 > 0.96)
        let event = make_crypto_event("BTCUSDT", dec!(50000), dec!(0.50), dec!(0.50));
        let domain_event = DomainEvent::Crypto(event);

        let intents = agent.on_event(domain_event).await.unwrap();

        // Should not generate signal
        assert!(intents.is_empty());
    }

    #[tokio::test]
    async fn test_exploration_decay() {
        let mut config = RLCryptoAgentConfig::default();
        config.exploration_rate = 0.5;
        config.rl_config.training.exploration_decay = 0.9;
        config.rl_config.training.exploration_min = 0.01;

        let mut agent = RLCryptoAgent::new(config);

        assert_eq!(agent.exploration_rate, 0.5);

        // Simulate multiple decays
        for _ in 0..10 {
            agent.decay_exploration();
        }

        // Should have decayed
        assert!(agent.exploration_rate < 0.5);
        assert!(agent.exploration_rate >= 0.01);
    }

    #[tokio::test]
    async fn test_position_tracking() {
        let config = RLCryptoAgentConfig {
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            ..Default::default()
        };
        let mut agent = RLCryptoAgent::new(config);
        agent.start().await.unwrap();

        assert_eq!(agent.position_count(), 0);
        assert_eq!(agent.total_exposure(), Decimal::ZERO);

        // Simulate successful fill
        let report = ExecutionReport {
            intent_id: uuid::Uuid::new_v4(),
            agent_id: agent.id().to_string(),
            order_id: Some("order-1".to_string()),
            status: ExecutionStatus::Filled,
            filled_shares: 100,
            avg_fill_price: Some(dec!(0.50)),
            fees: Decimal::ZERO,
            error_message: None,
            executed_at: Utc::now(),
            latency_ms: 50,
        };

        agent.on_execution(report).await;

        assert_eq!(agent.position_count(), 1);
        assert!(agent.total_exposure() > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_submitted_execution_does_not_pause_agent() {
        let mut agent = RLCryptoAgent::with_defaults();
        agent.start().await.unwrap();

        for _ in 0..3 {
            agent
                .on_execution(ExecutionReport {
                    intent_id: uuid::Uuid::new_v4(),
                    agent_id: agent.id().to_string(),
                    order_id: Some("intent-123".to_string()),
                    status: ExecutionStatus::Submitted,
                    filled_shares: 0,
                    avg_fill_price: None,
                    fees: Decimal::ZERO,
                    error_message: None,
                    executed_at: Utc::now(),
                    latency_ms: 5,
                })
                .await;
        }

        assert_eq!(agent.status(), AgentStatus::Running);
    }
}
