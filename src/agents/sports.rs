use crate::platform::AgentRiskParams;
use serde::{Deserialize, Serialize};

/// Legacy sports runtime compatibility config.
///
/// Sports execution now runs through the canonical managed strategy runtime,
/// but bootstrap still deserializes this config shape for runtime wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsTradingConfig {
    /// DB account scope (single DB multi-account).
    #[serde(default = "default_account_id")]
    pub account_id: String,
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for SportsTradingConfig {
    fn default() -> Self {
        Self {
            account_id: default_account_id(),
            agent_id: "sports".into(),
            name: "NBA Comeback".into(),
            poll_interval_secs: 30,
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
    use super::*;

    #[test]
    fn defaults_match_managed_runtime_expectations() {
        let cfg = SportsTradingConfig::default();
        assert_eq!(cfg.account_id, "default");
        assert_eq!(cfg.agent_id, "sports");
        assert_eq!(cfg.name, "NBA Comeback");
        assert_eq!(cfg.poll_interval_secs, 30);
        assert_eq!(cfg.heartbeat_interval_secs, 5);
    }
}
