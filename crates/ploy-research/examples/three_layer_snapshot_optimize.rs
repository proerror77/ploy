//! three_layer_snapshot_optimize - PM5D three-layer optimizer over an immutable research snapshot.
//!
//! This runner is the canonical optimizer path for multi-day research. It loads a
//! compiled `ResearchSnapshot`, expands side-aware `FactorObservationV2` rows,
//! and optimizes gates against official-settlement executable PM CLOB labels
//! without replaying raw tick Parquet for every trial.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_research::{
    FactorObservationV2, FactorReviewOptions, ResearchSnapshotManifest, build_data_health_report,
    build_factor_observations_v2_with_deribit_and_pm_books, load_research_snapshot,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
struct SnapshotThreeLayerParams {
    min_direction_prob: f64,
    min_distance_over_sigma: f64,
    min_confirmation_score: f64,
    min_drift_confirmation: f64,
    min_edge: f64,
    min_reward_risk: f64,
    min_entry_score: f64,
    alpha_contrarian: bool,
    cex_contrarian: bool,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StrategyProfile {
    /// Historical behavior: blend continuation and CEX/PM book confirmation.
    Mixed,
    /// Strategy A: contrarian alpha inside executable liquidity/risk gates.
    Champion,
    /// Strategy B: Strategy A plus CEX/PM order-book imbalance soft score.
    ObiSoft,
    /// Strategy C: Strategy A plus CEX continuation soft score.
    ContinuationSoft,
}

impl StrategyProfile {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "mixed" | "legacy" => Ok(Self::Mixed),
            "champion" | "a" | "alpha" | "alpha_only" | "contrarian_alpha" => Ok(Self::Champion),
            "obi" | "obi_soft" | "b" | "book_imbalance" | "orderbook" => Ok(Self::ObiSoft),
            "continuation" | "continuation_soft" | "c" | "cex_continuation" => {
                Ok(Self::ContinuationSoft)
            }
            other => anyhow::bail!(
                "unknown --strategy-profile {other:?}; expected mixed, champion, obi_soft, or continuation_soft"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Champion => "champion",
            Self::ObiSoft => "obi_soft",
            Self::ContinuationSoft => "continuation_soft",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Mixed => "legacy mixed continuation + order-book confirmation",
            Self::Champion => "contrarian alpha + executable liquidity/risk gates",
            Self::ObiSoft => "champion + CEX/PM order-book imbalance soft score",
            Self::ContinuationSoft => "champion + CEX continuation soft score",
        }
    }

    fn fixes_alpha_contrarian(self) -> Option<bool> {
        match self {
            Self::Mixed => None,
            Self::Champion | Self::ObiSoft | Self::ContinuationSoft => Some(true),
        }
    }

    fn fixes_cex_contrarian(self) -> Option<bool> {
        match self {
            Self::Champion => Some(false),
            Self::Mixed | Self::ObiSoft | Self::ContinuationSoft => None,
        }
    }

