use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use chrono::Datelike;
use serde::{Deserialize, Serialize};

use crate::factors::{pearson_ic, spearman_ic};
use crate::factors_v2::FactorObservationV2;

const EPS: f64 = 1e-9;
const MAX_DETERMINISTIC_MUTATION_DEPTH: usize = 2;
const MAX_DETERMINISTIC_MUTATION_CANDIDATES: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FactorExpr {
    Input(String),
    Const(f64),
    Add(Box<FactorExpr>, Box<FactorExpr>),
    Sub(Box<FactorExpr>, Box<FactorExpr>),
    Mul(Box<FactorExpr>, Box<FactorExpr>),
    SafeDiv(Box<FactorExpr>, Box<FactorExpr>),
    Max(Box<FactorExpr>, Box<FactorExpr>),
    Min(Box<FactorExpr>, Box<FactorExpr>),
    Tanh(Box<FactorExpr>),
    Log1pAbs(Box<FactorExpr>),
    SqrtAbs(Box<FactorExpr>),
    Clip {
        expr: Box<FactorExpr>,
        lo: f64,
        hi: f64,
    },
    Delta {
        expr: Box<FactorExpr>,
        lag: usize,
    },
    RollingMean {
        expr: Box<FactorExpr>,
        window: usize,
    },
    RollingStd {
        expr: Box<FactorExpr>,
        window: usize,
    },
    ZScore {
        expr: Box<FactorExpr>,
        window: usize,
    },
    Gate {
        expr: Box<FactorExpr>,
        gate: Box<FactorExpr>,
        min: f64,
    },
}

impl FactorExpr {
    pub fn input_names(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect_input_names(&mut out);
        out
    }

    fn collect_input_names(&self, out: &mut BTreeSet<String>) {
        match self {
            FactorExpr::Input(name) => {
                out.insert(name.clone());
            }
            FactorExpr::Const(_) => {}
            FactorExpr::Add(lhs, rhs)
            | FactorExpr::Sub(lhs, rhs)
            | FactorExpr::Mul(lhs, rhs)
            | FactorExpr::SafeDiv(lhs, rhs)
            | FactorExpr::Max(lhs, rhs)
            | FactorExpr::Min(lhs, rhs) => {
                lhs.collect_input_names(out);
                rhs.collect_input_names(out);
            }
            FactorExpr::Tanh(expr)
            | FactorExpr::Log1pAbs(expr)
            | FactorExpr::SqrtAbs(expr)
            | FactorExpr::Delta { expr, .. }
            | FactorExpr::RollingMean { expr, .. }
            | FactorExpr::RollingStd { expr, .. }
            | FactorExpr::ZScore { expr, .. }
            | FactorExpr::Clip { expr, .. } => expr.collect_input_names(out),
            FactorExpr::Gate { expr, gate, .. } => {
                expr.collect_input_names(out);
                gate.collect_input_names(out);
            }
        }
    }

    pub fn complexity(&self) -> usize {
        match self {
            FactorExpr::Input(_) | FactorExpr::Const(_) => 1,
            FactorExpr::Add(lhs, rhs)
            | FactorExpr::Sub(lhs, rhs)
            | FactorExpr::Mul(lhs, rhs)
            | FactorExpr::SafeDiv(lhs, rhs)
            | FactorExpr::Max(lhs, rhs)
            | FactorExpr::Min(lhs, rhs) => 1 + lhs.complexity() + rhs.complexity(),
            FactorExpr::Tanh(expr)
            | FactorExpr::Log1pAbs(expr)
            | FactorExpr::SqrtAbs(expr)
            | FactorExpr::Delta { expr, .. }
            | FactorExpr::RollingMean { expr, .. }
            | FactorExpr::RollingStd { expr, .. }
            | FactorExpr::ZScore { expr, .. }
            | FactorExpr::Clip { expr, .. } => 1 + expr.complexity(),
            FactorExpr::Gate { expr, gate, .. } => 1 + expr.complexity() + gate.complexity(),
        }
    }

    pub fn evaluate(&self, matrix: &AutoFactorMatrix) -> Result<Vec<f64>, AutoFactorError> {
        match self {
            FactorExpr::Input(name) => matrix
                .column(name)
                .map(|values| values.to_vec())
                .ok_or_else(|| AutoFactorError::MissingInput(name.clone())),
            FactorExpr::Const(value) => Ok(vec![*value; matrix.len()]),
            FactorExpr::Add(lhs, rhs) => binary_eval(lhs, rhs, matrix, |a, b| a + b),
            FactorExpr::Sub(lhs, rhs) => binary_eval(lhs, rhs, matrix, |a, b| a - b),
            FactorExpr::Mul(lhs, rhs) => binary_eval(lhs, rhs, matrix, |a, b| a * b),
            FactorExpr::SafeDiv(lhs, rhs) => binary_eval_raw(lhs, rhs, matrix, safe_div_value),
            FactorExpr::Max(lhs, rhs) => binary_eval(lhs, rhs, matrix, f64::max),
            FactorExpr::Min(lhs, rhs) => binary_eval(lhs, rhs, matrix, f64::min),
            FactorExpr::Tanh(expr) => unary_eval(expr, matrix, f64::tanh),
            FactorExpr::Log1pAbs(expr) => unary_eval(expr, matrix, log1p_abs),
            FactorExpr::SqrtAbs(expr) => unary_eval(expr, matrix, |value| value.abs().sqrt()),
            FactorExpr::Clip { expr, lo, hi } => unary_eval(expr, matrix, |value| {
                if value.is_finite() {
                    value.max(*lo).min(*hi)
                } else {
                    f64::NAN
                }
            }),
            FactorExpr::Delta { expr, lag } => Ok(delta_series(&expr.evaluate(matrix)?, *lag)),
            FactorExpr::RollingMean { expr, window } => {
                Ok(rolling_mean(&expr.evaluate(matrix)?, *window))
            }
            FactorExpr::RollingStd { expr, window } => {
                Ok(rolling_std(&expr.evaluate(matrix)?, *window))
            }
            FactorExpr::ZScore { expr, window } => Ok(zscore(&expr.evaluate(matrix)?, *window)),
            FactorExpr::Gate { expr, gate, min } => gate_eval(expr, gate, *min, matrix),
        }
    }
}

