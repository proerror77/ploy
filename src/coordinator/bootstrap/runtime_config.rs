use crate::platform::AgentRiskParams;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsRuntimeConfig {
    #[serde(default = "default_account_id")]
    pub account_id: String,
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for SportsRuntimeConfig {
    fn default() -> Self {
        Self {
            account_id: default_account_id(),
            agent_id: "sports".to_string(),
            name: "NBA Comeback".to_string(),
            poll_interval_secs: 30,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticsRuntimeConfig {
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for PoliticsRuntimeConfig {
    fn default() -> Self {
        Self {
            agent_id: "politics".to_string(),
            name: "Event Edge".to_string(),
            poll_interval_secs: 300,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

fn default_account_id() -> String {
    "default".to_string()
}

#[cfg(test)]
mod tests {
    use super::{PoliticsRuntimeConfig, SportsRuntimeConfig};

    #[test]
    fn sports_runtime_config_defaults_match_bootstrap_expectations() {
        let cfg = SportsRuntimeConfig::default();
        assert_eq!(cfg.account_id, "default");
        assert_eq!(cfg.agent_id, "sports");
        assert_eq!(cfg.poll_interval_secs, 30);
    }

    #[test]
    fn politics_runtime_config_defaults_match_bootstrap_expectations() {
        let cfg = PoliticsRuntimeConfig::default();
        assert_eq!(cfg.agent_id, "politics");
        assert_eq!(cfg.poll_interval_secs, 300);
    }
}
