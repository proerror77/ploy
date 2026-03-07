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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDeployment {
    pub deployment_id: String,
    pub plugin_id: String,
    pub account_id: String,
    pub state: DeploymentState,
}
