use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::autofactor::{
    factor_expr_hash, AutoFactorDecision, AutoFactorOptions, AutoFactorReport, FactorExpr,
};

const ALPHA_SEARCH_ARTIFACT_VERSION: &str = "alpha_search_artifacts_v1";

#[derive(Debug, Clone, Serialize)]
pub struct AlphaSearchArtifactSummary {
    pub target: String,
    pub output_dir: String,
    pub candidate_count: usize,
    pub rejected_count: usize,
    pub best_candidate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MctsSearchStateArtifact {
    pub version: String,
    pub mode: String,
    pub target: String,
    pub total_visits: usize,
    pub nodes: Vec<MctsSearchStateNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MctsSearchStateNode {
    pub factor_name: String,
    pub visits: usize,
    pub total_reward: f64,
    pub best_reward: f64,
    pub last_reward: f64,
    pub selected_dimension: String,
    pub last_decision: String,
}

#[derive(Debug)]
pub enum AlphaSearchArtifactError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for AlphaSearchArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "alpha search artifact I/O failed: {err}"),
            Self::Json(err) => write!(f, "alpha search artifact JSON failed: {err}"),
        }
    }
}

impl std::error::Error for AlphaSearchArtifactError {}

impl From<std::io::Error> for AlphaSearchArtifactError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AlphaSearchArtifactError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Serialize)]
struct SearchSpaceArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    feature_pool: Vec<String>,
    constant_pool: Vec<f64>,
    operator_pool: Vec<&'static str>,
    limits: SearchLimits,
}

#[derive(Debug, Serialize)]
struct SearchLimits {
    min_observations: usize,
    min_window_observations: usize,
    bucket_count: usize,
    min_spearman_ic: f64,
    min_icir: f64,
    min_positive_window_ratio: f64,
    min_top_bucket_avg_label: f64,
    min_monotonicity_score: f64,
    max_complexity: usize,
}

#[derive(Debug, Serialize)]
struct LlmPriorArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    hypotheses: Vec<PriorHypothesis>,
    allowed_mutation_types: Vec<&'static str>,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct PriorHypothesis {
    id: &'static str,
    hypothesis: &'static str,
    expected_mechanism: &'static str,
    required_surfaces: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CandidateExpression {
    name: String,
    target: Option<String>,
    source: &'static str,
    complexity: usize,
    root_gene: String,
    expr: FactorExpr,
}

#[derive(Debug, Serialize)]
struct RejectedExpression {
    name: String,
    target: Option<String>,
    root_gene: String,
    reason: String,
    complexity: usize,
}

#[derive(Debug, Serialize)]
struct TreeTraceArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    nodes: Vec<TreeTraceNode>,
}

#[derive(Debug, Serialize)]
struct TreeTraceNode {
    id: String,
    parent: Option<String>,
    factor_name: String,
    mutation: &'static str,
    selected_dimension: String,
    reward: f64,
    visits: usize,
    decision: String,
}

#[derive(Debug, Serialize)]
struct NodeMetric {
    id: String,
    factor_name: String,
    target: Option<String>,
    decision: String,
    reason: String,
    selected_dimension: String,
    effectiveness: f64,
    stability: f64,
    diversity: f64,
    execution_cost: f64,
    overfit_risk: f64,
    runtime_readiness: f64,
    reward: f64,
    spearman_ic: f64,
    icir: f64,
    positive_window_ratio: f64,
    top_bucket_avg_label: f64,
    top_bucket_full_depth_entry_fill_rate: f64,
    monotonicity_score: f64,
    complexity: usize,
}

#[derive(Debug, Serialize)]
struct AvoidedSubtree {
    root_gene: String,
    count: usize,
    action: &'static str,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct SearchFeedbackArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    candidate_count: usize,
    rejected_count: usize,
    watchlist_count: usize,
    passed_count: usize,
    best_candidate: Option<String>,
    best_reward: Option<f64>,
    interpretation: &'static str,
}

