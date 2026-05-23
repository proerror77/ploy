use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn horizon_for_target(target: &str) -> &'static str {
    match target {
        "reprice_pnl_5s" | "full_depth_reprice_pnl_5s" | "tradeable_full_depth_reprice_pnl_5s" => {
            "5s"
        }
        "reprice_pnl_10s"
        | "full_depth_reprice_pnl_10s"
        | "tradeable_full_depth_reprice_pnl_10s" => "10s",
        "reprice_pnl_30s"
        | "full_depth_reprice_pnl_30s"
        | "tradeable_full_depth_reprice_pnl_30s" => "30s",
        "reprice_pnl_60s"
        | "full_depth_reprice_pnl_60s"
        | "tradeable_full_depth_reprice_pnl_60s" => "60s",
        "settlement_pnl"
        | "settlement_executable_pnl"
        | "full_depth_settlement_executable_pnl"
        | "tradeable_full_depth_settlement_pnl" => "5m",
        _ => "5m",
    }
}

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
    #[serde(default)]
    pub runtime_contract: serde_json::Value,
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
    pub dataset_start_ts: DateTime<Utc>,
    pub dataset_end_ts: DateTime<Utc>,
    pub evidence_stage: String,
    pub evaluation_kind: String,
    pub candidate_replay_id: Option<String>,
    pub evaluator_version: String,
    #[serde(default)]
    pub runtime_contract: serde_json::Value,
    pub passed_gate: bool,
    pub promotion_decision: String,
    pub promotion_status: String,
    #[serde(default)]
    pub blockers_json: serde_json::Value,
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
    pub data_snapshot_id: Option<String>,
    pub dsl_hash: Option<String>,
    pub artifact_kind: String,
    pub evidence_stage: String,
    pub promotion_decision: Option<String>,
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

    #[test]
    fn target_horizon_contract_covers_reprice_and_settlement_targets() {
        assert_eq!(horizon_for_target("reprice_pnl_5s"), "5s");
        assert_eq!(horizon_for_target("full_depth_reprice_pnl_10s"), "10s");
        assert_eq!(
            horizon_for_target("tradeable_full_depth_reprice_pnl_30s"),
            "30s"
        );
        assert_eq!(horizon_for_target("full_depth_reprice_pnl_60s"), "60s");
        assert_eq!(
            horizon_for_target("tradeable_full_depth_settlement_pnl"),
            "5m"
        );
    }
}
