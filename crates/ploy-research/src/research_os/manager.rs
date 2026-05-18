use serde::{Deserialize, Serialize};

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

fn has_unhealthy_market_data(value: &serde_json::Value) -> bool {
    bool_path(value, &["has_critical_missing_surface"])
        || bool_path(value, &["critical_surface_stale"])
        || number_path(value, &["stale_source_count"]).unwrap_or(0.0) > 0.0
        || array_contains(value, &["missing_blocks_promotion"])
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
