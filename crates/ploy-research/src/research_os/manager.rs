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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_actions: Vec<ResearchBlockerAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchBlockerAction {
    pub blocker_family: String,
    pub action: String,
    pub reason: String,
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
        &["missing", "stale", "critical_missing"],
        &[true.into(), "true".into(), "critical".into()],
    ) {
        return Ok(plan_with_blocker_actions(
            "fix_data",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "repair_or_exclude_missing_data_surface",
                "rerun_snapshot_data_audit",
            ],
            derive_blocker_actions(input),
        ));
    }

    let blocker_actions = derive_blocker_actions(input);
    if blocker_actions
        .iter()
        .any(|item| item.blocker_family.starts_with("data_"))
    {
        let mut actions = vec!["rerun_snapshot_data_audit"];
        if blocker_actions
            .iter()
            .any(|item| item.action == "repair_official_settlement_coverage")
        {
            actions.insert(0, "repair_official_settlement_coverage");
        }
        if blocker_actions
            .iter()
            .any(|item| item.action == "collect_full_depth_execution_surface")
        {
            actions.insert(0, "collect_full_depth_execution_surface");
        }
        return Ok(plan_with_blocker_actions(
            "fix_data",
            0,
            0,
            "high",
            &input.evidence_stage,
            actions,
            blocker_actions,
        ));
    }

    if contains_string(
        &input.latest_runs,
        &[
            "replay_parity_missing",
            "runtime_parity_missing",
            "candidate_strategy_replay_not_runtime_replay",
            "candidate_strategy_replay_missing",
            "missing_runtime_contract",
            "runtime_contract_unmapped_factor",
            "missing_runtime_strategy_mapping",
        ],
    ) || has_any_key_value(
        &input.latest_runs,
        &["replay_parity_ready", "runtime_parity_ready"],
        &[false.into(), "false".into()],
    ) {
        return Ok(plan_with_blocker_actions(
            "fix_runtime",
            0,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "run_recorded_replay_parity",
                "compare_runtime_scorer_contract",
            ],
            blocker_actions,
        ));
    }

    if blocker_actions.iter().any(|item| {
        item.blocker_family == "search_power" || item.blocker_family == "execution_fillability"
    }) {
        return Ok(plan_with_blocker_actions(
            "revise_prior",
            input.research_budget.max_candidates_per_day.min(8),
            2,
            "high",
            &input.evidence_stage,
            vec![
                "generate_typed_llm_prior_json",
                "rerun_alpha_search_with_bounded_mutations",
            ],
            blocker_actions,
        ));
    }

    if has_unblocked_runtime_candidate(&input.factor_registry_summary) {
        return Ok(plan_with_blocker_actions(
            "candidate_to_runtime_replay",
            1,
            0,
            "high",
            &input.evidence_stage,
            vec![
                "build_runtime_candidate_replay",
                "write_runtime_replay_trace_artifact",
            ],
            blocker_actions,
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
        return Ok(plan_with_blocker_actions(
            "revise_prior",
            input.research_budget.max_candidates_per_day.min(8),
            2,
            "medium",
            &input.evidence_stage,
            vec![
                "generate_typed_llm_prior_json",
                "rerun_alpha_search_with_bounded_mutations",
            ],
            blocker_actions,
        ));
    }

    let has_selected_nodes = contains_string(&input.latest_runs, &["selected_nodes"])
        || contains_string(&input.factor_registry_summary, &["candidate", "watchlist"]);
    let candidate_count = if has_selected_nodes {
        input.research_budget.max_candidates_per_day.min(12)
    } else {
        input.research_budget.max_candidates_per_day.min(4)
    };

    Ok(plan_with_blocker_actions(
        "continue_search",
        candidate_count,
        2,
        "normal",
        &input.evidence_stage,
        vec![
            "continue_hosted_alpha_search",
            "write_registry_preview_and_trace_artifacts",
        ],
        blocker_actions,
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
        blocker_actions: Vec::new(),
    }
}

fn plan_with_blocker_actions(
    theme: &str,
    candidate_count: usize,
    search_depth: usize,
    priority: &str,
    evidence_stage: &str,
    actions: Vec<&str>,
    blocker_actions: Vec<ResearchBlockerAction>,
) -> ResearchManagerPlan {
    ResearchManagerPlan {
        blocker_actions,
        ..plan(
            theme,
            candidate_count,
            search_depth,
            priority,
            evidence_stage,
            actions,
        )
    }
}

fn derive_blocker_actions(input: &ResearchManagerInput) -> Vec<ResearchBlockerAction> {
    let mut actions = Vec::new();
    let latest = input.latest_runs.to_string().to_lowercase();
    let market_data = input.market_data_health.to_string().to_lowercase();
    let rejected = input.rejected_factor_patterns.to_string().to_lowercase();
    let runtime_or_promotion_blockers = format!("{latest} {rejected}");

    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "official_settlement_missing",
            "missing_official_settlement",
            "settlement_labels_missing",
            "official settlement",
        ],
    ) {
        actions.push(blocker_action(
            "data_settlement",
            "repair_official_settlement_coverage",
            "Runtime replay traded events are missing official settlement labels.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "sampled_snapshot_required_for_execution_surface",
            "required_execution_surface_is_sampled_snapshot",
            "full_depth_missing",
            "full-depth missing",
        ],
    ) {
        actions.push(blocker_action(
            "data_execution_surface",
            "collect_full_depth_execution_surface",
            "Promotion requires full-depth executable evidence instead of sampled snapshots.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &["trade_count_too_small", "min_trade_count", "too_few_trades"],
    ) {
        actions.push(blocker_action(
            "search_power",
            "increase_distinct_event_coverage_or_reduce_selectivity",
            "Runtime replay did not produce enough distinct event-level trades.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "fillability",
            "fill_rate_too_low",
            "entry_fill_rate_too_low",
            "capacity_too_low",
        ],
    ) {
        actions.push(blocker_action(
            "execution_fillability",
            "prefer_high_fillability_depth_filters",
            "Candidate selection must avoid thin depth or low-fillability entry surfaces.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "missing_runtime_contract",
            "runtime_contract_unmapped_factor",
            "missing_runtime_strategy_mapping",
        ],
    ) {
        actions.push(blocker_action(
            "runtime_contract",
            "repair_runtime_contract_mapping",
            "Factor needs a typed unblocked runtime contract before replay or promotion.",
        ));
    }
    if contains_any_text(
        &runtime_or_promotion_blockers,
        &[
            "candidate_strategy_replay_not_runtime_replay",
            "requires_runtime_replay_not_top_bucket_aggregate",
            "candidate_strategy_replay_identity_basis_mismatch",
        ],
    ) {
        actions.push(blocker_action(
            "runtime_replay",
            "build_runtime_market_update_replay",
            "Top-bucket aggregate evidence must be replaced by ordered runtime MarketUpdate replay.",
        ));
    }

    if contains_any_text(
        &market_data,
        &[
            "required_execution_surface_is_sampled_snapshot",
            "sampled_snapshot_required_for_execution_surface",
            "full_depth_missing",
            "full-depth missing",
        ],
    ) {
        actions.push(blocker_action(
            "promotion_data_execution_surface",
            "collect_full_depth_execution_surface",
            "Research snapshot data health reports sampled execution surface; promotion needs full-depth evidence.",
        ));
    }
    if contains_any_text(
        &market_data,
        &[
            "required_execution_surface_not_materialized",
            "pm_token_settlements",
            "official_settlement_missing",
        ],
    ) {
        actions.push(blocker_action(
            "promotion_data_settlement",
            "repair_official_settlement_coverage",
            "Research snapshot data health reports missing or non-materialized official settlement labels.",
        ));
    }

    dedupe_blocker_actions(actions)
}

fn blocker_action(blocker_family: &str, action: &str, reason: &str) -> ResearchBlockerAction {
    ResearchBlockerAction {
        blocker_family: blocker_family.to_string(),
        action: action.to_string(),
        reason: reason.to_string(),
    }
}

fn contains_any_text(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn dedupe_blocker_actions(actions: Vec<ResearchBlockerAction>) -> Vec<ResearchBlockerAction> {
    let mut deduped = Vec::new();
    for action in actions {
        if !deduped
            .iter()
            .any(|item: &ResearchBlockerAction| item.action == action.action)
        {
            deduped.push(action);
        }
    }
    deduped
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

fn has_unblocked_runtime_candidate(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            let status = map
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let candidate_status = status.is_empty() || matches!(status, "candidate" | "watchlist");
            let empty_row_blockers = json_array_empty(map.get("blockers"));
            let contract = map.get("runtime_contract");
            let unblocked_contract =
                contract
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|contract| {
                        let version_ok =
                            contract.get("version").and_then(serde_json::Value::as_str)
                                == Some("autofactor_runtime_contract_v1");
                        version_ok
                            && contract
                                .get("runtime_score")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|score| !score.is_empty())
                            && contract
                                .get("strategy_profile")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|profile| !profile.is_empty())
                            && json_array_empty(contract.get("blockers"))
                    });
            (candidate_status && empty_row_blockers && unblocked_contract)
                || map.values().any(has_unblocked_runtime_candidate)
        }
        serde_json::Value::Array(items) => items.iter().any(has_unblocked_runtime_candidate),
        _ => false,
    }
}

fn json_array_empty(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Array(items)) => items.is_empty(),
        Some(serde_json::Value::Null) | None => true,
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
            serde_json::json!({"critical_missing": true, "surface_blockers": [{"name": "clob", "blocker_type": "data_repair"}]}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_data");
        assert_eq!(plan.candidate_count, 0);
    }

    #[test]
    fn planner_does_not_rerun_snapshot_for_promotion_only_blockers() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {"surface": "clob_orderbook_snapshots", "reason": "required_execution_surface_is_sampled_snapshot"}
                ]
            }),
        ))
        .expect("plan");
        assert_ne!(plan.theme, "fix_data");
        assert!(plan.candidate_count > 0);
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_execution_surface"
                && item.action == "collect_full_depth_execution_surface"
        }));
    }

    #[test]
    fn planner_routes_runtime_contract_blockers_to_runtime_repair() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({"blockers": ["missing_runtime_contract:auto_settlement_edge"]}),
            serde_json::json!({"missing_blocks_promotion": true, "critical_missing": false}),
        ))
        .expect("plan");
        assert_eq!(plan.theme, "fix_runtime");
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
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
    }

    #[test]
    fn planner_routes_unblocked_runtime_candidate_to_candidate_replay() {
        let mut input = input(
            "walk_forward",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {"surface": "clob_orderbook_snapshots", "reason": "required_execution_surface_is_sampled_snapshot"}
                ]
            }),
        );
        input.factor_registry_summary = serde_json::json!({
            "recent_factors": [{
                "factor_name": "auto_settlement_conservative_settlement_edge",
                "status": "candidate",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "blockers": [],
                "runtime_contract": {
                    "version": "autofactor_runtime_contract_v1",
                    "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "strategy_profile": "settlement_probability",
                    "blockers": []
                }
            }]
        });

        let plan = plan_next_research(&input).expect("plan");
        assert_eq!(plan.theme, "candidate_to_runtime_replay");
        assert!(plan
            .actions
            .contains(&"build_runtime_candidate_replay".to_string()));
    }

    #[test]
    fn planner_turns_runtime_replay_trade_count_blocker_into_typed_prior_action() {
        let plan = plan_next_research(&input(
            "executable_replay",
            serde_json::json!({
                "candidate_strategy_replay": {
                    "blocking_risk_flags": ["trade_count_too_small:29<50"]
                }
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "revise_prior");
        assert!(plan
            .actions
            .contains(&"generate_typed_llm_prior_json".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "search_power"
                && item.action == "increase_distinct_event_coverage_or_reduce_selectivity"
        }));
    }

    #[test]
    fn planner_turns_runtime_replay_data_blockers_into_data_actions() {
        let plan = plan_next_research(&input(
            "executable_replay",
            serde_json::json!({
                "candidate_strategy_replay": {
                    "blocking_risk_flags": [
                        "official_settlement_missing:25<29",
                        "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"
                    ]
                }
            }),
            serde_json::json!({}),
        ))
        .expect("plan");

        assert_eq!(plan.theme, "fix_data");
        assert!(plan
            .actions
            .contains(&"repair_official_settlement_coverage".to_string()));
        assert!(plan
            .actions
            .contains(&"collect_full_depth_execution_surface".to_string()));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "data_execution_surface"
                && item.action == "collect_full_depth_execution_surface"
        }));
    }

    #[test]
    fn planner_surfaces_snapshot_settlement_promotion_blocker_without_forcing_data_repair() {
        let plan = plan_next_research(&input(
            "factor_attribution",
            serde_json::json!({}),
            serde_json::json!({
                "missing_blocks_promotion": true,
                "critical_missing": false,
                "promotion_blockers": [
                    {
                        "surface": "pm_token_settlements",
                        "reason": "required_execution_surface_not_materialized"
                    }
                ]
            }),
        ))
        .expect("plan");

        assert_ne!(plan.theme, "fix_data");
        assert!(plan.blocker_actions.iter().any(|item| {
            item.blocker_family == "promotion_data_settlement"
                && item.action == "repair_official_settlement_coverage"
        }));
    }
}
