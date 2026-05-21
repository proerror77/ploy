use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchBudget {
    pub max_candidates_per_day: usize,
    pub max_backtests_per_day: usize,
    pub max_llm_calls_per_day: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchManagerInput {
    #[serde(default)]
    pub latest_runs: serde_json::Value,
    #[serde(default)]
    pub factor_registry_summary: serde_json::Value,
    #[serde(default)]
    pub rejected_factor_patterns: serde_json::Value,
    #[serde(default)]
    pub market_data_health: serde_json::Value,
    #[serde(default)]
    pub research_trace_summary: serde_json::Value,
    pub research_budget: ResearchBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchManagerPlan {
    pub theme: String,
    pub candidate_count: usize,
    pub search_depth: usize,
    pub priority: String,
    pub evidence_stage: String,
    pub actions: Vec<String>,
}

pub fn build_research_manager_plan(input: &ResearchManagerInput) -> ResearchManagerPlan {
    let candidate_count = input.research_budget.max_candidates_per_day.clamp(1, 80);
    if has_unhealthy_market_data(&input.market_data_health) {
        return ResearchManagerPlan {
            theme: "fix_data".to_string(),
            candidate_count: 0,
            search_depth: 0,
            priority: "high".to_string(),
            evidence_stage: "diagnostic".to_string(),
            actions: vec![
                "repair critical market data surfaces before alpha expansion".to_string(),
                "rerun data health and snapshot coverage diagnostics".to_string(),
            ],
        };
    }
    if trace_prefers_fix_runtime(&input.research_trace_summary) {
        return ResearchManagerPlan {
            theme: "fix_runtime".to_string(),
            candidate_count: 0,
            search_depth: 0,
            priority: "high".to_string(),
            evidence_stage: "runtime_parity".to_string(),
            actions: vec![
                "read the Research OS trace blockers before generating new candidates".to_string(),
                "dispatch runtime-candidate-replay.yml for the trace-selected runtime score"
                    .to_string(),
                "keep promotion/config handoff blocked until typed runtime contract evidence is clean"
                    .to_string(),
            ],
        };
    }
    if trace_prefers_revise_prior(&input.research_trace_summary) {
        return ResearchManagerPlan {
            theme: "revise_prior".to_string(),
            candidate_count,
            search_depth: 1,
            priority: "high".to_string(),
            evidence_stage: "factor_attribution".to_string(),
            actions: vec![
                "seed the next typed prior from Research OS rejected families and blockers"
                    .to_string(),
                "avoid repeating trace-blocked factor surfaces before the next hosted search"
                    .to_string(),
            ],
        };
    }
    if trace_prefers_continue_search(&input.research_trace_summary) {
        return ResearchManagerPlan {
            theme: "continue_search".to_string(),
            candidate_count,
            search_depth: 2,
            priority: "medium".to_string(),
            evidence_stage: "walk_forward".to_string(),
            actions: vec![
                "continue hosted artifact search using trace-qualified candidates as priors"
                    .to_string(),
                "require runtime candidate replay before dry-run handoff".to_string(),
            ],
        };
    }
    if replay_parity_missing_for_ready_handoff(&input.latest_runs) {
        return ResearchManagerPlan {
            theme: "fix_runtime".to_string(),
            candidate_count: 0,
            search_depth: 0,
            priority: "high".to_string(),
            evidence_stage: "runtime_parity".to_string(),
            actions: vec![
                "run candidate strategy runtime replay for the exact runtime score".to_string(),
                "block dry-run handoff until replay parity evidence is ready".to_string(),
            ],
        };
    }
    if alpha_search_stagnated(&input.latest_runs, &input.rejected_factor_patterns) {
        return ResearchManagerPlan {
            theme: "revise_prior".to_string(),
            candidate_count,
            search_depth: 1,
            priority: "medium".to_string(),
            evidence_stage: "factor_attribution".to_string(),
            actions: vec![
                "generate a bounded typed prior from weak dimensions".to_string(),
                "penalize rejected or crowded factor subtrees".to_string(),
            ],
        };
    }
    if mcts_has_selected_nodes(&input.latest_runs) {
        return ResearchManagerPlan {
            theme: "continue_search".to_string(),
            candidate_count,
            search_depth: 2,
            priority: "medium".to_string(),
            evidence_stage: "walk_forward".to_string(),
            actions: vec![
                "continue bounded MCTS-guided hosted artifact search".to_string(),
                "require runtime candidate replay before dry-run handoff".to_string(),
            ],
        };
    }
    ResearchManagerPlan {
        theme: "revise_prior".to_string(),
        candidate_count,
        search_depth: 1,
        priority: "low".to_string(),
        evidence_stage: "factor_attribution".to_string(),
        actions: vec![
            "create a new typed prior because no MCTS continuation node is available".to_string(),
        ],
    }
}

pub fn validate_evidence_stage(stage: &str) -> bool {
    matches!(
        stage,
        "diagnostic"
            | "factor_attribution"
            | "walk_forward"
            | "runtime_parity"
            | "dry_run_candidate"
    )
}

pub fn summarize_research_trace(trace: &serde_json::Value) -> serde_json::Value {
    if trace.is_null() {
        return serde_json::json!({});
    }

    let promotion_decision = trace
        .get("promotion_decision")
        .or_else(|| trace.pointer("/experiment_trace/0/output_json/decision"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let qualified_count = trace
        .pointer("/experiment_trace/0/output_json/qualified_count")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            trace
                .get("qualified_count")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or_else(|| {
            trace
                .get("factor_evaluations")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| bool_path(row, &["passed_gate"]))
                        .count() as u64
                })
                .unwrap_or(0)
        });
    let blocked_count = trace
        .pointer("/experiment_trace/0/output_json/blocked_count")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            trace
                .get("blocked_count")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or_else(|| {
            trace
                .get("factor_evaluations")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| !bool_path(row, &["passed_gate"]))
                        .count() as u64
                })
                .unwrap_or(0)
        });

    let mut blockers = BTreeSet::new();
    collect_blockers(trace, &mut blockers);
    let mut rejected_families = BTreeSet::new();
    collect_rejected_families(trace, &mut rejected_families);
    let mut runtime_replay_requests = Vec::new();
    collect_runtime_replay_requests(trace, &mut runtime_replay_requests);

    serde_json::json!({
        "source": "autofactor-research-trace",
        "latest_decisions": {
            "promotion_decision": promotion_decision,
            "qualified_count": qualified_count,
            "blocked_count": blocked_count,
        },
        "latest_blockers": blockers.into_iter().collect::<Vec<_>>(),
        "runtime_replay_requests": runtime_replay_requests,
        "rejected_factor_families": rejected_families.into_iter().collect::<Vec<_>>(),
        "blocked_runtime_contract_count": count_blocked_runtime_contracts(trace),
    })
}

