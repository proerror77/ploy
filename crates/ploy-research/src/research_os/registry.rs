use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactorLifecycleStatus {
    Draft,
    Compiled,
    Evaluated,
    Candidate,
    DryRun,
    Approved,
    Production,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRegistryEntry {
    pub factor_id: String,
    pub factor_name: String,
    pub factor_family: String,
    pub status: FactorLifecycleStatus,
    pub hypothesis: String,
    pub economic_logic: String,
    pub dsl_source: String,
    pub dsl_hash: String,
    pub ast_json: serde_json::Value,
    pub target: String,
    pub horizon: String,
    pub created_by_agent: String,
    pub created_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorEvaluationRecord {
    pub eval_id: String,
    pub factor_id: String,
    pub run_id: String,
    pub data_snapshot_id: String,
    pub evaluator_version: String,
    pub passed_gate: bool,
    pub rejection_reason: Option<String>,
    pub metrics_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentTraceRecord {
    pub trace_id: String,
    pub run_id: String,
    pub parent_trace_id: Option<String>,
    pub event_type: String,
    pub agent_name: String,
    pub input_json: serde_json::Value,
    pub output_json: serde_json::Value,
    pub hash_prev: Option<String>,
    pub hash_current: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_lifecycle_status_uses_snake_case_contract() {
        let raw = serde_json::to_string(&FactorLifecycleStatus::DryRun).expect("serialize");
        assert_eq!(raw, "\"dry_run\"");
        let parsed: FactorLifecycleStatus = serde_json::from_str("\"candidate\"").expect("parse");
        assert_eq!(parsed, FactorLifecycleStatus::Candidate);
    }
}
