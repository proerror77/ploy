use chrono::{DateTime, Utc};
use ploy_operator_contracts::{DesiredState, ObservedState};
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerLaunchSpec {
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: String,
    pub desired_state: DesiredState,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub pid_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub deployment_id: String,
    pub observed_state: ObservedState,
    pub last_heartbeat: DateTime<Utc>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub last_error: Option<String>,
}