pub fn factor_expr_hash(expr: &FactorExpr) -> Result<String, serde_json::Error> {
    use sha2::{Digest, Sha256};

    let raw = serde_json::to_vec(expr)?;
    let mut hasher = Sha256::new();
    hasher.update(raw);
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedFactorExpr {
    pub name: String,
    pub expr: FactorExpr,
    pub target: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmPriorSpec {
    #[serde(default)]
    pub mutations: Vec<LlmMutationSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMutationSpec {
    pub base_factor: String,
    pub mutation_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default)]
    pub denominator_feature: Option<String>,
    #[serde(default)]
    pub constant: Option<f64>,
    #[serde(default)]
    pub lo: Option<f64>,
    #[serde(default)]
    pub hi: Option<f64>,
    #[serde(default)]
    pub window: Option<usize>,
}

impl NamedFactorExpr {
    pub fn new(name: impl Into<String>, expr: FactorExpr) -> Self {
        Self {
            name: name.into(),
            expr,
            target: None,
            notes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AutoFactorMatrix {
    columns: BTreeMap<String, Vec<f64>>,
    len: usize,
}

impl AutoFactorMatrix {
    pub fn new(columns: BTreeMap<String, Vec<f64>>) -> Result<Self, AutoFactorError> {
        let mut len = None;
        for (name, values) in &columns {
            match len {
                Some(expected) if values.len() != expected => {
                    return Err(AutoFactorError::LengthMismatch {
                        name: name.clone(),
                        expected,
                        actual: values.len(),
                    });
                }
                None => len = Some(values.len()),
                _ => {}
            }
        }
        Ok(Self {
            columns,
            len: len.unwrap_or(0),
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn column(&self, name: &str) -> Option<&[f64]> {
        self.columns.get(name).map(Vec::as_slice)
    }

    pub fn input_names(&self) -> BTreeSet<String> {
        self.columns
            .keys()
            .filter(|name| !name.starts_with("__"))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoFactorOptions {
    pub min_observations: usize,
    pub min_window_observations: usize,
    pub bucket_count: usize,
    pub min_spearman_ic: f64,
    pub min_icir: f64,
    pub min_positive_window_ratio: f64,
    pub min_top_bucket_avg_label: f64,
    pub min_top_bucket_full_depth_entry_fill_rate: f64,
    pub min_monotonicity_score: f64,
    pub max_complexity: usize,
}

impl Default for AutoFactorOptions {
    fn default() -> Self {
        Self {
            min_observations: 50,
            min_window_observations: 50,
            bucket_count: 5,
            min_spearman_ic: 0.0,
            min_icir: 0.5,
            min_positive_window_ratio: 0.60,
            min_top_bucket_avg_label: 0.0,
            min_top_bucket_full_depth_entry_fill_rate: 0.0,
            min_monotonicity_score: 0.50,
            max_complexity: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoFactorDecision {
    Candidate,
    Watchlist,
    Reject,
}

impl AutoFactorDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoFactorDecision::Candidate => "candidate",
            AutoFactorDecision::Watchlist => "watchlist",
            AutoFactorDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoFactorReport {
    pub name: String,
    pub target: Option<String>,
    pub expr: FactorExpr,
    pub n: usize,
    pub pearson_ic: f64,
    pub spearman_ic: f64,
    pub window_count: usize,
    pub window_ic_mean: f64,
    pub icir: f64,
    pub positive_window_ratio: f64,
    pub symbol_count: usize,
    pub symbol_ic_mean: f64,
    pub symbol_icir: f64,
    pub symbol_positive_ratio: f64,
    pub bucket_avg_labels: Vec<f64>,
    pub bottom_bucket_n: usize,
    pub bottom_bucket_avg_label: f64,
    pub top_bucket_n: usize,
    pub top_bucket_avg_label: f64,
    pub top_bucket_positive_label_rate: f64,
    pub top_bucket_full_depth_entry_fill_rate: f64,
    pub top_bucket_unique_event_count: usize,
    pub top_bucket_max_event_decisions: usize,
    pub monotonicity_score: f64,
    pub complexity: usize,
    pub decision: AutoFactorDecision,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoFactorV2Target {
    RepricePnl10s,
    RepricePnl30s,
    FullDepthRepricePnl10s,
    FullDepthRepricePnl30s,
    SettlementExecutablePnl,
    FullDepthSettlementExecutablePnl,
    TradeableFullDepthSettlementPnl,
}

impl AutoFactorV2Target {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoFactorV2Target::RepricePnl10s => "reprice_pnl_10s",
            AutoFactorV2Target::RepricePnl30s => "reprice_pnl_30s",
            AutoFactorV2Target::FullDepthRepricePnl10s => "full_depth_reprice_pnl_10s",
            AutoFactorV2Target::FullDepthRepricePnl30s => "full_depth_reprice_pnl_30s",
            AutoFactorV2Target::SettlementExecutablePnl => "settlement_executable_pnl",
            AutoFactorV2Target::FullDepthSettlementExecutablePnl => {
                "full_depth_settlement_executable_pnl"
            }
            AutoFactorV2Target::TradeableFullDepthSettlementPnl => {
                "tradeable_full_depth_settlement_pnl"
            }
        }
    }

    fn label(self, row: &FactorObservationV2) -> f64 {
        match self {
            AutoFactorV2Target::RepricePnl10s => row.label_future_exit_pnl_10s,
            AutoFactorV2Target::RepricePnl30s => row.label_future_exit_pnl_30s,
            AutoFactorV2Target::FullDepthRepricePnl10s => row.label_future_exit_full_depth_pnl_10s,
            AutoFactorV2Target::FullDepthRepricePnl30s => row.label_future_exit_full_depth_pnl_30s,
            AutoFactorV2Target::SettlementExecutablePnl => row.label_executable_pnl_15u,
            AutoFactorV2Target::FullDepthSettlementExecutablePnl => {
                row.label_full_depth_executable_pnl_15u
            }
            AutoFactorV2Target::TradeableFullDepthSettlementPnl => {
                if row.label_settlement_win.is_none() {
                    None
                } else if row.label_full_depth_entry_fillable {
                    row.label_full_depth_executable_pnl_15u
                } else {
                    Some(0.0)
                }
            }
        }
        .unwrap_or(f64::NAN)
    }
}

#[derive(Debug, Clone)]
struct BucketSummary {
    n: usize,
    avg_label: f64,
    positive_label_rate: f64,
    full_depth_entry_fill_rate: f64,
    indexes: Vec<usize>,
}

pub fn evaluate_named_factor(
    factor: &NamedFactorExpr,
    matrix: &AutoFactorMatrix,
    labels: &[f64],
    windows: &[String],
    symbols: &[String],
    event_ids: &[String],
    options: &AutoFactorOptions,
) -> Result<AutoFactorReport, AutoFactorError> {
    if labels.len() != matrix.len() {
        return Err(AutoFactorError::LabelLengthMismatch {
            expected: matrix.len(),
            actual: labels.len(),
        });
    }
    if !windows.is_empty() && windows.len() != matrix.len() {
        return Err(AutoFactorError::WindowLengthMismatch {
            expected: matrix.len(),
            actual: windows.len(),
        });
    }
    if !symbols.is_empty() && symbols.len() != matrix.len() {
        return Err(AutoFactorError::WindowLengthMismatch {
            expected: matrix.len(),
            actual: symbols.len(),
        });
    }
    if !event_ids.is_empty() && event_ids.len() != matrix.len() {
        return Err(AutoFactorError::WindowLengthMismatch {
            expected: matrix.len(),
            actual: event_ids.len(),
        });
    }

    let signal = factor.expr.evaluate(matrix)?;
    let scored: Vec<(usize, f64, f64)> = signal
        .iter()
        .zip(labels.iter())
        .enumerate()
        .filter_map(|(idx, (score, label))| {
            (score.is_finite() && label.is_finite()).then_some((idx, *score, *label))
        })
        .collect();

    let pairs: Vec<(f64, f64)> = scored
        .iter()
        .map(|(_, score, label)| (*score, *label))
        .collect();
    let xs = pairs.iter().map(|(score, _)| *score).collect::<Vec<_>>();
    let ys = pairs.iter().map(|(_, label)| *label).collect::<Vec<_>>();
    let pearson = pearson_ic(&xs, &ys);
    let spearman = spearman_ic(&xs, &ys);

    let mut grouped: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    for (idx, score, label) in &scored {
        let key = if windows.is_empty() {
            "all"
        } else {
            windows[*idx].as_str()
        };
        grouped.entry(key).or_default().push((*score, *label));
    }
    let window_ics = grouped
        .values()
        .filter(|pairs| pairs.len() >= options.min_window_observations)
        .map(|pairs| {
            let xs = pairs.iter().map(|(score, _)| *score).collect::<Vec<_>>();
            let ys = pairs.iter().map(|(_, label)| *label).collect::<Vec<_>>();
            spearman_ic(&xs, &ys)
        })
        .filter(|ic| ic.is_finite())
        .collect::<Vec<_>>();
    let positive_window_ratio = ratio(
        window_ics.iter().filter(|ic| **ic > 0.0).count(),
        window_ics.len(),
    );
    let window_ic_mean = finite_mean(window_ics.iter().copied());
    let factor_icir = icir(&window_ics);
    let mut grouped_symbols: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    for (idx, score, label) in &scored {
        let key = if symbols.is_empty() {
            "all"
        } else {
            symbols[*idx].as_str()
        };
        grouped_symbols
            .entry(key)
            .or_default()
            .push((*score, *label));
    }
    let symbol_ics = grouped_symbols
        .values()
        .filter(|pairs| pairs.len() >= options.min_window_observations)
        .map(|pairs| {
            let xs = pairs.iter().map(|(score, _)| *score).collect::<Vec<_>>();
            let ys = pairs.iter().map(|(_, label)| *label).collect::<Vec<_>>();
            spearman_ic(&xs, &ys)
        })
        .filter(|ic| ic.is_finite())
        .collect::<Vec<_>>();
    let symbol_positive_ratio = ratio(
        symbol_ics.iter().filter(|ic| **ic > 0.0).count(),
        symbol_ics.len(),
    );
    let symbol_ic_mean = finite_mean(symbol_ics.iter().copied());
    let symbol_icir = icir(&symbol_ics);

    let buckets = build_buckets(
        &scored,
        options.bucket_count,
        matrix.column("full_depth_entry_fillable_gate"),
    );
    let bucket_avg_labels = buckets
        .iter()
        .map(|bucket| bucket.avg_label)
        .collect::<Vec<_>>();
    let monotonicity_score = monotonicity_score(&bucket_avg_labels);
    let bottom = buckets.first();
    let event_level_top = event_level_bucket_summary(
        buckets.last(),
        &scored,
        event_ids,
        matrix.column("full_depth_entry_fillable_gate"),
    );
    let top = event_level_top.as_ref().or_else(|| buckets.last());
    let (top_bucket_unique_event_count, top_bucket_max_event_decisions) =
        top_bucket_event_decision_stats(top, event_ids);
    let complexity = factor.expr.complexity();
    let (decision, reason) = autofactor_decision(
        scored.len(),
        complexity,
        spearman,
        window_ics.len(),
        factor_icir,
        positive_window_ratio,
        top.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top.map(|bucket| bucket.full_depth_entry_fill_rate)
            .unwrap_or(f64::NAN),
        monotonicity_score,
        options,
    );

    Ok(AutoFactorReport {
        name: factor.name.clone(),
        target: factor.target.clone(),
        expr: factor.expr.clone(),
        n: scored.len(),
        pearson_ic: pearson,
        spearman_ic: spearman,
        window_count: window_ics.len(),
        window_ic_mean,
        icir: factor_icir,
        positive_window_ratio,
        symbol_count: symbol_ics.len(),
        symbol_ic_mean,
        symbol_icir,
        symbol_positive_ratio,
        bucket_avg_labels,
        bottom_bucket_n: bottom.map(|bucket| bucket.n).unwrap_or(0),
        bottom_bucket_avg_label: bottom.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_n: top.map(|bucket| bucket.n).unwrap_or(0),
        top_bucket_avg_label: top.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_positive_label_rate: top
            .map(|bucket| bucket.positive_label_rate)
            .unwrap_or(f64::NAN),
        top_bucket_full_depth_entry_fill_rate: top
            .map(|bucket| bucket.full_depth_entry_fill_rate)
            .unwrap_or(f64::NAN),
        top_bucket_unique_event_count,
        top_bucket_max_event_decisions,
        monotonicity_score,
        complexity,
        decision,
        reason,
    })
}

pub fn mine_autofactors(
    factors: &[NamedFactorExpr],
    matrix: &AutoFactorMatrix,
    labels: &[f64],
    windows: &[String],
    symbols: &[String],
    options: &AutoFactorOptions,
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    mine_autofactors_with_event_ids(factors, matrix, labels, windows, symbols, &[], options)
}

pub fn mine_autofactors_with_event_ids(
    factors: &[NamedFactorExpr],
    matrix: &AutoFactorMatrix,
    labels: &[f64],
    windows: &[String],
    symbols: &[String],
    event_ids: &[String],
    options: &AutoFactorOptions,
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    let mut reports = Vec::with_capacity(factors.len());
    for factor in factors {
        reports.push(evaluate_named_factor(
            factor, matrix, labels, windows, symbols, event_ids, options,
        )?);
    }
    reports.sort_by(|lhs, rhs| {
        decision_rank(rhs.decision)
            .cmp(&decision_rank(lhs.decision))
            .then_with(|| rhs.icir.total_cmp(&lhs.icir))
            .then_with(|| rhs.spearman_ic.total_cmp(&lhs.spearman_ic))
            .then_with(|| {
                rhs.top_bucket_avg_label
                    .total_cmp(&lhs.top_bucket_avg_label)
            })
    });
    Ok(reports)
}

pub fn format_autofactor_reports(reports: &[AutoFactorReport], top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== AutoFactor Seed Candidate Report ===\n");
    out.push_str(
        "target labels are side-aligned executable PnL for the requested target; reports are candidate discovery gates, not deploy decisions.\n",
    );
    out.push_str(
        "rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity\n",
    );
    for (idx, report) in reports.iter().take(top_n).enumerate() {
        out.push_str(&format!(
            "{},{},{},{},{},{},{:.6},{:.6},{},{:.6},{:.4},{},{:.4},{:.4},{},{:.6},{:.4},{:.4},{},{},{}\n",
            idx + 1,
            report.name,
            report.target.as_deref().unwrap_or("<unspecified>"),
            report.decision.as_str(),
            report.reason,
            report.n,
            report.spearman_ic,
            report.pearson_ic,
            report.window_count,
            report.icir,
            report.positive_window_ratio,
            report.symbol_count,
            report.symbol_positive_ratio,
            report.monotonicity_score,
            report.top_bucket_n,
            report.top_bucket_avg_label,
            report.top_bucket_positive_label_rate,
            report.top_bucket_full_depth_entry_fill_rate,
            report.top_bucket_unique_event_count,
            report.top_bucket_max_event_decisions,
            report.complexity,
        ));
    }
    out
}

pub fn mine_domain_autofactors_from_v2(
    rows: &[FactorObservationV2],
    target: AutoFactorV2Target,
    options: &AutoFactorOptions,
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    mine_domain_autofactors_from_v2_with_mcts_plan(rows, target, options, &[])
}

pub fn mine_domain_autofactors_from_v2_with_mcts_plan(
    rows: &[FactorObservationV2],
    target: AutoFactorV2Target,
    options: &AutoFactorOptions,
    mcts_selected_factor_names: &[String],
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    mine_domain_autofactors_from_v2_with_guidance(
        rows,
        target,
        options,
        mcts_selected_factor_names,
        None,
    )
}

pub fn mine_domain_autofactors_from_v2_with_guidance(
    rows: &[FactorObservationV2],
    target: AutoFactorV2Target,
    options: &AutoFactorOptions,
    mcts_selected_factor_names: &[String],
    llm_prior: Option<&LlmPriorSpec>,
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    let matrix = autofactor_matrix_from_v2(rows)?;
    let labels = autofactor_labels_from_v2(rows, target);
    let windows = autofactor_windows_from_v2(rows);
    let symbols = autofactor_symbols_from_v2(rows);
    let event_ids = autofactor_event_ids_from_v2(rows);
    let target_name = target.as_str().to_string();
    let candidates = domain_candidates_for_target_with_guidance(
        &matrix.input_names(),
        target,
        mcts_selected_factor_names,
        llm_prior,
    )
    .into_iter()
    .map(|mut factor| {
        factor.target = Some(target_name.clone());
        factor
    })
    .collect::<Vec<_>>();
    mine_autofactors_with_event_ids(
        &candidates,
        &matrix,
        &labels,
        &windows,
        &symbols,
        &event_ids,
        options,
    )
}

pub fn autofactor_matrix_from_v2(
    rows: &[FactorObservationV2],
) -> Result<AutoFactorMatrix, AutoFactorError> {
    let mut columns = BTreeMap::new();
    insert_column(&mut columns, "side_model_edge", rows, |row| {
        row.side_model_edge
    });
    insert_column(&mut columns, "side_fair_prob", rows, |row| {
        row.side_fair_prob
    });
    insert_column(&mut columns, "side_fair_edge", rows, |row| {
        if valid_pm_price(row.entry_ask) && row.side_fair_prob.is_finite() {
            row.side_fair_prob - row.entry_ask - pm_fee_cost(row.entry_ask)
        } else {
            f64::NAN
        }
    });
    insert_column(&mut columns, "full_depth_settlement_edge", rows, |row| {
        settlement_edge(row.side_fair_prob, row.entry_sweep_avg_price_15u)
    });
    insert_column(&mut columns, "conservative_settlement_edge", rows, |row| {
        settlement_edge(
            row.side_fair_prob,
            row.conservative_entry_sweep_avg_price_15u,
        )
    });
    insert_column(&mut columns, "entry_capacity_score", rows, |row| {
        if row.entry_capacity_ratio.is_finite() {
            (row.entry_capacity_ratio / 3.0).clamp(0.0, 1.0)
        } else {
            f64::NAN
        }
    });
    insert_column(
        &mut columns,
        "full_depth_entry_fillable_gate",
        rows,
        |row| {
            if row.label_full_depth_entry_fillable {
                1.0
            } else {
                0.0
            }
        },
    );
    insert_column(&mut columns, "entry_price_quality_score", rows, |row| {
        entry_price_quality_score(row.entry_ask)
    });
    insert_column(&mut columns, "repricing_gap_side_10s", rows, |row| {
        row.side_model_edge
    });
    insert_column(&mut columns, "external_pressure", rows, |row| {
        row.cex_continuation_score_side
    });
    insert_column(&mut columns, "cex_continuation_score_side", rows, |row| {
        row.cex_continuation_score_side
    });
    insert_column(
        &mut columns,
        "external_move_since_poly_update",
        rows,
        |row| row.cex_bar_return_30s * row.side.multiplier(),
    );
    insert_column(&mut columns, "cex_return_30s_side", rows, |row| {
        row.cex_bar_return_30s * row.side.multiplier()
    });
    insert_column(&mut columns, "cex_return_60s_side", rows, |row| {
        row.cex_bar_return_60s * row.side.multiplier()
    });
    insert_column(&mut columns, "sigma_horizon_pos", rows, |row| {
        positive_or_nan(row.sigma_horizon)
    });
    insert_column(&mut columns, "vol_gap_pos", rows, |row| {
        positive_or_nan(row.vol_gap)
    });
    insert_column(&mut columns, "poly_quote_age", rows, |row| {
        row.pm_lag_secs.max(0.0)
    });
    insert_column(&mut columns, "pm_lag_secs", rows, |row| {
        row.pm_lag_secs.max(0.0)
    });
    insert_column(&mut columns, "ofi_l5", rows, |row| {
        row.obi_10 * row.side.multiplier()
    });
    insert_column(&mut columns, "depth_top5", rows, |row| {
        positive_or_nan(row.entry_liquidity_usd)
    });
    insert_column(&mut columns, "near_strike_score", rows, near_strike_score);
    insert_column(&mut columns, "iv_change_1m", rows, |row| {
        first_finite(row.deribit_iv_change_60s, row.deribit_iv_change_30s)
    });
    insert_column(&mut columns, "lob_thinness", rows, |row| {
        inverse_liquidity(row.entry_liquidity_usd)
    });
    insert_column(&mut columns, "side_spread", rows, |row| {
        if row.pm_spread_bps.is_finite() {
            row.pm_spread_bps / 10_000.0
        } else {
            f64::NAN
        }
    });
    AutoFactorMatrix::new(columns)
}

pub fn autofactor_labels_from_v2(
    rows: &[FactorObservationV2],
    target: AutoFactorV2Target,
) -> Vec<f64> {
    rows.iter().map(|row| target.label(row)).collect()
}

pub fn autofactor_windows_from_v2(rows: &[FactorObservationV2]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let date = row.tick_ts.date_naive();
            format!(
                "{}|{:04}-{:02}-{:02}|{}|{}",
                row.symbol,
                date.year(),
                date.month(),
                date.day(),
                time_remaining_bucket(row.time_remaining_secs),
                distance_bucket(row.side_distance_over_sigma.abs())
            )
        })
        .collect()
}

pub fn autofactor_symbols_from_v2(rows: &[FactorObservationV2]) -> Vec<String> {
    rows.iter().map(|row| row.symbol.clone()).collect()
}

pub fn autofactor_event_ids_from_v2(rows: &[FactorObservationV2]) -> Vec<String> {
    rows.iter().map(|row| row.event_id.clone()).collect()
}

fn valid_pm_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn pm_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

fn settlement_edge(probability: f64, entry_price: f64) -> f64 {
    if valid_pm_price(entry_price) && probability.is_finite() {
        probability - entry_price - pm_fee_cost(entry_price)
    } else {
        f64::NAN
    }
}

pub fn domain_seed_candidates(input_names: &BTreeSet<String>) -> Vec<NamedFactorExpr> {
    let mut out = Vec::new();
    if input_names.contains("repricing_gap_side_10s") {
        out.push(NamedFactorExpr {
            name: "repricing_gap_side_10s".to_string(),
            expr: input("repricing_gap_side_10s"),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec!["Side-aligned fair-minus-entry gap proxy.".to_string()],
        });
    }
    if input_names.contains("side_fair_edge") {
        out.push(NamedFactorExpr {
            name: "settlement_fair_edge".to_string(),
            expr: input("side_fair_edge"),
            target: Some("settlement_executable_pnl".to_string()),
            notes: vec!["Settlement fair probability minus executable ask and fee.".to_string()],
        });
    }
    if has_all(input_names, &["ofi_l5", "depth_top5"]) {
        out.push(NamedFactorExpr {
            name: "ofi_l5_depth_norm".to_string(),
            expr: safe_div_expr(input("ofi_l5"), input("depth_top5")),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec!["External OFI scaled by local depth.".to_string()],
        });
    }
    if has_all(
        input_names,
        &[
            "external_pressure",
            "external_move_since_poly_update",
            "poly_quote_age",
        ],
    ) {
        out.push(NamedFactorExpr {
            name: "poly_lag_pressure".to_string(),
            expr: mul(
                mul(
                    input("external_pressure"),
                    FactorExpr::Log1pAbs(Box::new(input("external_move_since_poly_update"))),
                ),
                FactorExpr::Tanh(Box::new(safe_div_expr(
                    input("poly_quote_age"),
                    FactorExpr::Const(3.0),
                ))),
            ),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec![
                "External move is more actionable when Polymarket quote is stale.".to_string(),
            ],
        });
    }
    if has_all(
        input_names,
        &["near_strike_score", "iv_change_1m", "lob_thinness"],
    ) {
        out.push(NamedFactorExpr {
            name: "near_strike_iv_shock".to_string(),
            expr: mul(
                mul(input("near_strike_score"), input("iv_change_1m")),
                input("lob_thinness"),
            ),
            target: Some("tradable_move_10s".to_string()),
            notes: vec!["Vol shock matters most near strike and in thin LOB regimes.".to_string()],
        });
    }
    if has_all(
        input_names,
        &[
            "ofi_l5",
            "depth_top5",
            "near_strike_score",
            "poly_quote_age",
        ],
    ) {
        out.push(NamedFactorExpr {
            name: "stale_ofi_near_strike".to_string(),
            expr: mul(
                mul(
                    FactorExpr::Tanh(Box::new(FactorExpr::ZScore {
                        expr: Box::new(safe_div_expr(input("ofi_l5"), input("depth_top5"))),
                        window: 300,
                    })),
                    input("near_strike_score"),
                ),
                FactorExpr::Tanh(Box::new(safe_div_expr(
                    input("poly_quote_age"),
                    FactorExpr::Const(3.0),
                ))),
            ),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec![
                "Depth-normalized OFI should matter more when the contract is near strike and PM is stale."
                    .to_string(),
            ],
        });
    }
    if has_all(
        input_names,
        &["external_move_since_poly_update", "side_spread"],
    ) {
        out.push(NamedFactorExpr {
            name: "spread_adjusted_external_move".to_string(),
            expr: safe_div_expr(
                input("external_move_since_poly_update"),
                FactorExpr::Add(
                    Box::new(input("side_spread")),
                    Box::new(FactorExpr::Const(0.01)),
                ),
            ),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec!["External move must clear the side spread to be tradable.".to_string()],
        });
    }
    if has_all(input_names, &["cex_return_30s_side", "sigma_horizon_pos"]) {
        out.push(NamedFactorExpr {
            name: "amplitude_weighted_momentum_30s_sigma".to_string(),
            expr: mul(
                input("cex_return_30s_side"),
                FactorExpr::Log1pAbs(Box::new(input("sigma_horizon_pos"))),
            ),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec![
                "Side-aligned 30s CEX return weighted by current event volatility amplitude."
                    .to_string(),
            ],
        });
    }
    if has_all(input_names, &["cex_return_30s_side", "vol_gap_pos"]) {
        out.push(NamedFactorExpr {
            name: "amplitude_weighted_momentum_30s_vol_gap".to_string(),
            expr: mul(
                input("cex_return_30s_side"),
                FactorExpr::Log1pAbs(Box::new(input("vol_gap_pos"))),
            ),
            target: Some("reprice_pnl_10s".to_string()),
            notes: vec![
                "Side-aligned 30s CEX return weighted by positive volatility shock versus implied baseline."
                    .to_string(),
            ],
        });
    }
    out
}

fn domain_candidates_for_target_with_guidance(
    input_names: &BTreeSet<String>,
    target: AutoFactorV2Target,
    mcts_selected_factor_names: &[String],
    llm_prior: Option<&LlmPriorSpec>,
) -> Vec<NamedFactorExpr> {
    let mut out = domain_seed_candidates(input_names);
    if matches!(
        target,
        AutoFactorV2Target::FullDepthSettlementExecutablePnl
            | AutoFactorV2Target::TradeableFullDepthSettlementPnl
    ) {
        out.extend(settlement_native_generated_candidates(input_names));
    }
    out.extend(deterministic_mutation_candidates(input_names, &out, target));
    if !mcts_selected_factor_names.is_empty() {
        let selected = out
            .iter()
            .filter(|candidate| mcts_selected_factor_names.contains(&candidate.name))
            .cloned()
            .collect::<Vec<_>>();
        out.extend(
            deterministic_mutation_layer(input_names, &selected, target, 3)
                .into_iter()
                .map(|mut candidate| {
                    candidate.name = candidate.name.replacen("mut2_", "mcts_", 1);
                    candidate.name = candidate.name.replacen("mut_", "mcts_", 1);
                    candidate.notes.push(
                        "MCTS-guided expansion from prior mcts-expansion-plan.json selection."
                            .to_string(),
                    );
                    candidate
                }),
        );
    }
    if let Some(prior) = llm_prior {
        let base = out.clone();
        out.extend(llm_prior_mutation_candidates(input_names, &base, prior));
    }
    out
}

fn llm_prior_mutation_candidates(
    input_names: &BTreeSet<String>,
    candidates: &[NamedFactorExpr],
    prior: &LlmPriorSpec,
) -> Vec<NamedFactorExpr> {
    let by_name = candidates
        .iter()
        .map(|candidate| (candidate.name.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let mut out = Vec::new();
    for mutation in &prior.mutations {
        let Some(base) = by_name.get(mutation.base_factor.as_str()) else {
            continue;
        };
        let Some((suffix, expr)) = compile_llm_mutation(input_names, base, mutation) else {
            continue;
        };
        let name = mutation
            .name
            .as_ref()
            .map(|name| sanitize_factor_name(name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("llm_{}_{}", base.name, suffix));
        out.push(NamedFactorExpr {
            name,
            expr,
            target: base.target.clone(),
            notes: vec![format!(
                "Typed LLM-prior mutation `{}` compiled from base factor `{}`.",
                mutation.mutation_type, base.name
            )],
        });
    }
    out
}

fn compile_llm_mutation(
    input_names: &BTreeSet<String>,
    base: &NamedFactorExpr,
    mutation: &LlmMutationSpec,
) -> Option<(&'static str, FactorExpr)> {
    let constant = mutation.constant.unwrap_or(0.01);
    if !constant.is_finite() || constant.abs() > 300.0 {
        return None;
    }
    match mutation.mutation_type.as_str() {
        "add_feature_gate" => {
            let feature = existing_feature(input_names, mutation.feature.as_deref())?;
            if feature == "full_depth_entry_fillable_gate" {
                Some((
                    "full_depth_entry_gate",
                    gate(base.expr.clone(), input(feature), 0.5),
                ))
            } else {
                Some(("feature_gate", mul(base.expr.clone(), input(feature))))
            }
        }
        "add_capacity_gate" => {
            let feature = mutation.feature.as_deref().unwrap_or(
                if base.target.as_deref() == Some("tradeable_full_depth_settlement_pnl")
                    && input_names.contains("full_depth_entry_fillable_gate")
                {
                    "full_depth_entry_fillable_gate"
                } else {
                    "entry_capacity_score"
                },
            );
            let feature = existing_feature(input_names, Some(feature))?;
            if feature == "full_depth_entry_fillable_gate" {
                Some((
                    "full_depth_entry_gate",
                    gate(base.expr.clone(), input(feature), 0.5),
                ))
            } else {
                Some(("capacity_gate", mul(base.expr.clone(), input(feature))))
            }
        }
        "add_near_strike_interaction" => {
            let feature = existing_feature(input_names, Some("near_strike_score"))?;
            Some(("near_strike", mul(base.expr.clone(), input(feature))))
        }
        "add_spread_penalty" => {
            let feature = existing_feature(input_names, Some("side_spread"))?;
            Some((
                "spread_penalty",
                safe_div_expr(
                    base.expr.clone(),
                    FactorExpr::Add(
                        Box::new(input(feature)),
                        Box::new(FactorExpr::Const(constant)),
                    ),
                ),
            ))
        }
        "replace_denominator" => {
            let feature = existing_feature(
                input_names,
                mutation
                    .denominator_feature
                    .as_deref()
                    .or(mutation.feature.as_deref()),
            )?;
            Some((
                "replace_denominator",
                safe_div_expr(
                    base.expr.clone(),
                    FactorExpr::Add(
                        Box::new(input(feature)),
                        Box::new(FactorExpr::Const(constant)),
                    ),
                ),
            ))
        }
        "clip_or_squash" => {
            if let (Some(lo), Some(hi)) = (mutation.lo, mutation.hi) {
                if lo.is_finite() && hi.is_finite() && lo < hi {
                    return Some((
                        "clip",
                        FactorExpr::Clip {
                            expr: Box::new(base.expr.clone()),
                            lo,
                            hi,
                        },
                    ));
                }
            }
            Some(("squash", FactorExpr::Tanh(Box::new(base.expr.clone()))))
        }
        "change_time_window" => {
            let window = mutation.window.unwrap_or(30);
            if !(1..=300).contains(&window) {
                return None;
            }
            Some((
                "rolling_mean",
                FactorExpr::RollingMean {
                    expr: Box::new(base.expr.clone()),
                    window,
                },
            ))
        }
        "invert_or_contrarian" => Some((
            "contrarian",
            mul(FactorExpr::Const(-1.0), base.expr.clone()),
        )),
        "remove_component" => None,
        _ => None,
    }
}

fn existing_feature<'a>(
    input_names: &BTreeSet<String>,
    feature: Option<&'a str>,
) -> Option<&'a str> {
    let feature = feature?;
    input_names.contains(feature).then_some(feature)
}

fn sanitize_factor_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn deterministic_mutation_candidates(
    input_names: &BTreeSet<String>,
    seeds: &[NamedFactorExpr],
    target: AutoFactorV2Target,
) -> Vec<NamedFactorExpr> {
    let mut out = Vec::new();
    let mut frontier = seeds.to_vec();
    for depth in 1..=MAX_DETERMINISTIC_MUTATION_DEPTH {
        if out.len() >= MAX_DETERMINISTIC_MUTATION_CANDIDATES {
            break;
        }
        let next = deterministic_mutation_layer(input_names, &frontier, target, depth);
        for candidate in next.iter().cloned() {
            if out.len() >= MAX_DETERMINISTIC_MUTATION_CANDIDATES {
                break;
            }
            out.push(candidate);
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    out
}

fn deterministic_mutation_layer(
    input_names: &BTreeSet<String>,
    seeds: &[NamedFactorExpr],
    target: AutoFactorV2Target,
    depth: usize,
) -> Vec<NamedFactorExpr> {
    let mut out = Vec::new();
    let settlement_target = matches!(
        target,
        AutoFactorV2Target::SettlementExecutablePnl
            | AutoFactorV2Target::FullDepthSettlementExecutablePnl
            | AutoFactorV2Target::TradeableFullDepthSettlementPnl
    );
    let tradeable_settlement_target =
        matches!(target, AutoFactorV2Target::TradeableFullDepthSettlementPnl);

    for seed in seeds {
        push_mutation(
            &mut out,
            seed,
            depth,
            "squashed",
            FactorExpr::Tanh(Box::new(seed.expr.clone())),
            "clip_or_squash: bound the seed score so one extreme observation cannot dominate search feedback.",
        );

        if input_names.contains("side_spread") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "spread_adjusted",
                safe_div_expr(
                    seed.expr.clone(),
                    FactorExpr::Add(
                        Box::new(input("side_spread")),
                        Box::new(FactorExpr::Const(0.01)),
                    ),
                ),
                "add_spread_penalty: scale the seed by executable Polymarket spread plus epsilon.",
            );
        }

        if input_names.contains("near_strike_score") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "near_strike",
                mul(seed.expr.clone(), input("near_strike_score")),
                "add_near_strike_interaction: emphasize event states where small external moves matter more.",
            );
        }

        if settlement_target && input_names.contains("entry_capacity_score") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "capacity",
                mul(seed.expr.clone(), input("entry_capacity_score")),
                "add_capacity_gate: penalize alpha that cannot be executed at the configured stake.",
            );
        }

        if tradeable_settlement_target && input_names.contains("full_depth_entry_fillable_gate") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "full_depth_entry_gate",
                gate(seed.expr.clone(), input("full_depth_entry_fillable_gate"), 0.5),
                "add_capacity_gate: hard-filter rows that are not full-depth entry-fillable at the configured stake; this is an execution gate, not predictive alpha.",
            );
        }

        if settlement_target && input_names.contains("entry_price_quality_score") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "entry_price_quality",
                mul(seed.expr.clone(), input("entry_price_quality_score")),
                "add_feature_gate: penalize brittle binary-ticket prices before runtime handoff.",
            );
        }

        if !settlement_target && input_names.contains("poly_quote_age") {
            push_mutation(
                &mut out,
                seed,
                depth,
                "pm_lag_gate",
                mul(
                    seed.expr.clone(),
                    FactorExpr::Tanh(Box::new(safe_div_expr(
                        input("poly_quote_age"),
                        FactorExpr::Const(3.0),
                    ))),
                ),
                "add_feature_gate: repricing alpha should strengthen when the Polymarket quote is stale.",
            );
        }
    }
    out
}

fn push_mutation(
    out: &mut Vec<NamedFactorExpr>,
    seed: &NamedFactorExpr,
    depth: usize,
    suffix: &str,
    expr: FactorExpr,
    note: impl Into<String>,
) {
    let prefix = if depth <= 1 { "mut" } else { "mut2" };
    out.push(NamedFactorExpr {
        name: format!("{prefix}_{}_{}", seed.name, suffix),
        expr,
        target: seed.target.clone(),
        notes: vec![format!(
            "Deterministic alpha-search depth-{depth} mutation from `{}`. {}",
            seed.name,
            note.into()
        )],
    });
}

fn settlement_native_generated_candidates(input_names: &BTreeSet<String>) -> Vec<NamedFactorExpr> {
    let mut out = Vec::new();
    let edge_inputs = [
        (
            "full_depth_settlement_edge",
            "Full-depth q minus entry sweep price and fee.",
        ),
        (
            "conservative_settlement_edge",
            "Conservative q minus entry sweep price and fee.",
        ),
    ];
    for (edge_name, note) in edge_inputs {
        if !input_names.contains(edge_name) {
            continue;
        }
        push_generated(
            &mut out,
            format!("auto_settlement_{edge_name}"),
            input(edge_name),
            note,
        );
        if input_names.contains("near_strike_score") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_near_strike"),
                mul(input(edge_name), input("near_strike_score")),
                "Settlement edge gated by near-strike sensitivity.",
            );
        }
        if input_names.contains("entry_capacity_score") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_capacity"),
                mul(input(edge_name), input("entry_capacity_score")),
                "Settlement edge gated by full-depth entry capacity.",
            );
        }
        if input_names.contains("full_depth_entry_fillable_gate") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_full_depth_entry_gate"),
                gate(input(edge_name), input("full_depth_entry_fillable_gate"), 0.5),
                "Settlement edge hard-filtered to rows that are full-depth entry-fillable at the configured stake; this is an execution gate, not predictive alpha.",
            );
        }
        if input_names.contains("entry_price_quality_score") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_entry_price_quality"),
                mul(input(edge_name), input("entry_price_quality_score")),
                "Settlement edge gated by binary entry-price quality.",
            );
        }
        if has_all(input_names, &["near_strike_score", "entry_capacity_score"]) {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_near_strike_x_capacity"),
                mul(
                    mul(input(edge_name), input("near_strike_score")),
                    input("entry_capacity_score"),
                ),
                "Settlement edge gated by both near-strike state and executable capacity.",
            );
        }
        if has_all(
            input_names,
            &[
                "near_strike_score",
                "entry_capacity_score",
                "entry_price_quality_score",
            ],
        ) {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_near_strike_x_capacity_x_entry_price_quality"),
                mul(
                    mul(
                        mul(input(edge_name), input("near_strike_score")),
                        input("entry_capacity_score"),
                    ),
                    input("entry_price_quality_score"),
                ),
                "Settlement edge gated by near-strike state, executable capacity, and entry-price quality.",
            );
        }
        if input_names.contains("side_spread") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_spread_adjusted"),
                safe_div_expr(
                    input(edge_name),
                    FactorExpr::Add(
                        Box::new(input("side_spread")),
                        Box::new(FactorExpr::Const(0.01)),
                    ),
                ),
                "Settlement edge scaled by Polymarket side spread.",
            );
        }
        if input_names.contains("external_pressure") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_external_pressure"),
                mul(input(edge_name), input("external_pressure")),
                "Settlement edge interacted with side-aligned external pressure.",
            );
            if input_names.contains("full_depth_entry_fillable_gate") {
                push_generated(
                    &mut out,
                    format!("auto_settlement_{edge_name}_x_external_pressure_x_full_depth_entry_gate"),
                    gate(
                        mul(input(edge_name), input("external_pressure")),
                        input("full_depth_entry_fillable_gate"),
                        0.5,
                    ),
                    "Settlement edge and external pressure hard-filtered to full-depth entry-fillable rows; this tests predictive edge inside executable capacity.",
                );
            }
        }
        if input_names.contains("iv_change_1m") {
            push_generated(
                &mut out,
                format!("auto_settlement_{edge_name}_x_iv_change"),
                mul(input(edge_name), input("iv_change_1m")),
                "Settlement edge interacted with short implied-volatility change.",
            );
        }
    }
    out
}

