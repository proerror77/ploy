use crate::platform::AgentRiskParams;
use serde::{Deserialize, Serialize};

/// Legacy politics runtime compatibility config.
///
/// Politics execution now runs through the canonical managed strategy runtime,
/// but bootstrap still deserializes this config shape for runtime wiring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticsTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for PoliticsTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "politics".into(),
            name: "Event Edge".into(),
            poll_interval_secs: 300,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_managed_runtime_expectations() {
        let cfg = PoliticsTradingConfig::default();
        assert_eq!(cfg.agent_id, "politics");
        assert_eq!(cfg.name, "Event Edge");
        assert_eq!(cfg.poll_interval_secs, 300);
        assert_eq!(cfg.heartbeat_interval_secs, 5);
    }
}