    fn fixes_confirmation_threshold(self) -> Option<f64> {
        match self {
            Self::Champion => Some(0.0),
            Self::Mixed | Self::ObiSoft | Self::ContinuationSoft => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SnapshotObjectiveMetrics {
    candidates: usize,
    selected: usize,
    trades: usize,
    rejected_duplicate: usize,
    rejected_cooldown: usize,
    rejected_non_executable: usize,
    net_pnl: f64,
    avg_pnl: f64,
    avg_entry_price: f64,
    avg_reward_risk: f64,
    sharpe: f64,
    max_drawdown: f64,
    log_growth: f64,
    avg_log_return: f64,
    positive_day_rate: f64,
    positive_symbol_rate: f64,
    concentration: f64,
    reject_rate: f64,
    objective: f64,
    fill_rate: f64,
    win_rate: f64,
}

#[derive(Debug, Serialize)]
struct OptimizeSummary<'a> {
    snapshot_schema: &'a str,
    snapshot_hash: &'a str,
    snapshot_generated_at: DateTime<Utc>,
    snapshot_data_requirements: &'a [String],
    snapshot_data_audit_status: Option<&'a str>,
    snapshot_data_audit_report: Option<&'a str>,
    snapshot_include_deribit: bool,
    symbols: &'a [String],
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    strategy_profile: StrategyProfile,
    trials: usize,
    stake_usd: f64,
    selection_objective: f64,
    min_trades: usize,
    min_trades_source: &'a str,
    train_opportunities: usize,
    val_opportunities: usize,
    train_rows: usize,
    val_rows: usize,
    train_underpowered: bool,
    validation_underpowered: bool,
    best_params: SnapshotThreeLayerParams,
    train_metrics: SnapshotObjectiveMetrics,
    val_metrics: SnapshotObjectiveMetrics,
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn parse_window(args: &[String], date_flag: &str, ts_flag: &str, is_end: bool) -> DateTime<Utc> {
    if let Some(raw) = flag_value(args, ts_flag) {
        return parse_timestamp(&raw);
    }
    let raw = flag_value(args, date_flag).unwrap_or_else(|| panic!("{date_flag} required"));
    if is_end {
        parse_date_end(&raw)
    } else {
        parse_date_start(&raw)
    }
}

fn slice_by_time(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> &[FactorObservationV2] {
    let lo = rows.partition_point(|row| row.tick_ts < start);
    let hi = rows.partition_point(|row| row.tick_ts < end);
    &rows[lo..hi]
}

fn validate_snapshot_scope(
    manifest: &ResearchSnapshotManifest,
    symbols: &[String],
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    stake_usd: f64,
) -> Result<()> {
    if !manifest.immutable_input {
        anyhow::bail!("snapshot manifest is not immutable_input=true");
    }
    if manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("snapshot manifest is missing snapshot_hash");
    }
    if !manifest.require_official_settlement {
        anyhow::bail!("snapshot was not compiled with require_official_settlement=true");
    }
    if (manifest.stake_usd - stake_usd).abs() > 1e-9 {
        anyhow::bail!(
            "snapshot stake_usd {} does not match optimizer stake_usd {}",
            manifest.stake_usd,
            stake_usd
        );
    }

    let snapshot_symbols: HashSet<&str> = manifest.symbols.iter().map(String::as_str).collect();
    let missing: Vec<&str> = symbols
        .iter()
        .map(String::as_str)
        .filter(|symbol| !snapshot_symbols.contains(symbol))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "snapshot symbols {:?} do not cover requested symbols {:?}; missing {:?}",
            manifest.symbols,
            symbols,
            missing
        );
    }

    let requested_start = train_start.min(val_start);
    let requested_end = train_end.max(val_end);
    if manifest.start > requested_start || manifest.end < requested_end {
        anyhow::bail!(
            "snapshot window {} -> {} does not cover optimizer window {} -> {}",
            manifest.start,
            manifest.end,
            requested_start,
            requested_end
        );
    }

    Ok(())
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn crypto_fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

fn reward_risk_ratio(entry_price: f64) -> f64 {
    if !entry_price.is_finite() || entry_price <= 0.0 || entry_price >= 1.0 {
        return f64::NAN;
    }
    let fee = crypto_fee_cost(entry_price);
    let reward = 1.0 - entry_price - fee;
    let risk = entry_price + fee;
    if risk <= 0.0 { f64::NAN } else { reward / risk }
}

fn confirmation_score(row: &FactorObservationV2, profile: StrategyProfile) -> f64 {
    let side = row.side.multiplier();
    let continuation = finite_or_zero(row.cex_continuation_score_side).clamp(-1.0, 1.0);
    let obi = finite_or_zero(row.obi_10 * side).clamp(-1.0, 1.0);
    let obi_delta_10s = finite_or_zero(row.obi_delta_10s_side).clamp(-1.0, 1.0);
    let obi_persistence_30s = finite_or_zero(row.obi_persistence_30s_side).clamp(-1.0, 1.0);
    let depth = finite_or_zero(row.depth_imbalance * side).clamp(-1.0, 1.0);
    let microprice = finite_or_zero(row.microprice_offset_bps * side / 10.0).clamp(-1.0, 1.0);
    let trade_imbalance = finite_or_zero(row.trade_imbalance_delta_10s_side).clamp(-1.0, 1.0);

    match profile {
        StrategyProfile::Mixed => {
            0.45 * continuation + 0.25 * obi + 0.15 * depth + 0.15 * microprice
        }
        StrategyProfile::Champion => 0.0,
        StrategyProfile::ObiSoft => {
            0.30 * obi
                + 0.20 * obi_delta_10s
                + 0.20 * obi_persistence_30s
                + 0.15 * depth
                + 0.10 * microprice
                + 0.05 * trade_imbalance
        }
        StrategyProfile::ContinuationSoft => continuation,
    }
}

fn three_layer_entry_score(
    row: &FactorObservationV2,
    edge: f64,
    confirmation: f64,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> f64 {
    let alpha_prob = transformed_model_probability(row.side_model_prob, params.alpha_contrarian);
    let direction_score = directional_score(alpha_prob, params.min_direction_prob, 0.25, false);
    let distance_score = directional_score(
        row.side_distance_over_sigma,
        params.min_distance_over_sigma,
        0.60,
        params.alpha_contrarian,
    );
    let edge_score = executable_edge_score(edge, params);
    let drift_side = row.drift_30s * row.side.multiplier();
    let drift_score = ((drift_side - params.min_drift_confirmation) * 800.0).clamp(-0.50, 1.0);
    let pm_momentum = row
        .entry_ask_change_10s
        .max(row.entry_ask_change_30s)
        .max(row.pm_reprice_speed_30s * 30.0);
    let pm_momentum_score = if pm_momentum.is_finite() {
        (pm_momentum / 0.08).clamp(-0.50, 1.0)
    } else {
        0.0
    };
    let liquidity_score = if row.label_full_depth_entry_fillable {
        1.0
    } else if row.label_executable_fillable {
        0.5
    } else {
        -0.5
    };
    let confirmation_score = directional_score(
        confirmation,
        params.min_confirmation_score,
        0.50,
        params.cex_contrarian,
    );
    if profile == StrategyProfile::Champion {
        return 0.33 * direction_score
            + 0.17 * distance_score
            + 0.25 * edge_score
            + 0.10 * drift_score
            + 0.10 * pm_momentum_score
            + 0.05 * liquidity_score;
    }
    0.25 * direction_score
        + 0.12 * distance_score
        + 0.18 * edge_score
        + 0.15 * confirmation_score
        + 0.10 * drift_score
        + 0.12 * pm_momentum_score
        + 0.08 * liquidity_score
}

fn transformed_model_probability(side_model_prob: f64, alpha_contrarian: bool) -> f64 {
    if !side_model_prob.is_finite() {
        return f64::NAN;
    }
    if alpha_contrarian {
        1.0 - side_model_prob
    } else {
        side_model_prob
    }
}

fn executable_edge_threshold(params: &SnapshotThreeLayerParams) -> f64 {
    params.min_edge.max(0.0)
}

fn executable_edge_score(edge: f64, params: &SnapshotThreeLayerParams) -> f64 {
    directional_score(edge, executable_edge_threshold(params), 0.08, false)
}

fn executable_model_edge(row: &FactorObservationV2, params: &SnapshotThreeLayerParams) -> f64 {
    transformed_model_probability(row.side_model_prob, params.alpha_contrarian)
        - row.entry_ask
        - crypto_fee_cost(row.entry_ask)
}

fn directional_score(value: f64, threshold: f64, scale: f64, contrarian: bool) -> f64 {
    if !value.is_finite() || !threshold.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return -0.50;
    }
    let signed = if contrarian {
        threshold - value
    } else {
        value - threshold
    };
    (signed / scale).clamp(-0.50, 1.0)
}

fn executable_pnl(row: &FactorObservationV2) -> Option<f64> {
    row.label_full_depth_executable_pnl_15u
        .or(row.label_executable_pnl_15u)
        .filter(|pnl| pnl.is_finite())
}

fn entry_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable || row.label_executable_fillable
}

fn row_passes_gates(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> bool {
    if row.time_remaining_secs < params.min_time_remaining_secs
        || row.time_remaining_secs > params.max_time_remaining_secs
    {
        return false;
    }
    if !row.entry_ask.is_finite() || row.entry_ask < 0.10 || row.entry_ask > 0.85 {
        return false;
    }
    if !row.pm_lag_secs.is_finite() || row.pm_lag_secs < 0.0 || row.pm_lag_secs > 15.0 {
        return false;
    }
    if !row.side_model_prob.is_finite() {
        return false;
    }
    if !row.side_distance_over_sigma.is_finite() {
        return false;
    }

    let drift_side = row.drift_30s * row.side.multiplier();
    if !drift_side.is_finite() {
        return false;
    }

    let edge = executable_model_edge(row, params);
    if !edge.is_finite() || edge < executable_edge_threshold(params) {
        return false;
    }
    let reward_risk = reward_risk_ratio(row.entry_ask);
    if !reward_risk.is_finite() || reward_risk < params.min_reward_risk {
        return false;
    }
    let confirmation = confirmation_score(row, profile);
    if !confirmation.is_finite() {
        return false;
    }
    three_layer_entry_score(row, edge, confirmation, params, profile) >= params.min_entry_score
}

fn evaluate_snapshot_objective(
    rows: &[FactorObservationV2],
    params: &SnapshotThreeLayerParams,
    min_trades: usize,
    stake_usd: f64,
    profile: StrategyProfile,
) -> SnapshotObjectiveMetrics {
    let mut last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut traded_event_sides: HashSet<String> = HashSet::new();
    let mut pnls = Vec::new();
    let mut pnl_by_day: HashMap<String, f64> = HashMap::new();
    let mut pnl_by_symbol: HashMap<String, f64> = HashMap::new();
    let mut candidates = 0usize;
    let mut selected = 0usize;
    let mut rejected_duplicate = 0usize;
    let mut rejected_cooldown = 0usize;
    let mut rejected_non_executable = 0usize;
    let mut entry_price_sum = 0.0;
    let mut reward_risk_sum = 0.0;

    for row in rows {
        if !row_passes_gates(row, params, profile) {
            continue;
        }
        candidates += 1;

        let event_side_key = format!("{}:{}", row.event_id, row.side.as_str());
        if traded_event_sides.contains(&event_side_key) {
            rejected_duplicate += 1;
            continue;
        }
        if let Some(last_ts) = last_trade_by_symbol.get(&row.symbol) {
            if (row.tick_ts - *last_ts).num_seconds() < params.cooldown_secs {
                rejected_cooldown += 1;
                continue;
            }
        }

        selected += 1;
        if !entry_fillable(row) {
            rejected_non_executable += 1;
            continue;
        }
        let Some(pnl) = executable_pnl(row) else {
            rejected_non_executable += 1;
            continue;
        };
        let reward_risk = reward_risk_ratio(row.entry_ask);
        if !reward_risk.is_finite() {
            rejected_non_executable += 1;
            continue;
        }

        pnls.push(pnl);
        entry_price_sum += row.entry_ask;
        reward_risk_sum += reward_risk;
        *pnl_by_day
            .entry(row.tick_ts.date_naive().to_string())
            .or_default() += pnl;
        *pnl_by_symbol.entry(row.symbol.clone()).or_default() += pnl;
        traded_event_sides.insert(event_side_key);
        last_trade_by_symbol.insert(row.symbol.clone(), row.tick_ts);
    }

    let trades = pnls.len();
    let net_pnl = pnls.iter().sum::<f64>();
    let avg_pnl = if trades == 0 {
        f64::NAN
    } else {
        net_pnl / trades as f64
    };
    let avg_entry_price = if trades == 0 {
        f64::NAN
    } else {
        entry_price_sum / trades as f64
    };
    let avg_reward_risk = if trades == 0 {
        f64::NAN
    } else {
        reward_risk_sum / trades as f64
    };
    let sharpe = trade_sharpe(&pnls);
    let fill_rate = ratio(trades, selected);
    let reject_rate = ratio(rejected_non_executable, selected);
    let max_drawdown = max_drawdown(&pnls);
    let log_growth = compounded_log_growth(&pnls, stake_usd);
    let avg_log_return = if trades == 0 {
        f64::NAN
    } else {
        log_growth / trades as f64
    };
    let positive_day_rate = positive_bucket_rate(&pnl_by_day);
    let positive_symbol_rate = positive_bucket_rate(&pnl_by_symbol);
    let concentration = concentration_ratio(&pnl_by_day).max(concentration_ratio(&pnl_by_symbol));
    let win_rate = if trades == 0 {
        f64::NAN
    } else {
        ratio(pnls.iter().filter(|pnl| **pnl > 0.0).count(), trades)
    };
    let objective = if trades < min_trades {
        -1_000_000.0 + trades as f64
    } else {
        stable_compounding_objective(StableObjectiveInputs {
            trades,
            min_trades,
            stake_usd,
            net_pnl,
            sharpe,
            max_drawdown,
            log_growth,
            avg_entry_price,
            avg_reward_risk,
            fill_rate,
            reject_rate,
            positive_day_rate,
            positive_symbol_rate,
            concentration,
        })
    };

    SnapshotObjectiveMetrics {
        candidates,
        selected,
        trades,
        rejected_duplicate,
        rejected_cooldown,
        rejected_non_executable,
        net_pnl,
        avg_pnl,
        avg_entry_price,
        avg_reward_risk,
        sharpe,
        max_drawdown,
        log_growth,
        avg_log_return,
        positive_day_rate,
        positive_symbol_rate,
        concentration,
        reject_rate,
        objective,
        fill_rate,
        win_rate,
    }
}

#[derive(Debug, Clone, Copy)]
struct StableObjectiveInputs {
    trades: usize,
    min_trades: usize,
    stake_usd: f64,
    net_pnl: f64,
    sharpe: f64,
    max_drawdown: f64,
    log_growth: f64,
    avg_entry_price: f64,
    avg_reward_risk: f64,
    fill_rate: f64,
    reject_rate: f64,
    positive_day_rate: f64,
    positive_symbol_rate: f64,
    concentration: f64,
}

fn stable_compounding_objective(inputs: StableObjectiveInputs) -> f64 {
    if inputs.trades < inputs.min_trades {
        return -1_000_000.0 + inputs.trades as f64;
    }
    let stake = inputs.stake_usd.max(1.0);
    let sample_power = sample_power_multiplier(inputs.trades, inputs.min_trades);
    let pnl_stakes = inputs.net_pnl / stake;
    let pnl_term = pnl_stakes.signum() * pnl_stakes.abs().ln_1p();
    let drawdown_stakes = inputs.max_drawdown / stake;
    let reward_risk_quality = if inputs.avg_reward_risk.is_finite() {
        (inputs.avg_reward_risk - 1.0).clamp(-1.0, 2.0)
    } else {
        -1.0
    };
    let rich_entry_penalty = if inputs.avg_entry_price.is_finite() {
        ((inputs.avg_entry_price - 0.55).max(0.0) * 4.0).clamp(0.0, 2.0)
    } else {
        2.0
    };
    let stability_bonus = 2.0 * inputs.positive_day_rate
        + 1.5 * inputs.positive_symbol_rate
        + inputs.fill_rate.clamp(0.0, 1.0)
        + 0.5 * reward_risk_quality;
    let risk_penalty = 0.45 * drawdown_stakes
        + 2.0 * inputs.reject_rate.clamp(0.0, 1.0)
        + 2.0 * inputs.concentration.clamp(0.0, 1.0)
        + rich_entry_penalty;
    let sharpe_bonus = 0.25 * inputs.sharpe.clamp(-5.0, 5.0);

    sample_power * (inputs.log_growth + pnl_term + stability_bonus + sharpe_bonus - risk_penalty)
}

fn holistic_selection_objective(
    train: &SnapshotObjectiveMetrics,
    validation: &SnapshotObjectiveMetrics,
    min_trades: usize,
) -> f64 {
    if train.trades < min_trades || validation.trades < min_trades {
        return -1_000_000.0 + train.trades.min(validation.trades) as f64;
    }
    if train.net_pnl <= 0.0 || validation.net_pnl <= 0.0 {
        return train.objective.min(validation.objective) - 10_000.0;
    }

    let stability_gap = (train.positive_day_rate - validation.positive_day_rate).abs()
        + (train.positive_symbol_rate - validation.positive_symbol_rate).abs()
        + (train.fill_rate - validation.fill_rate).abs()
        + (train.concentration - validation.concentration).abs();
    let reward_risk_gap =
        finite_gap(train.avg_reward_risk, validation.avg_reward_risk).clamp(0.0, 3.0);
    let entry_gap = finite_gap(train.avg_entry_price, validation.avg_entry_price).clamp(0.0, 1.0);
    let generalization_penalty = 3.0 * stability_gap + 0.75 * reward_risk_gap + 2.0 * entry_gap;

    0.40 * train.objective + 0.60 * validation.objective - generalization_penalty
}

fn finite_gap(left: f64, right: f64) -> f64 {
    if left.is_finite() && right.is_finite() {
        (left - right).abs()
    } else {
        1.0
    }
}

fn max_drawdown(pnls: &[f64]) -> f64 {
    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut max_drawdown = 0.0;
    for pnl in pnls {
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let drawdown = peak - equity;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
    }
    max_drawdown
}

fn compounded_log_growth(pnls: &[f64], stake_usd: f64) -> f64 {
    let stake = stake_usd.max(1.0);
    pnls.iter()
        .map(|pnl| (1.0 + (pnl / stake).clamp(-0.99, 10.0)).ln())
        .sum()
}

fn positive_bucket_rate(buckets: &HashMap<String, f64>) -> f64 {
    if buckets.is_empty() {
        return 0.0;
    }
    ratio(
        buckets.values().filter(|pnl| **pnl > 0.0).count(),
        buckets.len(),
    )
}

fn concentration_ratio(buckets: &HashMap<String, f64>) -> f64 {
    let total_abs = buckets.values().map(|pnl| pnl.abs()).sum::<f64>();
    if total_abs <= 1e-9 {
        return 0.0;
    }
    buckets
        .values()
        .map(|pnl| pnl.abs() / total_abs)
        .fold(0.0, f64::max)
}

fn trade_sharpe(pnls: &[f64]) -> f64 {
    if pnls.is_empty() {
        return -100.0;
    }
    let mean = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let variance = pnls.iter().map(|pnl| (pnl - mean).powi(2)).sum::<f64>() / pnls.len() as f64;
    let std = variance.sqrt();
    if std <= 1e-9 {
        return mean.signum() * (pnls.len() as f64).sqrt();
    }
    mean / std * (pnls.len() as f64).sqrt()
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn event_side_opportunities(rows: &[FactorObservationV2]) -> usize {
    rows.iter()
        .map(|row| format!("{}:{}", row.event_id, row.side.as_str()))
        .collect::<HashSet<_>>()
        .len()
}

fn slice_coverage(rows: &[FactorObservationV2]) -> (usize, usize, usize) {
    let event_sides = event_side_opportunities(rows);
    let days = rows
        .iter()
        .map(|row| row.tick_ts.date_naive())
        .collect::<HashSet<_>>()
        .len();
    let symbols = rows
        .iter()
        .map(|row| row.symbol.as_str())
        .collect::<HashSet<_>>()
        .len();
    (event_sides, days, symbols)
}

fn default_min_trades(
    train_rows: &[FactorObservationV2],
    val_rows: &[FactorObservationV2],
) -> usize {
    let (train_events, train_days, train_symbols) = slice_coverage(train_rows);
    let (val_events, val_days, val_symbols) = slice_coverage(val_rows);
    default_min_trades_from_coverage(
        train_events.min(val_events),
        train_days.min(val_days),
        train_symbols.min(val_symbols),
    )
}

fn default_min_trades_from_coverage(
    event_side_opportunities: usize,
    days: usize,
    symbols: usize,
) -> usize {
    let opportunity_floor = ((event_side_opportunities as f64) * 0.015).ceil() as usize;
    let bucket_floor = days.saturating_mul(symbols).saturating_mul(8);
    opportunity_floor.max(bucket_floor).clamp(40, 1_500)
}

fn sample_power_multiplier(trades: usize, min_trades: usize) -> f64 {
    if trades < min_trades || min_trades == 0 {
        return 0.0;
    }
    let full_power_trades = min_trades.saturating_mul(4).max(min_trades + 1);
    (trades as f64 / full_power_trades as f64).min(1.0).sqrt()
}

fn write_outputs(
    output_dir: Option<PathBuf>,
    summary: &OptimizeSummary<'_>,
    trials: &[optimizer::sampler::CompletedTrial<f64>],
) -> Result<()> {
    let Some(output_dir) = output_dir else {
        return Ok(());
    };
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("create output dir {}", output_dir.display()))?;

    let summary_json =
        serde_json::to_string_pretty(summary).context("serialize snapshot optimizer summary")?;
    fs::write(
        output_dir.join("three-layer-snapshot-optimize-summary.json"),
        summary_json,
    )
    .context("write snapshot optimizer summary")?;

    let params = summary.best_params;
    let config = format!(
        "# PM5D three-layer snapshot optimizer output\n\
         # snapshot_hash = {}\n\
         # strategy_profile = {}\n\
         # selection_objective = {:.6}\n\
         # train_objective = {:.6}\n\
         # validation_objective = {:.6}\n\
         # validation_sharpe = {:.6}\n\
         # validation_pnl = {:.6}\n\
         # validation_trades = {}\n\
         # validation_avg_entry = {:.6}\n\
         # validation_avg_reward_risk = {:.6}\n\
         # validation_max_drawdown = {:.6}\n\
         # validation_log_growth = {:.6}\n\
         # validation_positive_day_rate = {:.6}\n\
         # validation_positive_symbol_rate = {:.6}\n\
         # min_trades = {}\n\
         # min_trades_source = {}\n\
         # data_requirements = {}\n\
         # data_audit_status = {}\n\
         # data_audit_report = {}\n\
         # include_deribit = {}\n\
         # train_underpowered = {}\n\
         # validation_underpowered = {}\n\
         three_layer_min_direction_prob = {:.6}\n\
         three_layer_min_distance_over_sigma = {:.6}\n\
         three_layer_min_confirmation_score = {:.6}\n\
         three_layer_min_drift_confirmation = {:.8}\n\
         three_layer_min_edge = {:.6}\n\
         three_layer_min_reward_risk = {:.6}\n\
         three_layer_min_entry_score = {:.6}\n\
         three_layer_alpha_contrarian = {}\n\
         three_layer_cex_contrarian = {}\n\
         cooldown_secs = {}\n\
         min_time_remaining_secs = {}\n\
         max_time_remaining_secs = {}\n\
         # three_layer_take_profit_ask and three_layer_stop_distance_pct are not optimized here.\n",
        summary.snapshot_hash,
        summary.strategy_profile.as_str(),
        summary.selection_objective,
        summary.train_metrics.objective,
        summary.val_metrics.objective,
        summary.val_metrics.sharpe,
        summary.val_metrics.net_pnl,
        summary.val_metrics.trades,
        summary.val_metrics.avg_entry_price,
        summary.val_metrics.avg_reward_risk,
        summary.val_metrics.max_drawdown,
        summary.val_metrics.log_growth,
        summary.val_metrics.positive_day_rate,
        summary.val_metrics.positive_symbol_rate,
        summary.min_trades,
        summary.min_trades_source,
        if summary.snapshot_data_requirements.is_empty() {
            "<unspecified>".to_string()
        } else {
            summary.snapshot_data_requirements.join(",")
        },
        summary
            .snapshot_data_audit_status
            .unwrap_or("<not-recorded>"),
        summary
            .snapshot_data_audit_report
            .unwrap_or("<not-recorded>"),
        summary.snapshot_include_deribit,
        summary.train_underpowered,
        summary.validation_underpowered,
        params.min_direction_prob,
        params.min_distance_over_sigma,
        params.min_confirmation_score,
        params.min_drift_confirmation,
        params.min_edge,
        params.min_reward_risk,
        params.min_entry_score,
        params.alpha_contrarian,
        params.cex_contrarian,
        params.cooldown_secs,
        params.min_time_remaining_secs,
        params.max_time_remaining_secs,
    );
    fs::write(
        output_dir.join("three-layer-snapshot-best-params.toml"),
        config,
    )
    .context("write snapshot optimizer best params")?;

    let mut top_trials = trials.to_vec();
    top_trials.sort_by(|lhs, rhs| {
        rhs.value
            .partial_cmp(&lhs.value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_text = top_trials
        .iter()
        .take(20)
        .map(|trial| format!("trial={} objective={:.6}", trial.id, trial.value))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        output_dir.join("three-layer-snapshot-top-trials.txt"),
        top_text,
    )
    .context("write snapshot optimizer top trials")?;
    Ok(())
}

fn print_metrics(label: &str, metrics: &SnapshotObjectiveMetrics) {
    eprintln!(
        "{label}: objective={:.3} sharpe={:.3} pnl=${:.2} avg_entry={:.3} avg_rr={:.3} max_dd=${:.2} log_growth={:.3} pos_day={:.2}% pos_symbol={:.2}% concentration={:.2}% trades={} candidates={} selected={} fill_rate={:.2}% win_rate={:.2}% reject={:.2}% non_exec={} dup={} cooldown={}",
        metrics.objective,
        metrics.sharpe,
        metrics.net_pnl,
        metrics.avg_entry_price,
        metrics.avg_reward_risk,
        metrics.max_drawdown,
        metrics.log_growth,
        metrics.positive_day_rate * 100.0,
        metrics.positive_symbol_rate * 100.0,
        metrics.concentration * 100.0,
        metrics.trades,
        metrics.candidates,
        metrics.selected,
        metrics.fill_rate * 100.0,
        metrics.win_rate * 100.0,
        metrics.reject_rate * 100.0,
        metrics.rejected_non_executable,
        metrics.rejected_duplicate,
        metrics.rejected_cooldown,
    );
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let snapshot_dir = PathBuf::from(flag_value(&args, "--snapshot-dir").unwrap_or_else(|| {
        eprintln!("ERROR: --snapshot-dir is required for three_layer_snapshot_optimize");
        std::process::exit(2);
    }));
    let train_start = parse_window(&args, "--train-start", "--train-start-ts", false);
    let train_end = parse_window(&args, "--train-end", "--train-end-ts", true);
    let val_start = parse_window(&args, "--val-start", "--val-start-ts", false);
    let val_end = parse_window(&args, "--val-end", "--val-end-ts", true);
    let symbols = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let n_trials = flag_value(&args, "--trials")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(50usize);
    let strategy_profile = StrategyProfile::parse(
        &flag_value(&args, "--strategy-profile").unwrap_or_else(|| "mixed".to_string()),
    )?;
    let stake_usd = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);
    let min_trades_override =
        flag_value(&args, "--min-trades").and_then(|raw| raw.parse::<usize>().ok());
    let output_dir = flag_value(&args, "--output-dir").map(PathBuf::from);

    let started = Instant::now();
    let snapshot = load_research_snapshot(&snapshot_dir)
        .with_context(|| format!("load research snapshot {}", snapshot_dir.display()))?;
    validate_snapshot_scope(
        &snapshot.manifest,
        &symbols,
        train_start,
        train_end,
        val_start,
        val_end,
        stake_usd,
    )?;
    let snapshot_hash = snapshot
        .manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or("<missing>");

    eprintln!("=== PM5D Three-Layer Snapshot Optimization ===");
    eprintln!(
        "Research snapshot: schema={} hash={} generated_at={}",
        snapshot.manifest.schema_version, snapshot_hash, snapshot.manifest.generated_at
    );
    eprintln!(
        "Snapshot rows: observations={} deribit={} pm_books={} load_ms={}",
        snapshot.observations.len(),
        snapshot.deribit_snapshots.len(),
        snapshot.pm_book_snapshots.len(),
        started.elapsed().as_millis()
    );
    eprintln!(
        "Train: {} -> {}  Val: {} -> {}  Symbols: {:?}",
        train_start, train_end, val_start, val_end, symbols
    );
    eprintln!(
        "Strategy profile: {} ({})",
        strategy_profile.as_str(),
        strategy_profile.description()
    );
    eprintln!(
        "Trials: {n_trials}  Algorithm: TPE  Stake: ${stake_usd:.2}  Min trades: {}",
        min_trades_override
            .map(|value| value.to_string())
            .unwrap_or_else(|| "dynamic".to_string())
    );
    eprintln!(
        "Objective labels: official settlement + executable PM CLOB full-depth/top-book fillability"
    );
    eprintln!(
        "Note: stop-loss/take-profit path exits are not optimized in this observation-level runner."
    );

    let review_options = FactorReviewOptions {
        stake_usd,
        min_observations: min_trades_override.unwrap_or(20),
        top_quantile: 0.2,
    };
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &snapshot.observations,
        &snapshot.deribit_snapshots,
        &snapshot.pm_book_snapshots,
        &review_options,
    );
    let symbol_set = symbols.iter().map(String::as_str).collect::<HashSet<_>>();
    v2_rows.retain(|row| symbol_set.contains(row.symbol.as_str()));
    v2_rows.sort_by_key(|row| row.tick_ts);

    let health = build_data_health_report(&snapshot.observations, &v2_rows);
    eprintln!(
        "Data health: source_obs={} v2_rows={} executable_pnl_rows={} full_depth_pnl_rows={} entry_fill_rate={:.2}% full_depth_entry_fill_rate={:.2}%",
        health.source_observations,
        health.v2_rows,
        health.executable_pnl_rows,
        health.full_depth_executable_pnl_rows,
        health.entry_fill_rate() * 100.0,
        health.full_depth_entry_fill_rate() * 100.0,
    );

    let train_rows = slice_by_time(&v2_rows, train_start, train_end).to_vec();
    let val_rows = slice_by_time(&v2_rows, val_start, val_end).to_vec();
    let train_opportunities = event_side_opportunities(&train_rows);
    let val_opportunities = event_side_opportunities(&val_rows);
    let min_trades =
        min_trades_override.unwrap_or_else(|| default_min_trades(&train_rows, &val_rows));
    let min_trades_source = if min_trades_override.is_some() {
        "cli"
    } else {
        "dynamic_default"
    };
    if train_rows.len() < min_trades {
        anyhow::bail!(
            "training slice has only {} rows; need at least {}",
            train_rows.len(),
            min_trades
        );
    }
    if val_rows.len() < min_trades {
        anyhow::bail!(
            "validation slice has only {} rows; need at least {}",
            val_rows.len(),
            min_trades
        );
    }
    eprintln!(
        "Slice rows: train={} validation={}",
        train_rows.len(),
        val_rows.len()
    );
    eprintln!(
        "Event-side opportunities: train={} validation={}",
        train_opportunities, val_opportunities
    );
    eprintln!("Trade floor: min_trades={min_trades} source={min_trades_source}");

    let train_rows = Arc::new(train_rows);
    let val_rows = Arc::new(val_rows);
    let study: Study<f64> = Study::maximize(TpeSampler::new());
    let p_min_direction_prob = FloatParam::new(0.50, 0.68).name("three_layer_min_direction_prob");
    let p_min_distance_over_sigma =
        FloatParam::new(-0.20, 0.60).name("three_layer_min_distance_over_sigma");
    let p_min_confirmation_score =
        FloatParam::new(-0.15, 0.25).name("three_layer_min_confirmation_score");
    let p_min_drift_confirmation =
        FloatParam::new(-0.0005, 0.0008).name("three_layer_min_drift_confirmation");
    let p_min_edge = FloatParam::new(0.0, 0.08).name("three_layer_min_edge");
    let p_min_reward_risk = FloatParam::new(0.20, 2.0).name("three_layer_min_reward_risk");
    let p_min_entry_score = FloatParam::new(0.05, 0.55).name("three_layer_min_entry_score");
    let p_alpha_contrarian = BoolParam::new().name("alpha_contrarian");
    let p_cex_contrarian = BoolParam::new().name("cex_contrarian");
    let p_cooldown_secs = IntParam::new(15, 60).name("cooldown_secs");
    let p_min_time_remaining_secs = IntParam::new(30, 120).name("min_time_remaining_secs");
    let p_max_time_span_secs = IntParam::new(30, 120).name("three_layer_time_span_secs");

    {
        let train_rows = Arc::clone(&train_rows);
        let p_min_direction_prob_c = p_min_direction_prob.clone();
        let p_min_distance_over_sigma_c = p_min_distance_over_sigma.clone();
        let p_min_confirmation_score_c = p_min_confirmation_score.clone();
        let p_min_drift_confirmation_c = p_min_drift_confirmation.clone();
        let p_min_edge_c = p_min_edge.clone();
        let p_min_reward_risk_c = p_min_reward_risk.clone();
        let p_min_entry_score_c = p_min_entry_score.clone();
        let p_alpha_contrarian_c = p_alpha_contrarian.clone();
        let p_cex_contrarian_c = p_cex_contrarian.clone();
        let p_cooldown_secs_c = p_cooldown_secs.clone();
        let p_min_time_remaining_secs_c = p_min_time_remaining_secs.clone();
        let p_max_time_span_secs_c = p_max_time_span_secs.clone();
        let val_rows = Arc::clone(&val_rows);

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let min_time_remaining_secs = p_min_time_remaining_secs_c.suggest(trial)?;
                let alpha_contrarian = match strategy_profile.fixes_alpha_contrarian() {
                    Some(value) => value,
                    None => p_alpha_contrarian_c.suggest(trial)?,
                };
                let cex_contrarian = match strategy_profile.fixes_cex_contrarian() {
                    Some(value) => value,
                    None => p_cex_contrarian_c.suggest(trial)?,
                };
                let min_confirmation_score =
                    match strategy_profile.fixes_confirmation_threshold() {
                        Some(value) => value,
                        None => p_min_confirmation_score_c.suggest(trial)?,
                    };
                let params = SnapshotThreeLayerParams {
                    min_direction_prob: p_min_direction_prob_c.suggest(trial)?,
                    min_distance_over_sigma: p_min_distance_over_sigma_c.suggest(trial)?,
                    min_confirmation_score,
                    min_drift_confirmation: p_min_drift_confirmation_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    min_reward_risk: p_min_reward_risk_c.suggest(trial)?,
                    min_entry_score: p_min_entry_score_c.suggest(trial)?,
                    alpha_contrarian,
                    cex_contrarian,
                    cooldown_secs: p_cooldown_secs_c.suggest(trial)?,
                    min_time_remaining_secs,
                    max_time_remaining_secs: min_time_remaining_secs
                        + p_max_time_span_secs_c.suggest(trial)?,
                };

                let train_metrics = evaluate_snapshot_objective(
                    &train_rows,
                    &params,
                    min_trades,
                    stake_usd,
                    strategy_profile,
                );
                let val_metrics = evaluate_snapshot_objective(
                    &val_rows,
                    &params,
                    min_trades,
                    stake_usd,
                    strategy_profile,
                );
                let objective =
                    holistic_selection_objective(&train_metrics, &val_metrics, min_trades);
                eprintln!(
                    "  Trial {:>3}: profile={} source=snapshot-observation objective={:>9.3} train_obj={:>8.3} val_obj={:>8.3} train_pnl=${:>8.2}/{} val_pnl=${:>8.2}/{} val_dd=${:>7.2} val_entry={:.3} val_rr={:.2} val_pos_sym={:>5.1}% | dir_prob={:.3} dist_sigma={:.3} conf={:.3} drift={:.5} edge={:.3} rr={:.2} score={:.3} alpha_contra={} cex_contra={} cd={}s time={}..{}",
                    trial.id(),
                    strategy_profile.as_str(),
                    objective,
                    train_metrics.objective,
                    val_metrics.objective,
                    train_metrics.net_pnl,
                    train_metrics.trades,
                    val_metrics.net_pnl,
                    val_metrics.trades,
                    val_metrics.max_drawdown,
                    val_metrics.avg_entry_price,
                    val_metrics.avg_reward_risk,
                    val_metrics.positive_symbol_rate * 100.0,
                    params.min_direction_prob,
                    params.min_distance_over_sigma,
                    params.min_confirmation_score,
                    params.min_drift_confirmation,
                    params.min_edge,
                    params.min_reward_risk,
                    params.min_entry_score,
                    params.alpha_contrarian,
                    params.cex_contrarian,
                    params.cooldown_secs,
                    params.min_time_remaining_secs,
                    params.max_time_remaining_secs,
                );
                Ok::<f64, Error>(objective)
            })
            .context("snapshot TPE optimization failed")?;
    }