fn push_generated(
    out: &mut Vec<NamedFactorExpr>,
    name: String,
    expr: FactorExpr,
    note: impl Into<String>,
) {
    out.push(NamedFactorExpr {
        name,
        expr,
        target: Some("full_depth_settlement_executable_pnl".to_string()),
        notes: vec![format!(
            "Auto-generated settlement-native formula. {}",
            note.into()
        )],
    });
}

fn insert_column(
    columns: &mut BTreeMap<String, Vec<f64>>,
    name: &str,
    rows: &[FactorObservationV2],
    accessor: impl Fn(&FactorObservationV2) -> f64,
) {
    columns.insert(name.to_string(), rows.iter().map(accessor).collect());
}

fn binary_eval(
    lhs: &FactorExpr,
    rhs: &FactorExpr,
    matrix: &AutoFactorMatrix,
    op: fn(f64, f64) -> f64,
) -> Result<Vec<f64>, AutoFactorError> {
    let lhs = lhs.evaluate(matrix)?;
    let rhs = rhs.evaluate(matrix)?;
    Ok(lhs
        .iter()
        .zip(rhs.iter())
        .map(|(left, right)| {
            if left.is_finite() && right.is_finite() {
                op(*left, *right)
            } else {
                f64::NAN
            }
        })
        .collect())
}

