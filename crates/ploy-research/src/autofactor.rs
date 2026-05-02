use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use chrono::Datelike;
use serde::{Deserialize, Serialize};

use crate::factors::{pearson_ic, spearman_ic};
use crate::factors_v2::FactorObservationV2;

const EPS: f64 = 1e-9;

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
}

impl FactorExpr {
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedFactorExpr {
    pub name: String,
    pub expr: FactorExpr,
    pub target: Option<String>,
    pub notes: Vec<String>,
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
        self.columns.keys().cloned().collect()
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
    pub bucket_avg_labels: Vec<f64>,
    pub bottom_bucket_n: usize,
    pub bottom_bucket_avg_label: f64,
    pub top_bucket_n: usize,
    pub top_bucket_avg_label: f64,
    pub top_bucket_positive_label_rate: f64,
    pub monotonicity_score: f64,
    pub complexity: usize,
    pub decision: AutoFactorDecision,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoFactorV2Target {
    RepricePnl10s,
    RepricePnl30s,
}

impl AutoFactorV2Target {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoFactorV2Target::RepricePnl10s => "reprice_pnl_10s",
            AutoFactorV2Target::RepricePnl30s => "reprice_pnl_30s",
        }
    }

    fn label(self, row: &FactorObservationV2) -> f64 {
        match self {
            AutoFactorV2Target::RepricePnl10s => row.label_future_exit_pnl_10s,
            AutoFactorV2Target::RepricePnl30s => row.label_future_exit_pnl_30s,
        }
        .unwrap_or(f64::NAN)
    }
}

#[derive(Debug, Clone)]
struct BucketSummary {
    n: usize,
    avg_label: f64,
    positive_label_rate: f64,
}

pub fn evaluate_named_factor(
    factor: &NamedFactorExpr,
    matrix: &AutoFactorMatrix,
    labels: &[f64],
    windows: &[String],
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

    let buckets = build_buckets(&scored, options.bucket_count);
    let bucket_avg_labels = buckets
        .iter()
        .map(|bucket| bucket.avg_label)
        .collect::<Vec<_>>();
    let monotonicity_score = monotonicity_score(&bucket_avg_labels);
    let bottom = buckets.first();
    let top = buckets.last();
    let complexity = factor.expr.complexity();
    let (decision, reason) = autofactor_decision(
        scored.len(),
        complexity,
        spearman,
        window_ics.len(),
        factor_icir,
        positive_window_ratio,
        top.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
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
        bucket_avg_labels,
        bottom_bucket_n: bottom.map(|bucket| bucket.n).unwrap_or(0),
        bottom_bucket_avg_label: bottom.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_n: top.map(|bucket| bucket.n).unwrap_or(0),
        top_bucket_avg_label: top.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_positive_label_rate: top
            .map(|bucket| bucket.positive_label_rate)
            .unwrap_or(f64::NAN),
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
    options: &AutoFactorOptions,
) -> Result<Vec<AutoFactorReport>, AutoFactorError> {
    let mut reports = Vec::with_capacity(factors.len());
    for factor in factors {
        reports.push(evaluate_named_factor(
            factor, matrix, labels, windows, options,
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
        "target labels are side-aligned executable repricing PnL; reports are candidate discovery gates, not deploy decisions.\n",
    );
    out.push_str(
        "rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,monotonicity,top_bucket_avg_label,top_bucket_positive_label_rate,complexity\n",
    );
    for (idx, report) in reports.iter().take(top_n).enumerate() {
        out.push_str(&format!(
            "{},{},{},{},{},{},{:.6},{:.6},{},{:.6},{:.4},{:.4},{:.6},{:.4},{}\n",
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
            report.monotonicity_score,
            report.top_bucket_avg_label,
            report.top_bucket_positive_label_rate,
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
    let matrix = autofactor_matrix_from_v2(rows)?;
    let labels = autofactor_labels_from_v2(rows, target);
    let windows = autofactor_windows_from_v2(rows);
    let candidates = domain_seed_candidates(&matrix.input_names());
    mine_autofactors(&candidates, &matrix, &labels, &windows, options)
}

pub fn autofactor_matrix_from_v2(
    rows: &[FactorObservationV2],
) -> Result<AutoFactorMatrix, AutoFactorError> {
    let mut columns = BTreeMap::new();
    insert_column(&mut columns, "side_model_edge", rows, |row| {
        row.side_model_edge
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
    out
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

fn build_buckets(scored: &[(usize, f64, f64)], bucket_count: usize) -> Vec<BucketSummary> {
    let mut sorted = scored.to_vec();
    sorted.sort_by(|lhs, rhs| lhs.1.total_cmp(&rhs.1));
    let bucket_count = bucket_count.clamp(2, sorted.len().max(2));
    (0..bucket_count)
        .filter_map(|bucket_idx| {
            let start = bucket_idx * sorted.len() / bucket_count;
            let end = ((bucket_idx + 1) * sorted.len() / bucket_count).min(sorted.len());
            (start < end).then(|| {
                let slice = &sorted[start..end];
                BucketSummary {
                    n: slice.len(),
                    avg_label: finite_mean(slice.iter().map(|(_, _, label)| *label)),
                    positive_label_rate: ratio(
                        slice.iter().filter(|(_, _, label)| *label > 0.0).count(),
                        slice.len(),
                    ),
                }
            })
        })
        .collect()
}

fn autofactor_decision(
    n: usize,
    complexity: usize,
    spearman_ic_value: f64,
    window_count: usize,
    factor_icir: f64,
    positive_window_ratio: f64,
    top_bucket_avg_label: f64,
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
            side_fair_prob: 0.5,
            side_model_edge: score * 0.01,
            side_distance_over_sigma: 0.25,
            abs_distance_to_beat: 10.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 1.0,
            vol_gap: 0.0,
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
            label_executable_fillable: true,
            label_exit_fillable: true,
            label_full_depth_entry_fillable: true,
            label_full_depth_exit_fillable: true,
            label_future_exit_bid_change_5s: Some(score * 0.001),
            label_future_exit_bid_change_10s: Some(score * 0.002),
            label_future_exit_bid_change_30s: Some(score * 0.003),
            label_future_exit_bid_change_60s: Some(score * 0.004),
            label_future_exit_pnl_5s: Some(score * 0.05),
            label_future_exit_pnl_10s: Some(score * 0.10),
            label_future_exit_pnl_30s: Some(score * 0.20),
            label_future_exit_pnl_60s: Some(score * 0.30),
            label_future_exit_fillable_5s: Some(1.0),
            label_future_exit_fillable_10s: Some(1.0),
            label_future_exit_fillable_30s: Some(1.0),
            label_future_exit_fillable_60s: Some(1.0),
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

        let reports =
            mine_autofactors(&candidates, &matrix, &labels, &windows, &options).expect("reports");
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
        let report =
            evaluate_named_factor(&factor, &matrix, &labels, &windows, &options).expect("report");
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
    }
}