    let best = study
        .best_trial()
        .context("no completed optimizer trials")?;
    let best_min_time_remaining_secs = best.get(&p_min_time_remaining_secs).unwrap_or(30);
    let best_params = SnapshotThreeLayerParams {
        min_direction_prob: best.get(&p_min_direction_prob).unwrap_or(0.52),
        min_distance_over_sigma: best.get(&p_min_distance_over_sigma).unwrap_or(0.10),
        min_confirmation_score: strategy_profile
            .fixes_confirmation_threshold()
            .unwrap_or_else(|| best.get(&p_min_confirmation_score).unwrap_or(0.05)),
        min_drift_confirmation: best.get(&p_min_drift_confirmation).unwrap_or(0.0001),
        min_edge: best.get(&p_min_edge).unwrap_or(0.02),
        min_reward_risk: best.get(&p_min_reward_risk).unwrap_or(0.8),
        min_entry_score: best.get(&p_min_entry_score).unwrap_or(0.10),
        alpha_contrarian: strategy_profile
            .fixes_alpha_contrarian()
            .unwrap_or_else(|| best.get(&p_alpha_contrarian).unwrap_or(false)),
        cex_contrarian: strategy_profile
            .fixes_cex_contrarian()
            .unwrap_or_else(|| best.get(&p_cex_contrarian).unwrap_or(false)),
        cooldown_secs: best.get(&p_cooldown_secs).unwrap_or(15),
        min_time_remaining_secs: best_min_time_remaining_secs,
        max_time_remaining_secs: best_min_time_remaining_secs
            + best.get(&p_max_time_span_secs).unwrap_or(60),
    };