fn binary_eval_raw(
    lhs: &FactorExpr,
    rhs: &FactorExpr,
    matrix: &AutoFactorMatrix,
    op: fn(f64, f64) -> f64,
) -> Result<Vec<f64>, AutoFactorError> {
    let lhs = lhs.evaluate(matrix)?;
    let rhs = rhs.evaluate(matrix)?;
    Ok(lhs
        .iter()
        .zip(rhs.iter())
        .map(|(left, right)| op(*left, *right))
        .collect())
}

fn unary_eval(
    expr: &FactorExpr,
    matrix: &AutoFactorMatrix,
    op: impl Fn(f64) -> f64,
) -> Result<Vec<f64>, AutoFactorError> {
    Ok(expr
        .evaluate(matrix)?
        .iter()
        .map(|value| {
            if value.is_finite() {
                op(*value)
            } else {
                f64::NAN
            }
        })
        .collect())
}

fn gate_eval(
    expr: &FactorExpr,
    gate: &FactorExpr,
    min: f64,
    matrix: &AutoFactorMatrix,
) -> Result<Vec<f64>, AutoFactorError> {
    let values = expr.evaluate(matrix)?;
    let gates = gate.evaluate(matrix)?;
    Ok(values
        .iter()
        .zip(gates.iter())
        .map(|(value, gate)| {
            if value.is_finite() && gate.is_finite() && *gate >= min {
                *value
            } else {
                f64::NAN
            }
        })
        .collect())
}