#[derive(Debug, Serialize)]
struct FactorRegistryPreviewArtifact {
    version: &'static str,
    target: String,
    factors: Vec<FactorRegistryPreviewRow>,
}

#[derive(Debug, Serialize)]
struct FactorRegistryPreviewRow {
    factor_name: String,
    target: Option<String>,
    dsl_hash: String,
    ast_json: FactorExpr,
    status: &'static str,
    metrics: serde_json::Value,
    blockers: Vec<String>,
    runtime_contract: RuntimeContractPreview,
}

#[derive(Debug, Serialize)]
struct RuntimeContractPreview {
    dsl_hash: String,
    ast_json: FactorExpr,
    runtime_score: Option<String>,
    strategy_profile: Option<String>,
    strategy_family: Option<String>,
    ast_input_names: Vec<String>,
    blockers: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MctsExpansionPlan {
    version: &'static str,
    mode: &'static str,
    target: String,
    exploration_weight: f64,
    selected_nodes: Vec<MctsSelectedNode>,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct MctsSelectedNode {
    node_id: String,
    factor_name: String,
    selected_dimension: String,
    proposed_mutation: &'static str,
    reward: f64,
    ucb_priority: f64,
}

pub fn write_alpha_search_artifacts(
    output_root: impl AsRef<Path>,
    target: &str,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    write_alpha_search_artifacts_with_state(
        output_root,
        target,
        input_names,
        reports,
        options,
        None,
    )
}

pub fn read_mcts_search_state(
    path: impl AsRef<Path>,
) -> Result<MctsSearchStateArtifact, AlphaSearchArtifactError> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn write_alpha_search_artifacts_with_state(
    output_root: impl AsRef<Path>,
    target: &str,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
    prior_state: Option<&MctsSearchStateArtifact>,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    let output_dir = output_root.as_ref().join(target);
    std::fs::create_dir_all(&output_dir)?;

    let feature_pool = {
        let mut values = input_names.to_vec();
        values.sort();
        values
    };
    write_json(
        &output_dir.join("search-space.json"),
        &SearchSpaceArtifact {
            version: ALPHA_SEARCH_ARTIFACT_VERSION,
            mode: "deterministic_seed_search",
            target: target.to_string(),
            feature_pool,
            constant_pool: vec![
                0.001, 0.005, 0.01, 0.02, 0.05, 0.10, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0, 60.0, 300.0,
            ],
            operator_pool: vec![
                "Input",
                "Const",
                "Add",
                "Sub",
                "Mul",
                "SafeDiv",
                "Max",
                "Min",
                "Tanh",
                "Log1pAbs",
                "SqrtAbs",
                "Clip",
                "Delta",
                "RollingMean",
                "RollingStd",
                "ZScore",
            ],
            limits: SearchLimits {
                min_observations: options.min_observations,
                min_window_observations: options.min_window_observations,
                bucket_count: options.bucket_count,
                min_spearman_ic: options.min_spearman_ic,
                min_icir: options.min_icir,
                min_positive_window_ratio: options.min_positive_window_ratio,
                min_top_bucket_avg_label: options.min_top_bucket_avg_label,
                min_monotonicity_score: options.min_monotonicity_score,
                max_complexity: options.max_complexity,
            },
        },
    )?;

    write_json(
        &output_dir.join("llm-priors.json"),
        &LlmPriorArtifact {
            version: ALPHA_SEARCH_ARTIFACT_VERSION,
            mode: "deterministic_domain_prior_placeholder",
            target: target.to_string(),
            hypotheses: default_hypotheses(target),
            allowed_mutation_types: vec![
                "add_feature_gate",
                "replace_denominator",
                "add_spread_penalty",
                "add_capacity_gate",
                "add_near_strike_interaction",
                "change_time_window",
                "clip_or_squash",
                "invert_or_contrarian",
                "remove_component",
            ],
            note: "External LLM expansion is not invoked in this artifact. This file records the machine-checkable prior schema used by deterministic seed search.",
        },
    )?;

    let candidates = reports
        .iter()
        .map(|report| CandidateExpression {
            name: report.name.clone(),
            target: report.target.clone(),
            source: candidate_source(&report.name),
            complexity: report.complexity,
            root_gene: root_gene(&report.expr),
            expr: report.expr.clone(),
        })
        .collect::<Vec<_>>();
    write_json(&output_dir.join("candidate-expressions.json"), &candidates)?;

    let rejected = reports
        .iter()
        .filter(|report| report.decision == AutoFactorDecision::Reject)
        .map(|report| RejectedExpression {
            name: report.name.clone(),
            target: report.target.clone(),
            root_gene: root_gene(&report.expr),
            reason: report.reason.clone(),
            complexity: report.complexity,
        })
        .collect::<Vec<_>>();
    write_json(&output_dir.join("rejected-expressions.json"), &rejected)?;

    let node_metrics = reports
        .iter()
        .enumerate()
        .map(|(idx, report)| node_metric(idx, report))
        .collect::<Vec<_>>();
    write_json(&output_dir.join("node-metrics.json"), &node_metrics)?;
    write_json(
        &output_dir.join("factor-registry-preview.json"),
        &factor_registry_preview(target, reports)?,
    )?;
    let mcts_state = mcts_search_state(target, &node_metrics, prior_state);
    write_json(&output_dir.join("mcts-state.json"), &mcts_state)?;
    write_json(
        &output_dir.join("mcts-expansion-plan.json"),
        &mcts_expansion_plan(target, &node_metrics, &mcts_state),
    )?;

    write_json(
        &output_dir.join("tree-trace.json"),
        &TreeTraceArtifact {
            version: ALPHA_SEARCH_ARTIFACT_VERSION,
            mode: "single_depth_seed_tree",
            target: target.to_string(),
            nodes: reports
                .iter()
                .enumerate()
                .map(|(idx, report)| TreeTraceNode {
                    id: format!("node-{idx}"),
                    parent: None,
                    factor_name: report.name.clone(),
                    mutation: "seed",
                    selected_dimension: selected_dimension(report),
                    reward: reward(report),
                    visits: 1,
                    decision: report.decision.as_str().to_string(),
                })
                .collect(),
        },
    )?;

    write_json(
        &output_dir.join("avoided-subtrees.json"),
        &avoided_subtrees(reports),
    )?;

    let best = node_metrics
        .iter()
        .max_by(|lhs, rhs| lhs.reward.total_cmp(&rhs.reward));
    let feedback = SearchFeedbackArtifact {
        version: ALPHA_SEARCH_ARTIFACT_VERSION,
        mode: "deterministic_seed_search",
        target: target.to_string(),
        candidate_count: reports.len(),
        rejected_count: rejected.len(),
        watchlist_count: reports
            .iter()
            .filter(|report| report.decision == AutoFactorDecision::Watchlist)
            .count(),
        passed_count: reports
            .iter()
            .filter(|report| report.decision == AutoFactorDecision::Candidate)
            .count(),
        best_candidate: best.map(|metric| metric.factor_name.clone()),
        best_reward: best.map(|metric| metric.reward),
        interpretation: "Search feedback is discovery evidence only. Promotion still requires the AutoFactor strategy-promotion gate and replay/runtime parity.",
    };
    write_json(&output_dir.join("search-feedback.json"), &feedback)?;

    Ok(AlphaSearchArtifactSummary {
        target: target.to_string(),
        output_dir: output_dir.display().to_string(),
        candidate_count: reports.len(),
        rejected_count: rejected.len(),
        best_candidate: feedback.best_candidate,
    })
}

fn factor_registry_preview(
    target: &str,
    reports: &[AutoFactorReport],
) -> Result<FactorRegistryPreviewArtifact, AlphaSearchArtifactError> {
    let mut factors = Vec::with_capacity(reports.len());
    for report in reports {
        let dsl_hash = factor_expr_hash(&report.expr)?;
        let runtime_contract = runtime_contract_preview(report, &dsl_hash);
        let mut blockers = Vec::new();
        if report.decision == AutoFactorDecision::Reject {
            blockers.push(format!("autofactor_rejected:{}", report.reason));
        } else if report.decision == AutoFactorDecision::Watchlist {
            blockers.push(format!("autofactor_watchlist:{}", report.reason));
        }
        blockers.extend(runtime_contract.blockers.iter().cloned());
        factors.push(FactorRegistryPreviewRow {
            factor_name: report.name.clone(),
            target: report.target.clone(),
            dsl_hash,
            ast_json: report.expr.clone(),
            status: registry_status(report.decision),
            metrics: serde_json::json!({
                "n": report.n,
                "spearman_ic": report.spearman_ic,
                "icir": report.icir,
                "positive_window_ratio": report.positive_window_ratio,
                "symbol_count": report.symbol_count,
                "symbol_positive_ratio": report.symbol_positive_ratio,
                "top_bucket_avg_label": report.top_bucket_avg_label,
                "top_bucket_full_depth_entry_fill_rate": report.top_bucket_full_depth_entry_fill_rate,
                "top_bucket_unique_event_count": report.top_bucket_unique_event_count,
                "top_bucket_max_event_decisions": report.top_bucket_max_event_decisions,
                "monotonicity_score": report.monotonicity_score,
                "complexity": report.complexity,
                "reason": report.reason,
            }),
            blockers,
            runtime_contract,
        });
    }
    Ok(FactorRegistryPreviewArtifact {
        version: ALPHA_SEARCH_ARTIFACT_VERSION,
        target: target.to_string(),
        factors,
    })
}

fn registry_status(decision: AutoFactorDecision) -> &'static str {
    match decision {
        AutoFactorDecision::Candidate => "candidate",
        AutoFactorDecision::Watchlist => "watchlist",
        AutoFactorDecision::Reject => "rejected",
    }
}

fn runtime_contract_preview(report: &AutoFactorReport, dsl_hash: &str) -> RuntimeContractPreview {
    let ast_input_names = report.expr.input_names().into_iter().collect::<Vec<_>>();
    let mut blockers = Vec::new();
    let mapping = runtime_mapping_for_factor(&report.name);
    if mapping.runtime_score.is_none() {
        blockers.push("missing_runtime_score".to_string());
    }
    if mapping.strategy_profile.is_none() {
        blockers.push("missing_runtime_strategy_profile".to_string());
    }
    RuntimeContractPreview {
        dsl_hash: dsl_hash.to_string(),
        ast_json: report.expr.clone(),
        runtime_score: mapping.runtime_score,
        strategy_profile: mapping.strategy_profile,
        strategy_family: mapping.strategy_family,
        ast_input_names,
        blockers,
    }
}

struct RuntimeMapping {
    runtime_score: Option<String>,
    strategy_profile: Option<String>,
    strategy_family: Option<String>,
}

fn runtime_mapping_for_factor(name: &str) -> RuntimeMapping {
    if name.starts_with("auto_settlement_") {
        return RuntimeMapping {
            runtime_score: Some(format!("autofactor_formula:{name}")),
            strategy_profile: Some("settlement_probability".to_string()),
            strategy_family: Some("settlement_probability".to_string()),
        };
    }
    match name {
        "amplitude_weighted_momentum_30s_sigma"
        | "mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate"
        | "mut_spread_adjusted_external_move_full_depth_entry_gate" => RuntimeMapping {
            runtime_score: Some(format!("autofactor_formula:{name}")),
            strategy_profile: Some("settlement_probability".to_string()),
            strategy_family: Some("predictive_settlement_probability".to_string()),
        },
        "spread_adjusted_external_move" => RuntimeMapping {
            runtime_score: Some("spread_adjusted_external_move_score".to_string()),
            strategy_profile: Some("repricing_momentum".to_string()),
            strategy_family: Some("repricing".to_string()),
        },
        "repricing_gap_side_10s" => RuntimeMapping {
            runtime_score: Some("repricing_gap_side_10s".to_string()),
            strategy_profile: Some("repricing_momentum".to_string()),
            strategy_family: Some("repricing".to_string()),
        },
        "settlement_fair_edge" => RuntimeMapping {
            runtime_score: None,
            strategy_profile: None,
            strategy_family: Some("settlement_probability".to_string()),
        },
        _ => RuntimeMapping {
            runtime_score: None,
            strategy_profile: None,
            strategy_family: None,
        },
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), AlphaSearchArtifactError> {
    let raw = serde_json::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}

fn default_hypotheses(target: &str) -> Vec<PriorHypothesis> {
    if target == "full_depth_settlement_executable_pnl" {
        vec![
            PriorHypothesis {
                id: "settlement_edge_after_execution_cost",
                hypothesis: "Settlement probability edge is valuable only after full-depth executable entry cost and PM fee are deducted.",
                expected_mechanism: "True q minus sweep price should rank event-side decisions when depth and quote freshness are adequate.",
                required_surfaces: vec!["polymarket_full_clob_depth", "official_settlement", "probability_state"],
            },
            PriorHypothesis {
                id: "capacity_and_near_strike_gate",
                hypothesis: "Settlement edge should be gated by near-strike state and executable capacity.",
                expected_mechanism: "Near-strike contracts are more sensitive to small external moves, but only deployable when the book can absorb the stake.",
                required_surfaces: vec!["event_geometry", "polymarket_full_clob_depth"],
            },
        ]
    } else {
        vec![PriorHypothesis {
            id: "repricing_after_pm_lag",
            hypothesis: "External market movement is more valuable when Polymarket quotes are stale or spread-adjusted friction is low.",
            expected_mechanism: "CEX movement can predict short-horizon PM quote repricing before the book updates.",
            required_surfaces: vec!["binance_price", "binance_l2", "polymarket_quote_ticks"],
        }]
    }
}

fn candidate_source(name: &str) -> &'static str {
    if name.starts_with("mut_") {
        "deterministic_mutation"
    } else if name.starts_with("auto_settlement_") {
        "settlement_native_generator"
    } else {
        "domain_seed"
    }
}

fn node_metric(idx: usize, report: &AutoFactorReport) -> NodeMetric {
    NodeMetric {
        id: format!("node-{idx}"),
        factor_name: report.name.clone(),
        target: report.target.clone(),
        decision: report.decision.as_str().to_string(),
        reason: report.reason.clone(),
        selected_dimension: selected_dimension(report),
        effectiveness: normalized_positive(report.top_bucket_avg_label),
        stability: report.positive_window_ratio.clamp(0.0, 1.0),
        diversity: 1.0 / report.complexity.max(1) as f64,
        execution_cost: execution_score(report),
        overfit_risk: 1.0 / report.complexity.max(1) as f64,
        runtime_readiness: if report.name.starts_with("auto_settlement_")
            || report.name == "amplitude_weighted_momentum_30s_sigma"
        {
            1.0
        } else {
            0.5
        },
        reward: reward(report),
        spearman_ic: finite_or_zero(report.spearman_ic),
        icir: finite_or_zero(report.icir),
        positive_window_ratio: finite_or_zero(report.positive_window_ratio),
        top_bucket_avg_label: finite_or_zero(report.top_bucket_avg_label),
        top_bucket_full_depth_entry_fill_rate: finite_or_zero(
            report.top_bucket_full_depth_entry_fill_rate,
        ),
        monotonicity_score: finite_or_zero(report.monotonicity_score),
        complexity: report.complexity,
    }
}

fn mcts_search_state(
    target: &str,
    metrics: &[NodeMetric],
    prior_state: Option<&MctsSearchStateArtifact>,
) -> MctsSearchStateArtifact {
    let mut nodes = prior_state
        .filter(|state| state.target == target)
        .map(|state| {
            state
                .nodes
                .iter()
                .map(|node| (node.factor_name.clone(), node.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    for metric in metrics {
        let mut node = nodes
            .remove(&metric.factor_name)
            .unwrap_or_else(|| MctsSearchStateNode {
                factor_name: metric.factor_name.clone(),
                visits: 0,
                total_reward: 0.0,
                best_reward: f64::NEG_INFINITY,
                last_reward: 0.0,
                selected_dimension: metric.selected_dimension.clone(),
                last_decision: metric.decision.clone(),
            });
        node.visits = node.visits.saturating_add(1);
        node.total_reward += metric.reward;
        node.best_reward = node.best_reward.max(metric.reward);
        node.last_reward = metric.reward;
        node.selected_dimension = metric.selected_dimension.clone();
        node.last_decision = metric.decision.clone();
        nodes.insert(node.factor_name.clone(), node);
    }

    let mut nodes = nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by(|lhs, rhs| lhs.factor_name.cmp(&rhs.factor_name));
    let total_visits = nodes.iter().map(|node| node.visits).sum();
    MctsSearchStateArtifact {
        version: ALPHA_SEARCH_ARTIFACT_VERSION.to_string(),
        mode: "cumulative_ucb_state".to_string(),
        target: target.to_string(),
        total_visits,
        nodes,
    }
}

fn mcts_expansion_plan(
    target: &str,
    metrics: &[NodeMetric],
    state: &MctsSearchStateArtifact,
) -> MctsExpansionPlan {
    let exploration_weight = 0.75;
    let total_visits = state.total_visits.max(1) as f64;
    let state_by_factor = state
        .nodes
        .iter()
        .map(|node| (node.factor_name.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let mut selected = metrics
        .iter()
        .filter(|metric| metric.decision != "reject")
        .map(|metric| {
            let state_node = state_by_factor.get(metric.factor_name.as_str());
            let visits = state_node.map(|node| node.visits).unwrap_or(1).max(1) as f64;
            let average_reward = state_node
                .map(|node| node.total_reward / visits)
                .unwrap_or(metric.reward);
            let ucb_priority =
                average_reward + exploration_weight * (total_visits.ln() / visits).sqrt();
            MctsSelectedNode {
                node_id: metric.id.clone(),
                factor_name: metric.factor_name.clone(),
                selected_dimension: metric.selected_dimension.clone(),
                proposed_mutation: proposed_mutation(&metric.selected_dimension),
                reward: metric.reward,
                ucb_priority,
            }
        })
        .collect::<Vec<_>>();
    selected.sort_by(|lhs, rhs| rhs.ucb_priority.total_cmp(&lhs.ucb_priority));
    selected.truncate(12);

    MctsExpansionPlan {
        version: ALPHA_SEARCH_ARTIFACT_VERSION,
        mode: "single_run_ucb_planner",
        target: target.to_string(),
        exploration_weight,
        selected_nodes: selected,
        note: "This is the first MCTS control artifact. It selects branches for the next search run from node metrics, but does not yet execute multi-run backpropagation.",
    }
}

fn proposed_mutation(selected_dimension: &str) -> &'static str {
    match selected_dimension {
        "sample_power" => "do_not_expand_collect_more_data",
        "stability" => "add_feature_gate",
        "effectiveness" => "replace_denominator",
        "monotonicity" => "clip_or_squash",
        "execution_quality" => "add_capacity_gate",
        "overfit_risk" => "remove_component",
        "exploit" => "add_capacity_gate",
        _ => "clip_or_squash",
    }
}

fn reward(report: &AutoFactorReport) -> f64 {
    let decision_bonus = match report.decision {
        AutoFactorDecision::Candidate => 1.0,
        AutoFactorDecision::Watchlist => 0.25,
        AutoFactorDecision::Reject => -0.5,
    };
    decision_bonus
        + finite_or_zero(report.icir).tanh()
        + finite_or_zero(report.spearman_ic).tanh()
        + finite_or_zero(report.positive_window_ratio)
        + normalized_positive(report.top_bucket_avg_label)
        + execution_score(report)
        + finite_or_zero(report.monotonicity_score)
        - (report.complexity as f64 / 32.0)
}

fn execution_score(report: &AutoFactorReport) -> f64 {
    let top_bucket_fillability = finite_or_zero(report.top_bucket_full_depth_entry_fill_rate);
    let structure_bonus = if report.name.contains("capacity") || report.name.contains("spread") {
        0.25
    } else {
        0.0
    };
    (top_bucket_fillability + structure_bonus).clamp(0.0, 1.0)
}

fn selected_dimension(report: &AutoFactorReport) -> String {
    match report.reason.as_str() {
        "too_few_observations" | "no_powered_windows" => "sample_power",
        "low_icir" | "unstable_positive_windows" => "stability",
        "nonpositive_top_bucket_label" | "nonpositive_rank_ic" => "effectiveness",
        "low_top_bucket_fillability" => "execution_quality",
        "nonmonotonic_buckets" => "monotonicity",
        "too_complex" => "overfit_risk",
        "passed" => "exploit",
        _ => "unknown",
    }
    .to_string()
}

fn avoided_subtrees(reports: &[AutoFactorReport]) -> Vec<AvoidedSubtree> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for report in reports {
        *counts.entry(root_gene(&report.expr)).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 2)
        .map(|(root_gene, count)| AvoidedSubtree {
            root_gene,
            count,
            action: "penalize",
            reason: "root_gene_crowding",
        })
        .collect()
}

fn root_gene(expr: &FactorExpr) -> String {
    match expr {
        FactorExpr::Input(_) => "Input",
        FactorExpr::Const(_) => "Const",
        FactorExpr::Add(_, _) => "Add",
        FactorExpr::Sub(_, _) => "Sub",
        FactorExpr::Mul(_, _) => "Mul",
        FactorExpr::SafeDiv(_, _) => "SafeDiv",
        FactorExpr::Max(_, _) => "Max",
        FactorExpr::Min(_, _) => "Min",
        FactorExpr::Tanh(_) => "Tanh",
        FactorExpr::Log1pAbs(_) => "Log1pAbs",
        FactorExpr::SqrtAbs(_) => "SqrtAbs",
        FactorExpr::Clip { .. } => "Clip",
        FactorExpr::Delta { .. } => "Delta",
        FactorExpr::RollingMean { .. } => "RollingMean",
        FactorExpr::RollingStd { .. } => "RollingStd",
        FactorExpr::ZScore { .. } => "ZScore",
        FactorExpr::Gate { .. } => "Gate",
    }
    .to_string()
}

fn normalized_positive(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value.tanh()
    } else {
        0.0
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autofactor::{AutoFactorDecision, FactorExpr};

    #[test]
    fn writes_search_artifact_bundle() {
        let tmp =
            std::env::temp_dir().join(format!("ploy-alpha-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let report = AutoFactorReport {
            name: "auto_settlement_conservative_settlement_edge".to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Input("conservative_settlement_edge".to_string()),
            n: 100,
            pearson_ic: 0.2,
            spearman_ic: 0.25,
            window_count: 3,
            window_ic_mean: 0.2,
            icir: 1.2,
            positive_window_ratio: 1.0,
            symbol_count: 2,
            symbol_ic_mean: 0.18,
            symbol_icir: 1.0,
            symbol_positive_ratio: 1.0,
            bucket_avg_labels: vec![-0.1, 0.0, 0.2],
            bottom_bucket_n: 20,
            bottom_bucket_avg_label: -0.1,
            top_bucket_n: 20,
            top_bucket_avg_label: 0.2,
            top_bucket_positive_label_rate: 0.7,
            top_bucket_full_depth_entry_fill_rate: 0.8,
            top_bucket_unique_event_count: 20,
            top_bucket_max_event_decisions: 1,
            monotonicity_score: 1.0,
            complexity: 1,
            decision: AutoFactorDecision::Candidate,
            reason: "passed".to_string(),
        };
        let summary = write_alpha_search_artifacts(
            &tmp,
            "full_depth_settlement_executable_pnl",
            &["conservative_settlement_edge".to_string()],
            &[report],
            &AutoFactorOptions::default(),
        )
        .expect("write artifacts");
        assert_eq!(summary.candidate_count, 1);
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/search-space.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/tree-trace.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/mcts-expansion-plan.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/mcts-state.json")
            .exists());
        let registry_preview_raw = std::fs::read_to_string(
            tmp.join("full_depth_settlement_executable_pnl/factor-registry-preview.json"),
        )
        .expect("registry preview");
        let registry_preview: serde_json::Value =
            serde_json::from_str(&registry_preview_raw).expect("registry preview json");
        let factor = &registry_preview["factors"][0];
        assert_eq!(
            factor["runtime_contract"]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge"
        );
        assert_eq!(
            factor["runtime_contract"]["strategy_profile"],
            "settlement_probability"
        );
        assert_eq!(
            factor["runtime_contract"]["ast_input_names"][0],
            "conservative_settlement_edge"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn merges_prior_mcts_state_into_search_artifacts() {
        let tmp = std::env::temp_dir().join(format!(
            "ploy-alpha-search-state-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let report = AutoFactorReport {
            name: "auto_settlement_conservative_settlement_edge".to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Input("conservative_settlement_edge".to_string()),
            n: 100,
            pearson_ic: 0.2,
            spearman_ic: 0.25,
            window_count: 3,
            window_ic_mean: 0.2,
            icir: 1.2,
            positive_window_ratio: 1.0,
            symbol_count: 2,
            symbol_ic_mean: 0.18,
            symbol_icir: 1.0,
            symbol_positive_ratio: 1.0,
            bucket_avg_labels: vec![-0.1, 0.0, 0.2],
            bottom_bucket_n: 20,
            bottom_bucket_avg_label: -0.1,
            top_bucket_n: 20,
            top_bucket_avg_label: 0.2,
            top_bucket_positive_label_rate: 0.7,
            top_bucket_full_depth_entry_fill_rate: 0.8,
            top_bucket_unique_event_count: 20,
            top_bucket_max_event_decisions: 1,
            monotonicity_score: 1.0,
            complexity: 1,
            decision: AutoFactorDecision::Candidate,
            reason: "passed".to_string(),
        };
        let prior = MctsSearchStateArtifact {
            version: ALPHA_SEARCH_ARTIFACT_VERSION.to_string(),
            mode: "cumulative_ucb_state".to_string(),
            target: "full_depth_settlement_executable_pnl".to_string(),
            total_visits: 3,
            nodes: vec![MctsSearchStateNode {
                factor_name: "auto_settlement_conservative_settlement_edge".to_string(),
                visits: 3,
                total_reward: 6.0,
                best_reward: 2.0,
                last_reward: 2.0,
                selected_dimension: "exploit".to_string(),
                last_decision: "candidate".to_string(),
            }],
        };

        write_alpha_search_artifacts_with_state(
            &tmp,
            "full_depth_settlement_executable_pnl",
            &["conservative_settlement_edge".to_string()],
            &[report],
            &AutoFactorOptions::default(),
            Some(&prior),
        )
        .expect("write artifacts");

        let state = read_mcts_search_state(
            tmp.join("full_depth_settlement_executable_pnl/mcts-state.json"),
        )
        .expect("state");
        let node = state
            .nodes
            .iter()
            .find(|node| node.factor_name == "auto_settlement_conservative_settlement_edge")
            .expect("merged node");
        assert_eq!(node.visits, 4);
        assert!(node.total_reward > 6.0);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