    let train_metrics = evaluate_snapshot_objective(
        &train_rows,
        &best_params,
        min_trades,
        stake_usd,
        strategy_profile,
    );
    let val_metrics = evaluate_snapshot_objective(
        &val_rows,
        &best_params,
        min_trades,
        stake_usd,
        strategy_profile,
    );
    let selection_objective =
        holistic_selection_objective(&train_metrics, &val_metrics, min_trades);
    let train_underpowered = train_metrics.trades < min_trades;
    let validation_underpowered = val_metrics.trades < min_trades;

    eprintln!("\n=== Best Parameters (Training) ===");
    eprintln!(
        "Strategy profile:                 {}",
        strategy_profile.as_str()
    );
    eprintln!("Objective:                       {:.3}", best.value);
    eprintln!(
        "three_layer_min_direction_prob = {:.6}",
        best_params.min_direction_prob
    );
    eprintln!(
        "three_layer_min_distance_over_sigma = {:.6}",
        best_params.min_distance_over_sigma
    );
    eprintln!(
        "three_layer_min_confirmation_score = {:.6}",
        best_params.min_confirmation_score
    );
    eprintln!(
        "three_layer_min_drift_confirmation = {:.8}",
        best_params.min_drift_confirmation
    );
    eprintln!("three_layer_min_edge = {:.6}", best_params.min_edge);
    eprintln!(
        "three_layer_min_reward_risk = {:.6}",
        best_params.min_reward_risk
    );
    eprintln!(
        "three_layer_min_entry_score = {:.6}",
        best_params.min_entry_score
    );
    eprintln!("alpha_contrarian = {}", best_params.alpha_contrarian);
    eprintln!("cex_contrarian = {}", best_params.cex_contrarian);
    eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
    eprintln!(
        "min_time_remaining_secs = {}",
        best_params.min_time_remaining_secs
    );
    eprintln!(
        "max_time_remaining_secs = {}",
        best_params.max_time_remaining_secs
    );
    eprintln!("\n=== Snapshot Objective Metrics ===");
    eprintln!("Selection objective: {:.3}", selection_objective);
    print_metrics("Train", &train_metrics);
    print_metrics("Validation", &val_metrics);
    if train_underpowered {
        eprintln!(
            "WARNING: best training result has {} trades below min_trades={}; do not use this parameter set.",
            train_metrics.trades, min_trades
        );
    }
    if validation_underpowered {
        eprintln!(
            "WARNING: validation result has {} trades below min_trades={}; do not use this parameter set.",
            val_metrics.trades, min_trades
        );
    }