fn delta_series(values: &[f64], lag: usize) -> Vec<f64> {
    let lag = lag.max(1);
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            if idx < lag || !value.is_finite() || !values[idx - lag].is_finite() {
                f64::NAN
            } else {
                *value - values[idx - lag]
            }
        })
        .collect()
}

fn rolling_mean(values: &[f64], window: usize) -> Vec<f64> {
    rolling_stat(values, window, |slice| finite_mean(slice.iter().copied()))
}

fn rolling_std(values: &[f64], window: usize) -> Vec<f64> {
    rolling_stat(values, window, finite_std)
}

fn zscore(values: &[f64], window: usize) -> Vec<f64> {
    let means = rolling_mean(values, window);
    let stds = rolling_std(values, window);
    values
        .iter()
        .zip(means.iter().zip(stds.iter()))
        .map(|(value, (avg, std))| {
            if value.is_finite() && avg.is_finite() && std.is_finite() && *std > EPS {
                (*value - *avg) / *std
            } else {
                0.0
            }
        })
        .collect()
}

fn rolling_stat(values: &[f64], window: usize, stat: fn(&[f64]) -> f64) -> Vec<f64> {
    let window = window.max(1);
    (0..values.len())
        .map(|idx| {
            let start = idx.saturating_add(1).saturating_sub(window);
            stat(&values[start..=idx])
        })
        .collect()
}

fn build_buckets(
    scored: &[(usize, f64, f64)],
    bucket_count: usize,
    full_depth_entry_fillable: Option<&[f64]>,
) -> Vec<BucketSummary> {
    let mut sorted = scored.to_vec();
    sorted.sort_by(|lhs, rhs| lhs.1.total_cmp(&rhs.1));
    let bucket_count = bucket_count.clamp(2, sorted.len().max(2));
    (0..bucket_count)
        .filter_map(|bucket_idx| {
            let start = bucket_idx * sorted.len() / bucket_count;
            let end = ((bucket_idx + 1) * sorted.len() / bucket_count).min(sorted.len());
            (start < end).then(|| {
                let slice = &sorted[start..end];
                summarize_bucket_slice(slice, full_depth_entry_fillable)
            })
        })
        .collect()
}

fn summarize_bucket_slice(
    slice: &[(usize, f64, f64)],
    full_depth_entry_fillable: Option<&[f64]>,
) -> BucketSummary {
    BucketSummary {
        n: slice.len(),
        avg_label: finite_mean(slice.iter().map(|(_, _, label)| *label)),
        positive_label_rate: ratio(
            slice.iter().filter(|(_, _, label)| *label > 0.0).count(),
            slice.len(),
        ),
        full_depth_entry_fill_rate: full_depth_entry_fillable
            .map(|values| {
                ratio(
                    slice
                        .iter()
                        .filter(|(idx, _, _)| values.get(*idx).copied().unwrap_or(0.0) > 0.5)
                        .count(),
                    slice.len(),
                )
            })
            .unwrap_or(f64::NAN),
        indexes: slice.iter().map(|(idx, _, _)| *idx).collect(),
    }
}

