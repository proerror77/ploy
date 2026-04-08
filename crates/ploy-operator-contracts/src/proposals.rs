use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalActionKind {
    PauseDeployment,
    DrainDeployment,
    ReduceMaxExposure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SafetyProposal {
    pub proposal_id: String,
    pub action_kind: ProposalActionKind,
    pub target_deployment_id: String,
    pub status: ProposalStatus,
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub source_run_id: Option<String>,
    #[serde(default)]
    pub proposed_max_gross_exposure: Option<Decimal>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalCreateRequest {
    pub action_kind: ProposalActionKind,
    pub target_deployment_id: String,
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub source_run_id: Option<String>,
    #[serde(default)]
    pub proposed_max_gross_exposure: Option<Decimal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalDecisionRequest {
    #[serde(default)]
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalSnapshotEvent {
    pub proposals: Vec<SafetyProposal>,
}

#[cfg(test)]
mod tests {
    use super::{
        ProposalActionKind, ProposalCreateRequest, ProposalDecisionRequest, ProposalSnapshotEvent,
        ProposalStatus, SafetyProposal,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn safety_proposal_uses_stable_wire_shape() {
        let created_at = Utc::now();
        let decided_at = Utc::now();
        let value = serde_json::to_value(SafetyProposal {
            proposal_id: "proposal-1".to_string(),
            action_kind: ProposalActionKind::ReduceMaxExposure,
            target_deployment_id: "example.paper".to_string(),
            status: ProposalStatus::Pending,
            rationale: "gross exposure exceeded threshold".to_string(),
            evidence: vec!["gross_exposure=6.10".to_string()],
            source_run_id: Some("run-1".to_string()),
            proposed_max_gross_exposure: Some(Decimal::new(400, 2)),
            created_at,
            decided_at: Some(decided_at),
            decision_note: Some("operator review pending".to_string()),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "proposal_id": "proposal-1",
                "action_kind": "reduce_max_exposure",
                "target_deployment_id": "example.paper",
                "status": "pending",
                "rationale": "gross exposure exceeded threshold",
                "evidence": ["gross_exposure=6.10"],
                "source_run_id": "run-1",
                "proposed_max_gross_exposure": "4.00",
                "created_at": created_at,
                "decided_at": decided_at,
                "decision_note": "operator review pending",
            })
        );
    }

    #[test]
    fn proposal_request_and_snapshot_event_use_stable_wire_shape() {
        let request = serde_json::to_value(ProposalCreateRequest {
            action_kind: ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "pnl regression crossed threshold".to_string(),
            evidence: vec!["net_pnl=-2.50".to_string()],
            source_run_id: Some("run-2".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("request");
        assert_eq!(
            request,
            json!({
                "action_kind": "pause_deployment",
                "target_deployment_id": "example.paper",
                "rationale": "pnl regression crossed threshold",
                "evidence": ["net_pnl=-2.50"],
                "source_run_id": "run-2",
                "proposed_max_gross_exposure": null,
            })
        );

        let decision = serde_json::to_value(ProposalDecisionRequest {
            decision_note: Some("approved by operator".to_string()),
        })
        .expect("decision");
        assert_eq!(decision, json!({ "decision_note": "approved by operator" }));

        let snapshot = serde_json::to_value(ProposalSnapshotEvent {
            proposals: vec![SafetyProposal {
                proposal_id: "proposal-2".to_string(),
                action_kind: ProposalActionKind::PauseDeployment,
                target_deployment_id: "example.paper".to_string(),
                status: ProposalStatus::Approved,
                rationale: "operator approved pause".to_string(),
                evidence: vec![],
                source_run_id: None,
                proposed_max_gross_exposure: None,
                created_at: Utc::now(),
                decided_at: None,
                decision_note: None,
            }],
        })
        .expect("snapshot");
        assert_eq!(snapshot["proposals"][0]["action_kind"], json!("pause_deployment"));
    }
}