    eprintln!("\n=== Config Snippet ===");
    eprintln!("# Paste into [strategy] only after walk-forward and dry-run/live parity review.");
    eprintln!("# strategy_profile = {}", strategy_profile.as_str());
    eprintln!(
        "three_layer_min_direction_prob = {:.6}",
        best_params.min_direction_prob
    );
    eprintln!(
        "three_layer_min_distance_over_sigma = {:.6}",
        best_params.min_distance_over_sigma
    );
    eprintln!(
        "three_layer_min_confirmation_score = {:.6}",
        best_params.min_confirmation_score
    );
    eprintln!(
        "three_layer_min_drift_confirmation = {:.8}",
        best_params.min_drift_confirmation
    );
    eprintln!("three_layer_min_edge = {:.6}", best_params.min_edge);
    eprintln!(
        "three_layer_min_reward_risk = {:.6}",
        best_params.min_reward_risk
    );
    eprintln!(
        "three_layer_min_entry_score = {:.6}",
        best_params.min_entry_score
    );
    eprintln!(
        "three_layer_alpha_contrarian = {}",
        best_params.alpha_contrarian
    );
    eprintln!(
        "three_layer_cex_contrarian = {}",
        best_params.cex_contrarian
    );
    eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
    eprintln!(
        "min_time_remaining_secs = {}",
        best_params.min_time_remaining_secs
    );
    eprintln!(
        "max_time_remaining_secs = {}",
        best_params.max_time_remaining_secs
    );
    eprintln!("# stop-loss/take-profit path exits require a future path replay label.");