fn event_level_bucket_summary(
    bucket: Option<&BucketSummary>,
    scored: &[(usize, f64, f64)],
    event_ids: &[String],
    full_depth_entry_fillable: Option<&[f64]>,
) -> Option<BucketSummary> {
    let bucket = bucket?;
    if event_ids.is_empty() {
        return Some(bucket.clone());
    }
    let by_index = scored
        .iter()
        .map(|(idx, score, label)| (*idx, (*score, *label)))
        .collect::<BTreeMap<_, _>>();
    let mut by_event: BTreeMap<&str, (usize, f64, f64)> = BTreeMap::new();
    for idx in &bucket.indexes {
        let Some(event_id) = event_ids.get(*idx) else {
            continue;
        };
        let Some((score, label)) = by_index.get(idx).copied() else {
            continue;
        };
        by_event
            .entry(event_id.as_str())
            .and_modify(|current| {
                if score > current.1 {
                    *current = (*idx, score, label);
                }
            })
            .or_insert((*idx, score, label));
    }
    let selected = by_event.into_values().collect::<Vec<_>>();
    if selected.is_empty() {
        None
    } else {
        Some(summarize_bucket_slice(&selected, full_depth_entry_fillable))
    }
}

fn top_bucket_event_decision_stats(
    bucket: Option<&BucketSummary>,
    event_ids: &[String],
) -> (usize, usize) {
    let Some(bucket) = bucket else {
        return (0, 0);
    };
    if event_ids.is_empty() {
        return (0, 0);
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for idx in &bucket.indexes {
        if let Some(event_id) = event_ids.get(*idx) {
            *counts.entry(event_id.as_str()).or_default() += 1;
        }
    }
    (counts.len(), counts.values().copied().max().unwrap_or(0))
}

fn autofactor_decision(
    n: usize,
    complexity: usize,
    spearman_ic_value: f64,
    window_count: usize,
    factor_icir: f64,
    positive_window_ratio: f64,
    top_bucket_avg_label: f64,
    top_bucket_full_depth_entry_fill_rate: f64,
    monotonicity_score: f64,
    options: &AutoFactorOptions,
) -> (AutoFactorDecision, String) {
    if n < options.min_observations {
        return (
            AutoFactorDecision::Reject,
            "too_few_observations".to_string(),
        );
    }
    if complexity > options.max_complexity {
        return (AutoFactorDecision::Reject, "too_complex".to_string());
    }
    if !spearman_ic_value.is_finite() || spearman_ic_value <= options.min_spearman_ic {
        return (
            AutoFactorDecision::Reject,
            "nonpositive_rank_ic".to_string(),
        );
    }
    if window_count == 0 {
        return (
            AutoFactorDecision::Watchlist,
            "no_powered_windows".to_string(),
        );
    }
    if !factor_icir.is_finite() || factor_icir < options.min_icir {
        return (AutoFactorDecision::Watchlist, "low_icir".to_string());
    }
    if positive_window_ratio < options.min_positive_window_ratio {
        return (
            AutoFactorDecision::Watchlist,
            "unstable_positive_windows".to_string(),
        );
    }
    if !top_bucket_avg_label.is_finite() || top_bucket_avg_label <= options.min_top_bucket_avg_label
    {
        return (
            AutoFactorDecision::Watchlist,
            "nonpositive_top_bucket_label".to_string(),
        );
    }
    if options.min_top_bucket_full_depth_entry_fill_rate > 0.0
        && (!top_bucket_full_depth_entry_fill_rate.is_finite()
            || top_bucket_full_depth_entry_fill_rate
                < options.min_top_bucket_full_depth_entry_fill_rate)
    {
        return (
            AutoFactorDecision::Watchlist,
            "low_top_bucket_fillability".to_string(),
        );
    }
    if monotonicity_score < options.min_monotonicity_score {
        return (
            AutoFactorDecision::Watchlist,
            "nonmonotonic_buckets".to_string(),
        );
    }
    (AutoFactorDecision::Candidate, "passed".to_string())
}

fn monotonicity_score(values: &[f64]) -> f64 {
    let pairs = values
        .windows(2)
        .filter(|pair| pair[0].is_finite() && pair[1].is_finite())
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return f64::NAN;
    }
    ratio(
        pairs.iter().filter(|pair| pair[1] + EPS >= pair[0]).count(),
        pairs.len(),
    )
}

fn decision_rank(decision: AutoFactorDecision) -> usize {
    match decision {
        AutoFactorDecision::Candidate => 3,
        AutoFactorDecision::Watchlist => 2,
        AutoFactorDecision::Reject => 1,
    }
}

fn finite_mean(values: impl Iterator<Item = f64>) -> f64 {
    let values = values.filter(|value| value.is_finite()).collect::<Vec<_>>();
    if values.is_empty() {
        f64::NAN
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn finite_std(values: &[f64]) -> f64 {
    let values = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.len() < 2 {
        return f64::NAN;
    }
    let avg = values.iter().sum::<f64>() / values.len() as f64;
    let var = values
        .iter()
        .map(|value| (value - avg).powi(2))
        .sum::<f64>()
        / values.len() as f64;
    var.sqrt()
}

fn icir(values: &[f64]) -> f64 {
    let avg = finite_mean(values.iter().copied());
    let std = finite_std(values);
    if !avg.is_finite() || !std.is_finite() {
        f64::NAN
    } else if std <= EPS && avg.abs() > EPS {
        avg.signum() * 1_000_000.0
    } else if std > EPS {
        avg / std
    } else {
        f64::NAN
    }
}

fn ratio(num: usize, denom: usize) -> f64 {
    if denom == 0 {
        f64::NAN
    } else {
        num as f64 / denom as f64
    }
}

fn safe_div_value(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_finite() && rhs.is_finite() && rhs.abs() > EPS {
        lhs / rhs
    } else {
        0.0
    }
}

fn log1p_abs(value: f64) -> f64 {
    value.signum() * value.abs().ln_1p()
}

fn positive_or_nan(value: f64) -> f64 {
    if value.is_finite() && value > EPS {
        value
    } else {
        f64::NAN
    }
}

fn first_finite(primary: f64, fallback: f64) -> f64 {
    if primary.is_finite() {
        primary
    } else {
        fallback
    }
}

fn inverse_liquidity(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        1.0 / (1.0 + value)
    } else {
        f64::NAN
    }
}

fn near_strike_score(row: &FactorObservationV2) -> f64 {
    if row.side_distance_over_sigma.is_finite() {
        (1.0 - row.side_distance_over_sigma.abs()).clamp(0.0, 1.0)
    } else {
        f64::NAN
    }
}

fn entry_price_quality_score(entry_price: f64) -> f64 {
    if !valid_pm_price(entry_price) {
        return f64::NAN;
    }
    let low_ticket_gate = ((entry_price - 0.08) / 0.12).clamp(0.0, 1.0);
    let expensive_ticket_gate = ((0.85 - entry_price) / 0.20).clamp(0.0, 1.0);
    low_ticket_gate.min(expensive_ticket_gate)
}

fn time_remaining_bucket(secs: i64) -> &'static str {
    match secs {
        181..=300 => "180_300s",
        91..=180 => "90_180s",
        31..=90 => "30_90s",
        11..=30 => "10_30s",
        _ => "0_10s",
    }
}

fn distance_bucket(abs_distance_sigma: f64) -> &'static str {
    if !abs_distance_sigma.is_finite() {
        "distance_unknown"
    } else if abs_distance_sigma < 0.5 {
        "distance_0_0p5"
    } else if abs_distance_sigma < 1.0 {
        "distance_0p5_1"
    } else if abs_distance_sigma < 2.0 {
        "distance_1_2"
    } else {
        "distance_2_plus"
    }
}

fn input(name: &str) -> FactorExpr {
    FactorExpr::Input(name.to_string())
}

fn mul(lhs: FactorExpr, rhs: FactorExpr) -> FactorExpr {
    FactorExpr::Mul(Box::new(lhs), Box::new(rhs))
}

fn gate(expr: FactorExpr, gate: FactorExpr, min: f64) -> FactorExpr {
    FactorExpr::Gate {
        expr: Box::new(expr),
        gate: Box::new(gate),
        min,
    }
}

fn safe_div_expr(lhs: FactorExpr, rhs: FactorExpr) -> FactorExpr {
    FactorExpr::SafeDiv(Box::new(lhs), Box::new(rhs))
}