fn has_unhealthy_market_data(value: &serde_json::Value) -> bool {
    bool_path(value, &["has_critical_missing_surface"])
        || bool_path(value, &["critical_surface_stale"])
        || number_path(value, &["stale_source_count"]).unwrap_or(0.0) > 0.0
        || array_contains(value, &["missing_blocks_promotion"])
}

fn trace_prefers_fix_runtime(value: &serde_json::Value) -> bool {
    if value.is_null() {
        return false;
    }
    number_path(value, &["blocked_runtime_contract_count"]).unwrap_or(0.0) > 0.0
        || array_contains(value, &["runtime_replay_requests"])
        || string_array_path(value, &["latest_blockers"])
            .iter()
            .any(|blocker| {
                blocker.contains("runtime_contract")
                    || blocker.contains("runtime_replay")
                    || blocker.contains("missing_runtime_strategy_mapping")
                    || blocker.contains("candidate_strategy_replay")
                    || blocker.contains("unsupported_runtime_formula_semantics")
            })
}

fn trace_prefers_revise_prior(value: &serde_json::Value) -> bool {
    if value.is_null() || value == &serde_json::json!({}) {
        return false;
    }
    let qualified_count =
        number_path(value, &["latest_decisions", "qualified_count"]).unwrap_or(0.0);
    if qualified_count > 0.0 {
        return false;
    }
    let decision = value
        .pointer("/latest_decisions/promotion_decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    matches!(decision, "blocked" | "rejected" | "reject" | "no_qualified")
        || array_contains(value, &["latest_blockers"])
        || array_contains(value, &["rejected_factor_families"])
}

fn trace_prefers_continue_search(value: &serde_json::Value) -> bool {
    if value.is_null() || value == &serde_json::json!({}) {
        return false;
    }
    let qualified_count =
        number_path(value, &["latest_decisions", "qualified_count"]).unwrap_or(0.0);
    let decision = value
        .pointer("/latest_decisions/promotion_decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    qualified_count > 0.0 || matches!(decision, "qualified" | "continue" | "continue_search")
}

fn replay_parity_missing_for_ready_handoff(value: &serde_json::Value) -> bool {
    bool_path(value, &["ready_handoff"]) && !bool_path(value, &["replay_parity_ready"])
}

fn alpha_search_stagnated(
    latest_runs: &serde_json::Value,
    rejected_factor_patterns: &serde_json::Value,
) -> bool {
    bool_path(latest_runs, &["reward_stagnation"])
        || bool_path(latest_runs, &["stagnated"])
        || number_path(rejected_factor_patterns, &["rejected_count"]).unwrap_or(0.0) > 0.0
}

fn mcts_has_selected_nodes(value: &serde_json::Value) -> bool {
    number_path(value, &["selected_node_count"]).unwrap_or(0.0) > 0.0
        || value
            .pointer("/mcts_expansion_plan/selected_nodes")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|nodes| !nodes.is_empty())
}

fn bool_path(value: &serde_json::Value, path: &[&str]) -> bool {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn number_path(value: &serde_json::Value, path: &[&str]) -> Option<f64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(serde_json::Value::as_f64)
}

fn array_contains(value: &serde_json::Value, path: &[&str]) -> bool {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

fn string_array_path(value: &serde_json::Value, path: &[&str]) -> Vec<String> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn collect_blockers(value: &serde_json::Value, blockers: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "blockers" {
                    if let Some(items) = child.as_array() {
                        for item in items.iter().filter_map(serde_json::Value::as_str) {
                            blockers.insert(item.to_string());
                        }
                    }
                } else if key == "rejection_reason" {
                    if let Some(reason) = child.as_str() {
                        for item in reason.split(';').filter(|item| !item.is_empty()) {
                            blockers.insert(item.to_string());
                        }
                    }
                }
                collect_blockers(child, blockers);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_blockers(item, blockers);
            }
        }
        _ => {}
    }
}

