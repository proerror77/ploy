use chrono::{DateTime, Utc};
use ploy_operator_contracts::{DesiredState, ObservedState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLaunchSpec {
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: String,
    pub desired_state: DesiredState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub deployment_id: String,
    pub observed_state: ObservedState,
    pub last_heartbeat: DateTime<Utc>,
}
