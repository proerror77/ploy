use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Enabled,
    Draining,
    Disabled,
    Archived,
}

impl Default for DeploymentState {
    fn default() -> Self {
        Self::Enabled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedState {
    Starting,
    Running,
    Degraded,
    Paused,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentSummary {
    pub deployment_id: String,
    pub desired_state: DesiredState,
    pub observed_state: ObservedState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentStateSummary {
    pub enabled: usize,
    pub draining: usize,
    pub disabled: usize,
    pub archived: usize,
}

#[cfg(test)]
mod tests {
    use super::{DeploymentState, DeploymentSummary, DesiredState, ObservedState};
    use serde_json::json;

    #[test]
    fn deployment_state_serializes_as_snake_case() {
        let json = serde_json::to_string(&DeploymentState::Draining).expect("serialize");
        assert_eq!(json, "\"draining\"");
    }

    #[test]
    fn observed_state_serializes_as_running() {
        let json = serde_json::to_string(&ObservedState::Running).expect("serialize");
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn deployment_summary_uses_stable_wire_keys() {
        let summary = DeploymentSummary {
            deployment_id: "openclaw.default".to_string(),
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Degraded,
        };

        let value = serde_json::to_value(summary).expect("to_value");
        assert_eq!(
            value,
            json!({
                "deployment_id": "openclaw.default",
                "desired_state": "running",
                "observed_state": "degraded",
            })
        );
    }
}