fn collect_rejected_families(value: &serde_json::Value, families: &mut BTreeSet<String>) {
    for row in value
        .get("factor_registry_upserts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let status = row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if matches!(status, "blocked" | "rejected" | "reject") {
            if let Some(family) = row.get("factor_family").and_then(serde_json::Value::as_str) {
                families.insert(family.to_string());
            }
        }
    }
}

fn collect_runtime_replay_requests(
    value: &serde_json::Value,
    requests: &mut Vec<serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if matches!(
                    key.as_str(),
                    "runtime_replay_request" | "runtime_candidate_replay_request"
                ) {
                    requests.push(child.clone());
                }
                collect_runtime_replay_requests(child, requests);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_runtime_replay_requests(item, requests);
            }
        }
        _ => {}
    }
}

fn count_blocked_runtime_contracts(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Object(map) => {
            let current = map
                .get("runtime_contract")
                .and_then(serde_json::Value::as_object)
                .and_then(|contract| contract.get("status"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|status| status == "blocked") as u64;
            current
                + map
                    .values()
                    .map(count_blocked_runtime_contracts)
                    .sum::<u64>()
        }
        serde_json::Value::Array(items) => items.iter().map(count_blocked_runtime_contracts).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        latest_runs: serde_json::Value,
        market_data_health: serde_json::Value,
    ) -> ResearchManagerInput {
        ResearchManagerInput {
            latest_runs,
            factor_registry_summary: serde_json::json!({}),
            rejected_factor_patterns: serde_json::json!({}),
            market_data_health,
            research_trace_summary: serde_json::json!({}),
            research_budget: ResearchBudget {
                max_candidates_per_day: 20,
                max_backtests_per_day: 5,
                max_llm_calls_per_day: 2,
            },
        }
    }

    #[test]
    fn chooses_fix_data_for_critical_missing_surface() {
        let plan = build_research_manager_plan(&input(
            serde_json::json!({"selected_node_count": 5}),
            serde_json::json!({"has_critical_missing_surface": true}),
        ));
        assert_eq!(plan.theme, "fix_data");
        assert_eq!(plan.evidence_stage, "diagnostic");
    }

    #[test]
    fn chooses_fix_data_for_numeric_stale_source_count() {
        let plan = build_research_manager_plan(&input(
            serde_json::json!({"selected_node_count": 5}),
            serde_json::json!({"stale_source_count": 1}),
        ));
        assert_eq!(plan.theme, "fix_data");
        assert_eq!(plan.candidate_count, 0);
    }

    #[test]
    fn chooses_fix_runtime_for_ready_handoff_without_replay_parity() {
        let plan = build_research_manager_plan(&input(
            serde_json::json!({"ready_handoff": true, "replay_parity_ready": false}),
            serde_json::json!({}),
        ));
        assert_eq!(plan.theme, "fix_runtime");
        assert_eq!(plan.evidence_stage, "runtime_parity");
    }

    #[test]
    fn chooses_fix_runtime_from_trace_runtime_contract_blocker() {
        let mut input = input(serde_json::json!({}), serde_json::json!({}));
        input.research_trace_summary = summarize_research_trace(&serde_json::json!({
            "promotion_decision": "blocked",
            "factor_evaluations": [{
                "passed_gate": false,
                "metrics_json": {
                    "runtime_contract": {"status": "blocked"},
                    "blockers": ["missing_runtime_strategy_mapping"]
                }
            }]
        }));

        let plan = build_research_manager_plan(&input);

        assert_eq!(plan.theme, "fix_runtime");
        assert_eq!(plan.evidence_stage, "runtime_parity");
    }

    #[test]
    fn chooses_revise_prior_from_trace_rejected_family() {
        let mut input = input(serde_json::json!({}), serde_json::json!({}));
        input.research_trace_summary = summarize_research_trace(&serde_json::json!({
            "promotion_decision": "blocked",
            "factor_registry_upserts": [{
                "status": "rejected",
                "factor_family": "external_microstructure"
            }],
            "factor_evaluations": [{
                "passed_gate": false,
                "metrics_json": {"blockers": ["nonpositive_rank_ic"]}
            }]
        }));

        let plan = build_research_manager_plan(&input);

        assert_eq!(plan.theme, "revise_prior");
        assert_eq!(plan.priority, "high");
    }

    #[test]
    fn chooses_continue_search_when_mcts_has_selected_nodes() {
        let plan = build_research_manager_plan(&input(
            serde_json::json!({"selected_node_count": 3}),
            serde_json::json!({}),
        ));
        assert_eq!(plan.theme, "continue_search");
        assert_eq!(plan.candidate_count, 20);
    }

    #[test]
    fn validates_allowed_evidence_stage_values() {
        assert!(validate_evidence_stage("walk_forward"));
        assert!(!validate_evidence_stage("live_candidate"));
    }
}
