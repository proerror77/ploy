use crate::agent_runtime::AgentRiskParams;
use crate::rl::config::RLConfig;
use serde::{Deserialize, Serialize};

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
