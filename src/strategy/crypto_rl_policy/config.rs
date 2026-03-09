use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::platform::AgentRiskParams;

fn default_policy_output() -> String {
    "continuous".to_string()
}

fn default_observation_version() -> u32 {
    2
}

/// Runtime config shared by bootstrap-managed crypto RL policy wrappers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoRlPolicyConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub event_refresh_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub prefer_close_to_end: bool,
    pub default_shares: u64,
    pub max_entry_price: Decimal,
    pub cooldown_secs: u64,
    pub max_lob_snapshot_age_secs: u64,
    pub decision_interval_ms: u64,
    #[serde(default = "default_observation_version")]
    pub observation_version: u32,
    #[serde(default)]
    pub policy_model_path: Option<String>,
    #[serde(default = "default_policy_output")]
    pub policy_output: String,
    #[serde(default)]
    pub policy_model_version: Option<String>,
    #[serde(default)]
    pub exploration_rate: f32,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
}

impl Default for CryptoRlPolicyConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto_rl_policy".into(),
            name: "Crypto RL Policy".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into()],
            event_refresh_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            prefer_close_to_end: true,
            default_shares: 50,
            max_entry_price: dec!(0.70),
            cooldown_secs: 10,
            max_lob_snapshot_age_secs: 2,
            decision_interval_ms: 1000,
            observation_version: default_observation_version(),
            policy_model_path: None,
            policy_output: default_policy_output(),
            policy_model_version: None,
            exploration_rate: 0.0,
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
        }
    }
}
