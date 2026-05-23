use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchBudget {
    pub max_candidates_per_day: usize,
    pub max_backtests_per_day: usize,
    pub max_llm_calls_per_day: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchManagerInput {
    #[serde(default = "default_evidence_stage")]
    pub evidence_stage: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchManagerPlan {
    pub theme: String,
    pub candidate_count: usize,
    pub search_depth: usize,
    pub priority: String,
    pub evidence_stage: String,
    pub actions: Vec<String>,
}

fn default_evidence_stage() -> String {
    "factor_attribution".to_string()
}

pub fn validate_evidence_stage(stage: &str) -> Result<(), String> {
    match stage {
        "diagnostic" | "factor_attribution" | "executable_replay" | "walk_forward"
        | "runtime_parity" | "dry_run_candidate" | "live_candidate" => Ok(()),
        _ => Err(format!("unsupported evidence_stage: {stage}")),
    }
}

pub fn build_research_manager_plan(input: &ResearchManagerInput) -> ResearchManagerPlan {
    plan_next_research(input).unwrap_or_else(|_| {
        plan(
            "fix_workflow",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec!["fix_unsupported_evidence_stage"],
        )
    })
}

pub fn plan_next_research(input: &ResearchManagerInput) -> Result<ResearchManagerPlan, String> {
    validate_evidence_stage(&input.evidence_stage)?;

    if has_any_key_value(
        &input.market_data_health,
        &[
            "missing",
            "stale",
            "critical_missing",
            "missing_blocks_promotion",
        ],
        &[true.into(), "true".into(), "critical".into()],
    ) || contains_string(
        &input.market_data_health,
        &["missing_blocks_promotion", "stale", "critical_missing"],
    ) {
        return Ok(plan(
            "fix_data",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "repair_or_exclude_missing_data_surface",
                "rerun_snapshot_data_audit",
            ],
        ));
    }

    if contains_string(
        &input.latest_runs,
        &["replay_parity_missing", "runtime_parity_missing"],
    ) || has_any_key_value(
        &input.latest_runs,
        &["replay_parity_ready", "runtime_parity_ready"],
        &[false.into(), "false".into()],
    ) {
        return Ok(plan(
            "fix_runtime",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "run_recorded_replay_parity",
                "compare_runtime_scorer_contract",
            ],
        ));
    }

    if contains_string(
        &input.latest_runs,
        &[
            "reward_stagnation",
            "empty_search",
            "stagnant",
            "revise_prior",
        ],
    ) || contains_string(
        &input.rejected_factor_patterns,
        &["stagnant", "repeated_rejection"],
    ) {
        return Ok(plan(
            "revise_prior",
            input.research_budget.max_candidates_per_day.min(8),
            2,
            "medium",
            &input.evidence_stage,
            vec![
                "generate_typed_llm_prior_json",
                "rerun_alpha_search_with_bounded_mutations",
            ],
        ));
    }

    let has_selected_nodes = contains_string(&input.latest_runs, &["selected_nodes"])
        || contains_string(&input.factor_registry_summary, &["candidate", "watchlist"]);
    let candidate_count = if has_selected_nodes {
        input.research_budget.max_candidates_per_day.min(12)
    } else {
        input.research_budget.max_candidates_per_day.min(4)
    };

    Ok(plan(
        "continue_search",
        candidate_count,
        2,
        "normal",
        &input.evidence_stage,
        vec![
            "continue_hosted_alpha_search",
            "write_registry_preview_and_trace_artifacts",
        ],
    ))
}

fn plan(
    theme: &str,
    candidate_count: usize,
    search_depth: usize,
    priority: &str,
    evidence_stage: &str,
    actions: Vec<&str>,
) -> ResearchManagerPlan {
    ResearchManagerPlan {
        theme: theme.to_string(),
        candidate_count,
        search_depth,
        priority: priority.to_string(),
        evidence_stage: evidence_stage.to_string(),
        actions: actions.into_iter().map(str::to_string).collect(),
    }
}

fn contains_string(value: &serde_json::Value, needles: &[&str]) -> bool {
    let raw = value.to_string();
    needles.iter().any(|needle| raw.contains(needle))
}

fn has_any_key_value(
    value: &serde_json::Value,
    keys: &[&str],
    expected_values: &[serde_json::Value],
) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, item)| {
            (keys.contains(&key.as_str()) && expected_values.contains(item))
                || has_any_key_value(item, keys, expected_values)
        }),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| has_any_key_value(item, keys, expected_values)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        evidence_stage: &str,
        latest_runs: serde_json::Value,
        market_data_health: serde_json::Value,
    ) -> ResearchManagerInput {
        ResearchManagerInput {
            evidence_stage: evidence_stage.to_string(),
            latest_runs,
            factor_registry_summary: serde_json::json!({"selected_nodes": [{"id": "n1"}]}),
            rejected_factor_patterns: serde_json::json!({}),
            market_data_health,
            research_budget: ResearchBudget {
                max_candidates_per_day: 20,
                max_backtests_per_day: 4,
                max_llm_calls_per_day: 1,
            },
        }
    }

    #[test]
    fn planner_fails_closed_on_unknown_evidence_stage() {
        let err = plan_next_research(&input(
            "unknown_stage",
            serde_json::json!({}),
            serde_json::json!({}),
        ))
        .expect_err("stage");
        assert!(err.contains("unsupported evidence_stage"));
    }

    #[test]
    fn planner_prioritizes_data_repairs() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({}),
            serde_json::json!({"surfaces": [{"name": "clob", "missing_blocks_promotion": true}]}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_data");
        assert_eq!(plan.candidate_count, 0);
    }

    #[test]
    fn planner_prioritizes_runtime_parity_repairs() {
        let plan = plan_next_research(&input(
            "walk_forward",
            serde_json::json!({"handoff": {"replay_parity_ready": false}}),
            serde_json::json!({}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_runtime");
    }

    #[test]
    fn planner_revises_prior_after_stagnation() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({"stop_reason": "reward_stagnation"}),
            serde_json::json!({}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "revise_prior");
        assert!(
            plan.actions
                .contains(&"generate_typed_llm_prior_json".to_string())
        );
    }
}