    let trials = study.trials();
    let summary = OptimizeSummary {
        snapshot_schema: &snapshot.manifest.schema_version,
        snapshot_hash,
        snapshot_generated_at: snapshot.manifest.generated_at,
        snapshot_data_requirements: &snapshot.manifest.data_requirements,
        snapshot_data_audit_status: snapshot.manifest.data_audit_status.as_deref(),
        snapshot_data_audit_report: snapshot.manifest.data_audit_report.as_deref(),
        snapshot_include_deribit: snapshot.manifest.include_deribit,
        symbols: &symbols,
        train_start,
        train_end,
        val_start,
        val_end,
        strategy_profile,
        trials: n_trials,
        stake_usd,
        selection_objective,
        min_trades,
        min_trades_source,
        train_opportunities,
        val_opportunities,
        train_rows: train_rows.len(),
        val_rows: val_rows.len(),
        train_underpowered,
        validation_underpowered,
        best_params,
        train_metrics,
        val_metrics,
    };
    write_outputs(output_dir, &summary, &trials)?;
    if train_underpowered || validation_underpowered {
        anyhow::bail!(
            "snapshot optimizer result is underpowered: train_trades={} validation_trades={} min_trades={}",
            summary.train_metrics.trades,
            summary.val_metrics.trades,
            summary.min_trades
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        SnapshotObjectiveMetrics, SnapshotThreeLayerParams, StableObjectiveInputs, StrategyProfile,
        compounded_log_growth, default_min_trades_from_coverage, directional_score,
        executable_edge_score, holistic_selection_objective, max_drawdown, parse_date_end,
        reward_risk_ratio, sample_power_multiplier, stable_compounding_objective, trade_sharpe,
        transformed_model_probability,
    };

    fn test_params() -> SnapshotThreeLayerParams {
        SnapshotThreeLayerParams {
            min_direction_prob: 0.55,
            min_distance_over_sigma: 0.0,
            min_confirmation_score: 0.0,
            min_drift_confirmation: 0.0,
            min_edge: 0.0,
            min_reward_risk: 0.5,
            min_entry_score: 0.1,
            alpha_contrarian: false,
            cex_contrarian: false,
            cooldown_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 180,
        }
    }

    fn metrics_for_selection(
        trades: usize,
        net_pnl: f64,
        objective: f64,
    ) -> SnapshotObjectiveMetrics {
        SnapshotObjectiveMetrics {
            candidates: trades,
            selected: trades,
            trades,
            rejected_duplicate: 0,
            rejected_cooldown: 0,
            rejected_non_executable: 0,
            net_pnl,
            avg_pnl: if trades == 0 {
                f64::NAN
            } else {
                net_pnl / trades as f64
            },
            avg_entry_price: 0.42,
            avg_reward_risk: 1.35,
            sharpe: 2.0,
            max_drawdown: 60.0,
            log_growth: 8.0,
            avg_log_return: if trades == 0 {
                f64::NAN
            } else {
                8.0 / trades as f64
            },
            positive_day_rate: 1.0,
            positive_symbol_rate: 0.8,
            concentration: 0.35,
            reject_rate: 0.0,
            objective,
            fill_rate: 1.0,
            win_rate: 0.45,
        }
    }

    #[test]
    fn date_end_is_exclusive_next_day() {
        assert_eq!(
            parse_date_end("2026-04-27").to_rfc3339(),
            "2026-04-28T00:00:00+00:00"
        );
    }

    #[test]
    fn reward_risk_rejects_invalid_prices() {
        assert!(reward_risk_ratio(0.0).is_nan());
        assert!(reward_risk_ratio(1.0).is_nan());
        assert!(reward_risk_ratio(0.25).is_finite());
    }

    #[test]
    fn trade_sharpe_penalizes_empty_trials() {
        assert_eq!(trade_sharpe(&[]), -100.0);
        assert!(trade_sharpe(&[1.0, 2.0, 3.0]).is_finite());
    }

    #[test]
    fn directional_score_supports_normal_and_contrarian_search() {
        assert!(directional_score(0.62, 0.55, 0.25, false) > 0.0);
        assert!(directional_score(0.48, 0.55, 0.25, true) > 0.0);
        assert!(directional_score(0.48, 0.55, 0.25, false) < 0.0);
        assert_eq!(directional_score(f64::NAN, 0.55, 0.25, false), -0.50);
    }

    #[test]
    fn contrarian_probability_transforms_before_edge_scoring() {
        assert!((transformed_model_probability(0.22, true) - 0.78).abs() < 1e-9);
        assert!((transformed_model_probability(0.78, false) - 0.78).abs() < 1e-9);
    }

    #[test]
    fn executable_edge_score_never_rewards_negative_edge() {
        let mut params = test_params();
        params.alpha_contrarian = true;
        params.min_edge = -0.02;

        assert!(
            executable_edge_score(-0.001, &params) < 0.0,
            "negative edge should remain a penalty even in contrarian mode"
        );
        assert!(executable_edge_score(0.04, &params) > 0.0);
    }

    #[test]
    fn stable_objective_prefers_smooth_compounding_over_choppy_sharpe() {
        let smooth = stable_compounding_objective(StableObjectiveInputs {
            trades: 800,
            min_trades: 400,
            stake_usd: 15.0,
            net_pnl: 900.0,
            sharpe: 2.0,
            max_drawdown: 60.0,
            log_growth: 12.0,
            avg_entry_price: 0.42,
            avg_reward_risk: 1.35,
            fill_rate: 0.95,
            reject_rate: 0.02,
            positive_day_rate: 1.0,
            positive_symbol_rate: 1.0,
            concentration: 0.25,
        });
        let choppy = stable_compounding_objective(StableObjectiveInputs {
            trades: 800,
            min_trades: 400,
            stake_usd: 15.0,
            net_pnl: 900.0,
            sharpe: 4.5,
            max_drawdown: 450.0,
            log_growth: 2.0,
            avg_entry_price: 0.72,
            avg_reward_risk: 0.35,
            fill_rate: 0.70,
            reject_rate: 0.25,
            positive_day_rate: 0.50,
            positive_symbol_rate: 0.50,
            concentration: 0.80,
        });

        assert!(smooth > choppy);
    }

    #[test]
    fn drawdown_and_log_growth_capture_compounding_risk() {
        assert_eq!(max_drawdown(&[10.0, -5.0, -20.0, 15.0]), 25.0);
        assert!(compounded_log_growth(&[1.0, 1.0, 1.0], 15.0) > 0.0);
        assert!(compounded_log_growth(&[-15.0], 15.0) < -4.0);
    }

    #[test]
    fn default_min_trades_scales_with_event_coverage_not_rows() {
        assert_eq!(default_min_trades_from_coverage(10_368, 3, 6), 156);
        assert_eq!(default_min_trades_from_coverage(1_000, 1, 2), 40);
        assert_eq!(default_min_trades_from_coverage(200_000, 7, 6), 1_500);
    }

    #[test]
    fn holistic_selection_requires_powered_train_and_validation_windows() {
        let train = metrics_for_selection(300, 900.0, 10.0);
        let sparse_validation = metrics_for_selection(39, 300.0, 8.0);
        let powered_validation = metrics_for_selection(160, 300.0, 8.0);

        assert!(
            holistic_selection_objective(&train, &sparse_validation, 40) < -999_000.0,
            "sparse validation should remain rejected even when profitable"
        );
        assert!(holistic_selection_objective(&train, &powered_validation, 40) > 0.0);
    }

    #[test]
    fn holistic_selection_rejects_non_profitable_validation() {
        let train = metrics_for_selection(300, 900.0, 10.0);
        let validation_loss = metrics_for_selection(160, -50.0, 8.0);

        assert!(holistic_selection_objective(&train, &validation_loss, 40) < -9_000.0);
    }

    #[test]
    fn sample_power_multiplier_penalizes_threshold_hugging() {
        assert_eq!(sample_power_multiplier(499, 500), 0.0);
        assert!(sample_power_multiplier(500, 500) < 0.6);
        assert_eq!(sample_power_multiplier(2_000, 500), 1.0);
        assert_eq!(sample_power_multiplier(3_000, 500), 1.0);
    }

    #[test]
    fn strategy_profile_parses_operator_aliases() {
        assert_eq!(
            StrategyProfile::parse("mixed").unwrap(),
            StrategyProfile::Mixed
        );
        assert_eq!(
            StrategyProfile::parse("A").unwrap(),
            StrategyProfile::Champion
        );
        assert_eq!(
            StrategyProfile::parse("obi").unwrap(),
            StrategyProfile::ObiSoft
        );
        assert_eq!(
            StrategyProfile::parse("cex_continuation").unwrap(),
            StrategyProfile::ContinuationSoft
        );
        assert!(StrategyProfile::parse("unknown").is_err());
    }

    #[test]
    fn three_arm_profiles_share_contrarian_alpha_base() {
        assert_eq!(
            StrategyProfile::Champion.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::ObiSoft.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::ContinuationSoft.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(StrategyProfile::Mixed.fixes_alpha_contrarian(), None);
        assert_eq!(
            StrategyProfile::Champion.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::Champion.fixes_confirmation_threshold(),
            Some(0.0)
        );
    }
}
