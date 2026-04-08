use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Started,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolCallRecord {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunEvaluation {
    pub usefulness: String,
    pub research_reports: usize,
    pub oversight_alerts: usize,
    pub operator_recommendations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRuntimeContextSummary {
    #[serde(default)]
    pub deployment_sample: Vec<String>,
    #[serde(default)]
    pub oversight_signal_summary: Vec<String>,
    #[serde(default)]
    pub oversight_playbook_summary: Vec<String>,
    #[serde(default)]
    pub diagnostic_candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunOutputSummary {
    #[serde(default)]
    pub research_report_summaries: Vec<String>,
    #[serde(default)]
    pub oversight_alert_summaries: Vec<String>,
    #[serde(default)]
    pub operator_recommendation_summaries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunRecord {
    pub run_id: String,
    pub cycle_kind: String,
    pub status: AgentRunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub session_id: Option<String>,
    pub model: String,
    pub platform_status: Option<String>,
    pub deployment_count: usize,
    pub oversight_signal_count: usize,
    pub oversight_playbook_count: usize,
    pub total_cost_usd: Option<f64>,
    pub tool_calls: Vec<AgentToolCallRecord>,
    pub research_reports: usize,
    pub oversight_alerts: usize,
    pub operator_recommendations: usize,
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub runtime_context: Option<AgentRuntimeContextSummary>,
    #[serde(default)]
    pub output_summary: Option<AgentRunOutputSummary>,
    pub evaluation: Option<AgentRunEvaluation>,
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRunEvaluation, AgentRunOutputSummary, AgentRunRecord, AgentRunStatus,
        AgentRuntimeContextSummary, AgentToolCallRecord,
    };
    use serde_json::json;

    #[test]
    fn agent_run_record_uses_stable_wire_shape() {
        let value = serde_json::to_value(AgentRunRecord {
            run_id: "run-123".to_string(),
            cycle_kind: "research_oversight".to_string(),
            status: AgentRunStatus::Succeeded,
            started_at: "2026-04-07T00:00:00Z".to_string(),
            finished_at: Some("2026-04-07T00:00:10Z".to_string()),
            session_id: Some("session-1".to_string()),
            model: "sonnet".to_string(),
            platform_status: Some("running".to_string()),
            deployment_count: 2,
            oversight_signal_count: 1,
            oversight_playbook_count: 1,
            total_cost_usd: Some(0.0123),
            tool_calls: vec![AgentToolCallRecord {
                name: "mcp__research__check_oversight".to_string(),
                status: "called".to_string(),
            }],
            research_reports: 1,
            oversight_alerts: 1,
            operator_recommendations: 1,
            failure_reason: None,
            runtime_context: Some(AgentRuntimeContextSummary {
                deployment_sample: vec!["example.paper".to_string()],
                oversight_signal_summary: vec!["critical:pnl_regression:example.paper".to_string()],
                oversight_playbook_summary: vec!["pause_review:example.paper".to_string()],
                diagnostic_candidates: vec!["example.paper".to_string()],
            }),
            output_summary: Some(AgentRunOutputSummary {
                research_report_summaries: vec!["diagnostic:example.paper:completed".to_string()],
                oversight_alert_summaries: vec!["critical:pnl_regression:example.paper".to_string()],
                operator_recommendation_summaries: vec![
                    "diagnose:example.paper".to_string()
                ],
            }),
            evaluation: Some(AgentRunEvaluation {
                usefulness: "high".to_string(),
                research_reports: 1,
                oversight_alerts: 1,
                operator_recommendations: 1,
            }),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "run_id": "run-123",
                "cycle_kind": "research_oversight",
                "status": "succeeded",
                "started_at": "2026-04-07T00:00:00Z",
                "finished_at": "2026-04-07T00:00:10Z",
                "session_id": "session-1",
                "model": "sonnet",
                "platform_status": "running",
                "deployment_count": 2,
                "oversight_signal_count": 1,
                "oversight_playbook_count": 1,
                "total_cost_usd": 0.0123,
                "tool_calls": [{
                    "name": "mcp__research__check_oversight",
                    "status": "called"
                }],
                "research_reports": 1,
                "oversight_alerts": 1,
                "operator_recommendations": 1,
                "failure_reason": null,
                "runtime_context": {
                    "deployment_sample": ["example.paper"],
                    "oversight_signal_summary": ["critical:pnl_regression:example.paper"],
                    "oversight_playbook_summary": ["pause_review:example.paper"],
                    "diagnostic_candidates": ["example.paper"]
                },
                "output_summary": {
                    "research_report_summaries": ["diagnostic:example.paper:completed"],
                    "oversight_alert_summaries": ["critical:pnl_regression:example.paper"],
                    "operator_recommendation_summaries": ["diagnose:example.paper"]
                },
                "evaluation": {
                    "usefulness": "high",
                    "research_reports": 1,
                    "oversight_alerts": 1,
                    "operator_recommendations": 1
                }
            })
        );
    }
}