fn has_all(input_names: &BTreeSet<String>, required: &[&str]) -> bool {
    required.iter().all(|name| input_names.contains(*name))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoFactorError {
    MissingInput(String),
    LengthMismatch {
        name: String,
        expected: usize,
        actual: usize,
    },
    LabelLengthMismatch {
        expected: usize,
        actual: usize,
    },
    WindowLengthMismatch {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for AutoFactorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AutoFactorError::MissingInput(name) => write!(f, "missing autofactor input: {name}"),
            AutoFactorError::LengthMismatch {
                name,
                expected,
                actual,
            } => write!(
                f,
                "autofactor input {name} length mismatch: expected {expected}, got {actual}"
            ),
            AutoFactorError::LabelLengthMismatch { expected, actual } => write!(
                f,
                "autofactor label length mismatch: expected {expected}, got {actual}"
            ),
            AutoFactorError::WindowLengthMismatch { expected, actual } => write!(
                f,
                "autofactor window length mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl Error for AutoFactorError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factors_v2::ReviewSide;
    use chrono::TimeZone;
    use ploy_operator_contracts::Regime;

    fn synthetic_matrix(rows: usize) -> (AutoFactorMatrix, Vec<f64>, Vec<String>) {
        let mut columns = BTreeMap::new();
        let mut ofi = Vec::with_capacity(rows);
        let mut depth = Vec::with_capacity(rows);
        let mut near = Vec::with_capacity(rows);
        let mut labels = Vec::with_capacity(rows);
        let mut windows = Vec::with_capacity(rows);
        for idx in 0..rows {
            let local = (idx % 6) as f64;
            let window = idx / 6;
            let signal = local + window as f64 * 0.05;
            ofi.push(signal + 1.0);
            depth.push(1.0);
            near.push(1.0);
            labels.push(signal * 0.2 - 0.4);
            windows.push(format!("window-{window}"));
        }
        columns.insert("ofi_l5".to_string(), ofi);
        columns.insert("depth_top5".to_string(), depth);
        columns.insert("near_strike_score".to_string(), near);
        (
            AutoFactorMatrix::new(columns).expect("matrix"),
            labels,
            windows,
        )
    }

    fn synthetic_v2_row(idx: usize) -> FactorObservationV2 {
        let score = (idx % 8) as f64;
        FactorObservationV2 {
            event_id: format!("event-{}", idx / 2),
            symbol: "BTCUSDT".to_string(),
            tick_ts: chrono::Utc.with_ymd_and_hms(2026, 5, 3, 0, 0, 0).unwrap()
                + chrono::Duration::days((idx / 20) as i64)
                + chrono::Duration::seconds(idx as i64),
            time_remaining_secs: 120,
            regime: Regime::Middle,
            side: ReviewSide::Up,
            side_model_prob: 0.5,
            side_fair_prob: (0.50 + score * 0.01).clamp(0.01, 0.99),
            side_model_edge: score * 0.01,
            side_distance_over_sigma: 0.25,
            abs_distance_to_beat: 10.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 1.0,
            vol_gap: score + 1.0,
            obi_10: score + 1.0,
            depth_imbalance: 0.0,
            depth_acceleration: 0.0,
            microprice_offset_bps: 0.0,
            cex_spread_bps: 1.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            obi_delta_10s_side: 0.0,
            obi_delta_30s_side: 0.0,
            obi_persistence_30s_side: 0.0,
            obi_flip_count_60s: 0.0,
            depth_imbalance_delta_30s_side: 0.0,
            microprice_momentum_30s_side: 0.0,
            trade_imbalance_delta_10s_side: 0.0,
            trade_imbalance_delta_30s_side: 0.0,
            cex_bar_return_30s: score * 0.001,
            cex_bar_return_60s: score * 0.001,
            cex_bar_volume_ratio_30s: 1.0,
            cex_bar_volume_trend_3: 1.0,
            cex_signed_volume_ratio_30s: score,
            cex_consecutive_bar_side: score,
            cex_breakout_volume_side: score,
            cex_continuation_score_side: score,
            cex_continuation_edge_gate: score * 0.01,
            cex_continuation_liquidity_gate: score,
            entry_ask: 0.50,
            exit_bid: 0.48,
            entry_ask_size: 100.0,
            exit_bid_size: 100.0,
            opposite_ask: 0.52,
            opposite_bid: 0.50,
            up_down_ask_sum: 1.02,
            pm_spread_bps: 400.0,
            pm_lag_secs: 3.0,
            entry_ask_change_10s: 0.0,
            entry_ask_change_30s: 0.0,
            exit_bid_change_30s: 0.0,
            pm_spread_change_30s: 0.0,
            entry_size_change_30s: 0.0,
            up_down_ask_sum_change_30s: 0.0,
            pm_reprice_speed_30s: 0.0,
            pm_quote_stability_30s: 1.0,
            deribit_mark_iv: 0.5,
            deribit_bid_iv: 0.49,
            deribit_ask_iv: 0.51,
            deribit_iv_spread: 0.02,
            deribit_iv_lag_secs: 1.0,
            deribit_iv_horizon: 0.5,
            deribit_iv_gap_horizon: 0.0,
            deribit_iv_change_30s: score * 0.001,
            deribit_iv_change_60s: score * 0.002,
            deribit_underlying_basis_bps: 0.0,
            deribit_delta: 0.0,
            deribit_gamma: 0.0,
            deribit_vega: 0.0,
            deribit_theta: 0.0,
            stake_usd: 15.0,
            entry_shares: 30.0,
            entry_fee_usd: 0.0,
            entry_capacity_ratio: 3.0,
            exit_capacity_ratio: 3.0,
            entry_liquidity_usd: 50.0 + score,
            exit_liquidity_usd: 50.0 + score,
            liquidity_shortfall_usd: 0.0,
            slippage_to_fill_15u_bps: 0.0,
            entry_sweep_avg_price_15u: 0.50,
            exit_sweep_avg_price_15u: 0.48,
            entry_sweep_shares_15u: 30.0,
            exit_sweep_shares_15u: 30.0,
            entry_sweep_levels_15u: 1.0,
            exit_sweep_levels_15u: 1.0,
            entry_sweep_slippage_bps: 0.0,
            exit_sweep_slippage_bps: 0.0,
            conservative_entry_sweep_avg_price_15u: 0.50,
            conservative_entry_sweep_shares_15u: 30.0,
            conservative_entry_sweep_levels_15u: 1.0,
            conservative_entry_sweep_slippage_bps: 0.0,
            roundtrip_cost_usd: 0.60,
            roundtrip_pnl_now_15u: Some(-0.60),
            roundtrip_pnl_now_full_depth_15u: Some(-0.60),
            portfolio_stake_usd: 15.0,
            portfolio_event_exposure_usd: 15.0,
            same_event_observation_count: 1.0,
            same_event_side_observation_count: 1.0,
            side_is_up: 1.0,
            label_settlement_win: Some(1.0),
            label_executable_pnl_15u: Some(score),
            label_full_depth_executable_pnl_15u: Some(score),
            label_conservative_executable_pnl_15u: Some(score),
            label_executable_fillable: true,
            label_exit_fillable: true,
            label_full_depth_entry_fillable: true,
            label_full_depth_exit_fillable: true,
            label_conservative_entry_fillable: true,
            label_future_exit_bid_change_5s: Some(score * 0.001),
            label_future_exit_bid_change_10s: Some(score * 0.002),
            label_future_exit_bid_change_30s: Some(score * 0.003),
            label_future_exit_bid_change_60s: Some(score * 0.004),
            label_future_exit_pnl_5s: Some(score * 0.05),
            label_future_exit_pnl_10s: Some(score * 0.10),
            label_future_exit_pnl_30s: Some(score * 0.20),
            label_future_exit_pnl_60s: Some(score * 0.30),
            label_future_exit_full_depth_pnl_5s: Some(score * 0.04),
            label_future_exit_full_depth_pnl_10s: Some(score * 0.08),
            label_future_exit_full_depth_pnl_30s: Some(score * 0.16),
            label_future_exit_full_depth_pnl_60s: Some(score * 0.24),
            label_future_exit_full_depth_value_5s: Some(15.0 + score * 0.04),
            label_future_exit_full_depth_value_10s: Some(15.0 + score * 0.08),
            label_future_exit_full_depth_value_30s: Some(15.0 + score * 0.16),
            label_future_exit_full_depth_value_60s: Some(15.0 + score * 0.24),
            label_future_exit_fillable_5s: Some(1.0),
            label_future_exit_fillable_10s: Some(1.0),
            label_future_exit_fillable_30s: Some(1.0),
            label_future_exit_fillable_60s: Some(1.0),
            label_future_exit_full_depth_fillable_5s: Some(1.0),
            label_future_exit_full_depth_fillable_10s: Some(1.0),
            label_future_exit_full_depth_fillable_30s: Some(1.0),
            label_future_exit_full_depth_fillable_60s: Some(1.0),
        }
    }

    #[test]
    fn evaluates_safe_expression_series() {
        let mut columns = BTreeMap::new();
        columns.insert("a".to_string(), vec![2.0, 4.0, f64::NAN]);
        columns.insert("b".to_string(), vec![1.0, 0.0, 2.0]);
        let matrix = AutoFactorMatrix::new(columns).expect("matrix");
        let expr = FactorExpr::Log1pAbs(Box::new(FactorExpr::SafeDiv(
            Box::new(input("a")),
            Box::new(input("b")),
        )));
        let values = expr.evaluate(&matrix).expect("values");
        assert!((values[0] - 2.0_f64.ln_1p()).abs() < 1e-9);
        assert_eq!(values[1], 0.0);
        assert!(values[2].is_finite());
    }

    #[test]
    fn evaluates_gate_expression_as_hard_sample_filter() {
        let mut columns = BTreeMap::new();
        columns.insert("score".to_string(), vec![1.0, 2.0, 3.0]);
        columns.insert(
            "full_depth_entry_fillable_gate".to_string(),
            vec![1.0, 0.0, 1.0],
        );
        let matrix = AutoFactorMatrix::new(columns).expect("matrix");
        let values = gate(input("score"), input("full_depth_entry_fillable_gate"), 0.5)
            .evaluate(&matrix)
            .expect("values");

        assert_eq!(values[0], 1.0);
        assert!(values[1].is_nan());
        assert_eq!(values[2], 3.0);
    }

    #[test]
    fn factor_expr_hash_is_stable_and_sensitive_to_ast_changes() {
        let expr_a = FactorExpr::SafeDiv(Box::new(input("ofi_l5")), Box::new(input("depth_top5")));
        let expr_b = FactorExpr::SafeDiv(Box::new(input("ofi_l5")), Box::new(input("depth_top5")));
        let expr_c =
            FactorExpr::SafeDiv(Box::new(input("ofi_l5")), Box::new(FactorExpr::Const(2.0)));

        assert_eq!(
            factor_expr_hash(&expr_a).expect("hash a"),
            factor_expr_hash(&expr_b).expect("hash b")
        );
        assert_ne!(
            factor_expr_hash(&expr_a).expect("hash a"),
            factor_expr_hash(&expr_c).expect("hash c")
        );
        assert_eq!(
            expr_a.input_names(),
            BTreeSet::from(["ofi_l5".to_string(), "depth_top5".to_string()])
        );
    }

    #[test]
    fn evaluates_domain_candidate_with_icir_gate() {
        let (matrix, labels, windows) = synthetic_matrix(24);
        let candidates = domain_seed_candidates(&matrix.input_names());
        assert!(candidates
            .iter()
            .any(|item| item.name == "ofi_l5_depth_norm"));
        let options = AutoFactorOptions {
            min_observations: 20,
            min_window_observations: 6,
            min_icir: 0.5,
            ..Default::default()
        };

        let symbols = Vec::new();
        let reports = mine_autofactors(&candidates, &matrix, &labels, &windows, &symbols, &options)
            .expect("reports");
        let report = reports
            .iter()
            .find(|item| item.name == "ofi_l5_depth_norm")
            .expect("ofi report");
        assert_eq!(report.decision, AutoFactorDecision::Candidate);
        assert!(report.spearman_ic > 0.95);
        assert_eq!(report.window_count, 4);
        assert_eq!(report.positive_window_ratio, 1.0);
        assert!(report.top_bucket_avg_label > report.bottom_bucket_avg_label);
        assert!(report.monotonicity_score >= 0.99);
    }

    #[test]
    fn rejects_over_complex_expression() {
        let (matrix, labels, windows) = synthetic_matrix(24);
        let factor = NamedFactorExpr::new(
            "too_complex",
            mul(
                mul(
                    safe_div_expr(input("ofi_l5"), input("depth_top5")),
                    input("near_strike_score"),
                ),
                FactorExpr::Tanh(Box::new(input("ofi_l5"))),
            ),
        );
        let options = AutoFactorOptions {
            min_observations: 20,
            min_window_observations: 6,
            max_complexity: 3,
            ..Default::default()
        };
        let symbols = Vec::new();
        let report =
            evaluate_named_factor(&factor, &matrix, &labels, &windows, &symbols, &[], &options)
                .expect("report");
        assert_eq!(report.decision, AutoFactorDecision::Reject);
        assert_eq!(report.reason, "too_complex");
    }

    #[test]
    fn mines_domain_candidates_from_v2_repricing_rows() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };
        let reports =
            mine_domain_autofactors_from_v2(&rows, AutoFactorV2Target::RepricePnl10s, &options)
                .expect("reports");
        let repricing_gap = reports
            .iter()
            .find(|report| report.name == "repricing_gap_side_10s")
            .expect("repricing gap report");
        assert_eq!(repricing_gap.decision, AutoFactorDecision::Candidate);
        assert!(repricing_gap.spearman_ic > 0.95);
        assert!(reports
            .iter()
            .any(|report| report.name == "poly_lag_pressure"));
        assert!(reports
            .iter()
            .any(|report| report.name == "amplitude_weighted_momentum_30s_sigma"));
        assert!(reports
            .iter()
            .any(|report| report.name == "amplitude_weighted_momentum_30s_vol_gap"));
        assert!(reports
            .iter()
            .any(|report| report.name == "mut_spread_adjusted_external_move_pm_lag_gate"));
        assert!(reports
            .iter()
            .any(|report| report.name
                == "mut2_mut_spread_adjusted_external_move_pm_lag_gate_squashed"));
    }

    #[test]
    fn mines_domain_candidates_from_v2_settlement_rows() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };
        let reports = mine_domain_autofactors_from_v2(
            &rows,
            AutoFactorV2Target::SettlementExecutablePnl,
            &options,
        )
        .expect("reports");
        let fair_edge = reports
            .iter()
            .find(|report| report.name == "settlement_fair_edge")
            .expect("settlement fair-edge report");

        assert_eq!(fair_edge.decision, AutoFactorDecision::Candidate);
        assert_eq!(
            fair_edge.target.as_deref(),
            Some("settlement_executable_pnl")
        );
        assert!(fair_edge.spearman_ic > 0.95);
    }

    #[test]
    fn mines_generated_settlement_native_candidates_from_v2_rows() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };

        let reports = mine_domain_autofactors_from_v2(
            &rows,
            AutoFactorV2Target::FullDepthSettlementExecutablePnl,
            &options,
        )
        .expect("reports");

        let full_depth_edge = reports
            .iter()
            .find(|report| report.name == "auto_settlement_full_depth_settlement_edge")
            .expect("generated full-depth settlement edge report");
        assert_eq!(full_depth_edge.decision, AutoFactorDecision::Candidate);
        assert_eq!(
            full_depth_edge.target.as_deref(),
            Some("full_depth_settlement_executable_pnl")
        );
        assert!(full_depth_edge.spearman_ic > 0.95);
        assert!(reports.iter().any(|report| {
            report.name == "auto_settlement_full_depth_settlement_edge_x_near_strike_x_capacity"
        }));
        assert!(reports.iter().any(|report| {
            report.name == "auto_settlement_full_depth_settlement_edge_x_entry_price_quality"
        }));
        assert!(reports.iter().any(|report| {
            report.name
                == "auto_settlement_full_depth_settlement_edge_x_near_strike_x_capacity_x_entry_price_quality"
        }));
        assert!(
            reports
                .iter()
                .any(|report| report.name
                    == "mut_auto_settlement_full_depth_settlement_edge_capacity")
        );
        assert!(reports
            .iter()
            .any(|report| report.name.starts_with("mut2_")));
    }

    #[test]
    fn tradeable_full_depth_target_penalizes_unfillable_settled_rows() {
        let mut rows = (0..4).map(synthetic_v2_row).collect::<Vec<_>>();
        rows[0].label_full_depth_entry_fillable = false;
        rows[0].label_full_depth_executable_pnl_15u = None;

        let labels =
            autofactor_labels_from_v2(&rows, AutoFactorV2Target::TradeableFullDepthSettlementPnl);
        let legacy_labels =
            autofactor_labels_from_v2(&rows, AutoFactorV2Target::FullDepthSettlementExecutablePnl);

        assert_eq!(labels[0], 0.0);
        assert!(legacy_labels[0].is_nan());
        assert!(labels[1].is_finite());
    }

    #[test]
    fn autofactor_gate_marks_low_top_bucket_fillability_watchlist() {
        let rows = 100;
        let mut columns = BTreeMap::new();
        columns.insert(
            "score".to_string(),
            (0..rows).map(|idx| idx as f64).collect::<Vec<_>>(),
        );
        columns.insert(
            "full_depth_entry_fillable_gate".to_string(),
            (0..rows)
                .map(|idx| if idx < 50 { 1.0 } else { 0.0 })
                .collect::<Vec<_>>(),
        );
        let matrix = AutoFactorMatrix::new(columns).expect("matrix");
        let labels = (0..rows).map(|idx| idx as f64).collect::<Vec<_>>();
        let windows = (0..rows)
            .map(|idx| format!("w{}", idx / 20))
            .collect::<Vec<_>>();
        let symbols = vec!["BTCUSDT".to_string(); rows];
        let options = AutoFactorOptions {
            min_observations: 50,
            min_window_observations: 20,
            min_icir: 0.0,
            min_top_bucket_full_depth_entry_fill_rate: 0.30,
            ..Default::default()
        };
        let reports = mine_autofactors(
            &[NamedFactorExpr {
                name: "score".to_string(),
                target: Some("tradeable_full_depth_settlement_pnl".to_string()),
                expr: input("score"),
                notes: vec![],
            }],
            &matrix,
            &labels,
            &windows,
            &symbols,
            &options,
        )
        .expect("reports");

        assert_eq!(reports[0].decision, AutoFactorDecision::Watchlist);
        assert_eq!(reports[0].reason, "low_top_bucket_fillability");
        assert_eq!(reports[0].top_bucket_full_depth_entry_fill_rate, 0.0);
    }

    #[test]
    fn autofactor_top_bucket_metrics_are_event_level_when_event_ids_are_available() {
        let rows = 100;
        let mut columns = BTreeMap::new();
        columns.insert(
            "score".to_string(),
            (0..rows).map(|idx| idx as f64).collect::<Vec<_>>(),
        );
        columns.insert(
            "full_depth_entry_fillable_gate".to_string(),
            vec![1.0; rows],
        );
        let matrix = AutoFactorMatrix::new(columns).expect("matrix");
        let labels = (0..rows).map(|idx| idx as f64).collect::<Vec<_>>();
        let windows = (0..rows)
            .map(|idx| format!("w{}", idx / 20))
            .collect::<Vec<_>>();
        let symbols = vec!["BTCUSDT".to_string(); rows];
        let event_ids = (0..rows)
            .map(|idx| format!("event-{}", idx / 2))
            .collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_window_observations: 10,
            min_icir: 0.0,
            ..Default::default()
        };

        let report = evaluate_named_factor(
            &NamedFactorExpr {
                name: "score".to_string(),
                target: Some("tradeable_full_depth_settlement_pnl".to_string()),
                expr: input("score"),
                notes: vec![],
            },
            &matrix,
            &labels,
            &windows,
            &symbols,
            &event_ids,
            &options,
        )
        .expect("report");

        assert_eq!(report.top_bucket_n, 10);
        assert_eq!(report.top_bucket_unique_event_count, 10);
        assert_eq!(report.top_bucket_max_event_decisions, 1);
        assert_eq!(report.top_bucket_full_depth_entry_fill_rate, 1.0);
        assert!((report.top_bucket_avg_label - 90.0).abs() < 1e-9);
    }

    #[test]
    fn tradeable_settlement_mutations_include_hard_full_depth_entry_gate() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let matrix = autofactor_matrix_from_v2(&rows).expect("matrix");
        let seeds = vec![NamedFactorExpr {
            name: "predictive_seed".to_string(),
            target: Some("tradeable_full_depth_settlement_pnl".to_string()),
            expr: input("external_move_since_poly_update"),
            notes: vec![],
        }];

        let mutations = deterministic_mutation_layer(
            &matrix.input_names(),
            &seeds,
            AutoFactorV2Target::TradeableFullDepthSettlementPnl,
            1,
        );
        let hard_gate = mutations
            .iter()
            .find(|candidate| candidate.name == "mut_predictive_seed_full_depth_entry_gate")
            .expect("hard full-depth entry gate mutation");

        assert!(matches!(hard_gate.expr, FactorExpr::Gate { .. }));
        assert!(
            hard_gate.notes[0].contains("execution gate"),
            "hard fillability should be documented as execution gating"
        );
    }

    #[test]
    fn typed_llm_prior_mutations_compile_into_factor_expr_candidates() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };
        let prior = LlmPriorSpec {
            mutations: vec![LlmMutationSpec {
                base_factor: "auto_settlement_full_depth_settlement_edge".to_string(),
                mutation_type: "add_feature_gate".to_string(),
                name: Some("llm_full_depth_edge_near_strike".to_string()),
                feature: Some("near_strike_score".to_string()),
                denominator_feature: None,
                constant: None,
                lo: None,
                hi: None,
                window: None,
            }],
        };

        let reports = mine_domain_autofactors_from_v2_with_guidance(
            &rows,
            AutoFactorV2Target::FullDepthSettlementExecutablePnl,
            &options,
            &[],
            Some(&prior),
        )
        .expect("reports");

        let report = reports
            .iter()
            .find(|report| report.name == "llm_full_depth_edge_near_strike")
            .expect("LLM-prior compiled candidate");
        assert_eq!(
            report.target.as_deref(),
            Some("full_depth_settlement_executable_pnl")
        );
        assert!(matches!(report.expr, FactorExpr::Mul(_, _)));
    }

    #[test]
    fn keeps_settlement_native_generated_candidates_out_of_repricing_targets() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };

        let reports = mine_domain_autofactors_from_v2(
            &rows,
            AutoFactorV2Target::FullDepthRepricePnl10s,
            &options,
        )
        .expect("reports");

        assert!(reports
            .iter()
            .all(|report| !report.name.starts_with("auto_settlement_")));
    }

    #[test]
    fn mines_domain_candidates_from_v2_uses_requested_target_metadata() {
        let rows = (0..80).map(synthetic_v2_row).collect::<Vec<_>>();
        let options = AutoFactorOptions {
            min_observations: 40,
            min_window_observations: 10,
            min_icir: 0.1,
            ..Default::default()
        };

        let reports =
            mine_domain_autofactors_from_v2(&rows, AutoFactorV2Target::RepricePnl30s, &options)
                .expect("reports");

        assert!(!reports.is_empty());
        assert!(reports
            .iter()
            .all(|report| report.target.as_deref() == Some("reprice_pnl_30s")));

        let formatted = format_autofactor_reports(&reports, reports.len());
        assert!(formatted.contains("reprice_pnl_30s"));
        assert!(!formatted.contains("reprice_pnl_10s"));
    }
}
