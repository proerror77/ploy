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
    build_data_health_report, build_factor_observations_v2_with_deribit_and_pm_books,
    load_research_snapshot, FactorObservationV2, FactorReviewOptions, ResearchSnapshotManifest,
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
    sharpe: f64,
    objective: f64,
    fill_rate: f64,
    win_rate: f64,
}

#[derive(Debug, Serialize)]
struct OptimizeSummary<'a> {
    snapshot_schema: &'a str,
    snapshot_hash: &'a str,
    snapshot_generated_at: DateTime<Utc>,
    symbols: &'a [String],
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    trials: usize,
    stake_usd: f64,
    min_trades: usize,
    min_trades_source: &'a str,
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
    if value.is_finite() {
        value
    } else {
        0.0
    }
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
    if risk <= 0.0 {
        f64::NAN
    } else {
        reward / risk
    }
}

fn confirmation_score(row: &FactorObservationV2) -> f64 {
    let side = row.side.multiplier();
    let continuation = finite_or_zero(row.cex_continuation_score_side).clamp(-1.0, 1.0);
    let obi = finite_or_zero(row.obi_10 * side).clamp(-1.0, 1.0);
    let depth = finite_or_zero(row.depth_imbalance * side).clamp(-1.0, 1.0);
    let microprice = finite_or_zero(row.microprice_offset_bps * side / 10.0).clamp(-1.0, 1.0);
    0.45 * continuation + 0.25 * obi + 0.15 * depth + 0.15 * microprice
}

fn three_layer_entry_score(
    row: &FactorObservationV2,
    edge: f64,
    confirmation: f64,
    params: &SnapshotThreeLayerParams,
) -> f64 {
    let direction_score = directional_score(
        row.side_model_prob,
        params.min_direction_prob,
        0.25,
        params.alpha_contrarian,
    );
    let distance_score = directional_score(
        row.side_distance_over_sigma,
        params.min_distance_over_sigma,
        0.60,
        params.alpha_contrarian,
    );
    let edge_score = directional_score(edge, params.min_edge, 0.08, params.alpha_contrarian);
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
    0.25 * direction_score
        + 0.12 * distance_score
        + 0.18 * edge_score
        + 0.15 * confirmation_score
        + 0.10 * drift_score
        + 0.12 * pm_momentum_score
        + 0.08 * liquidity_score
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

fn row_passes_gates(row: &FactorObservationV2, params: &SnapshotThreeLayerParams) -> bool {
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

    let edge = if row.side_model_edge.is_finite() {
        row.side_model_edge
    } else {
        row.side_model_prob - row.entry_ask - crypto_fee_cost(row.entry_ask)
    };
    if !edge.is_finite() {
        return false;
    }
    let reward_risk = reward_risk_ratio(row.entry_ask);
    if !reward_risk.is_finite() || reward_risk < params.min_reward_risk {
        return false;
    }
    let confirmation = confirmation_score(row);
    if !confirmation.is_finite() {
        return false;
    }
    three_layer_entry_score(row, edge, confirmation, params) >= params.min_entry_score
}

fn evaluate_snapshot_objective(
    rows: &[FactorObservationV2],
    params: &SnapshotThreeLayerParams,
    min_trades: usize,
) -> SnapshotObjectiveMetrics {
    let mut last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut traded_event_sides: HashSet<String> = HashSet::new();
    let mut pnls = Vec::new();
    let mut candidates = 0usize;
    let mut selected = 0usize;
    let mut rejected_duplicate = 0usize;
    let mut rejected_cooldown = 0usize;
    let mut rejected_non_executable = 0usize;

    for row in rows {
        if !row_passes_gates(row, params) {
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

        pnls.push(pnl);
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
    let sharpe = trade_sharpe(&pnls);
    let fill_rate = ratio(trades, selected);
    let win_rate = if trades == 0 {
        f64::NAN
    } else {
        ratio(pnls.iter().filter(|pnl| **pnl > 0.0).count(), trades)
    };
    let objective = if trades < min_trades {
        -1_000_000.0 + trades as f64
    } else {
        (sharpe * sample_power_multiplier(trades, min_trades)) + (net_pnl / 100_000.0)
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
        sharpe,
        objective,
        fill_rate,
        win_rate,
    }
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

fn default_min_trades(train_rows: usize, val_rows: usize) -> usize {
    let base_rows = train_rows.min(val_rows);
    (base_rows / 200).clamp(500, 5_000)
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
         # validation_sharpe = {:.6}\n\
         # validation_pnl = {:.6}\n\
         # validation_trades = {}\n\
         # min_trades = {}\n\
         # min_trades_source = {}\n\
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
        summary.val_metrics.sharpe,
        summary.val_metrics.net_pnl,
        summary.val_metrics.trades,
        summary.min_trades,
        summary.min_trades_source,
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
        "{label}: objective={:.3} sharpe={:.3} pnl=${:.2} trades={} candidates={} selected={} fill_rate={:.2}% win_rate={:.2}% non_exec={} dup={} cooldown={}",
        metrics.objective,
        metrics.sharpe,
        metrics.net_pnl,
        metrics.trades,
        metrics.candidates,
        metrics.selected,
        metrics.fill_rate * 100.0,
        metrics.win_rate * 100.0,
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
    let min_trades =
        min_trades_override.unwrap_or_else(|| default_min_trades(train_rows.len(), val_rows.len()));
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
    eprintln!("Trade floor: min_trades={min_trades} source={min_trades_source}");

    let train_rows = Arc::new(train_rows);
    let study: Study<f64> = Study::maximize(TpeSampler::new());
    let p_min_direction_prob = FloatParam::new(0.50, 0.68).name("three_layer_min_direction_prob");
    let p_min_distance_over_sigma =
        FloatParam::new(-0.20, 0.60).name("three_layer_min_distance_over_sigma");
    let p_min_confirmation_score =
        FloatParam::new(-0.15, 0.25).name("three_layer_min_confirmation_score");
    let p_min_drift_confirmation =
        FloatParam::new(-0.0005, 0.0008).name("three_layer_min_drift_confirmation");
    let p_min_edge = FloatParam::new(-0.02, 0.05).name("three_layer_min_edge");
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

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let min_time_remaining_secs = p_min_time_remaining_secs_c.suggest(trial)?;
                let params = SnapshotThreeLayerParams {
                    min_direction_prob: p_min_direction_prob_c.suggest(trial)?,
                    min_distance_over_sigma: p_min_distance_over_sigma_c.suggest(trial)?,
                    min_confirmation_score: p_min_confirmation_score_c.suggest(trial)?,
                    min_drift_confirmation: p_min_drift_confirmation_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    min_reward_risk: p_min_reward_risk_c.suggest(trial)?,
                    min_entry_score: p_min_entry_score_c.suggest(trial)?,
                    alpha_contrarian: p_alpha_contrarian_c.suggest(trial)?,
                    cex_contrarian: p_cex_contrarian_c.suggest(trial)?,
                    cooldown_secs: p_cooldown_secs_c.suggest(trial)?,
                    min_time_remaining_secs,
                    max_time_remaining_secs: min_time_remaining_secs
                        + p_max_time_span_secs_c.suggest(trial)?,
                };

                let metrics = evaluate_snapshot_objective(&train_rows, &params, min_trades);
                eprintln!(
                    "  Trial {:>3}: source=snapshot-observation objective={:>9.3} sharpe={:>7.3} pnl=${:>8.2} trades={:>4} selected={:>5} fill={:>5.1}% | dir_prob={:.3} dist_sigma={:.3} conf={:.3} drift={:.5} edge={:.3} rr={:.2} score={:.3} alpha_contra={} cex_contra={} cd={}s time={}..{}",
                    trial.id(),
                    metrics.objective,
                    metrics.sharpe,
                    metrics.net_pnl,
                    metrics.trades,
                    metrics.selected,
                    metrics.fill_rate * 100.0,
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
                Ok::<f64, Error>(metrics.objective)
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
        min_confirmation_score: best.get(&p_min_confirmation_score).unwrap_or(0.05),
        min_drift_confirmation: best.get(&p_min_drift_confirmation).unwrap_or(0.0001),
        min_edge: best.get(&p_min_edge).unwrap_or(0.02),
        min_reward_risk: best.get(&p_min_reward_risk).unwrap_or(0.8),
        min_entry_score: best.get(&p_min_entry_score).unwrap_or(0.10),
        alpha_contrarian: best.get(&p_alpha_contrarian).unwrap_or(false),
        cex_contrarian: best.get(&p_cex_contrarian).unwrap_or(false),
        cooldown_secs: best.get(&p_cooldown_secs).unwrap_or(15),
        min_time_remaining_secs: best_min_time_remaining_secs,
        max_time_remaining_secs: best_min_time_remaining_secs
            + best.get(&p_max_time_span_secs).unwrap_or(60),
    };

    let train_metrics = evaluate_snapshot_objective(&train_rows, &best_params, min_trades);
    let val_metrics = evaluate_snapshot_objective(&val_rows, &best_params, min_trades);
    let train_underpowered = train_metrics.trades < min_trades;
    let validation_underpowered = val_metrics.trades < min_trades;

    eprintln!("\n=== Best Parameters (Training) ===");
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
        symbols: &symbols,
        train_start,
        train_end,
        val_start,
        val_end,
        trials: n_trials,
        stake_usd,
        min_trades,
        min_trades_source,
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
        default_min_trades, directional_score, parse_date_end, reward_risk_ratio,
        sample_power_multiplier, trade_sharpe,
    };

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
    fn default_min_trades_scales_with_snapshot_size() {
        assert_eq!(default_min_trades(185_158, 107_774), 538);
        assert_eq!(default_min_trades(1_000, 1_000), 500);
        assert_eq!(default_min_trades(2_000_000, 2_000_000), 5_000);
    }

    #[test]
    fn sample_power_multiplier_penalizes_threshold_hugging() {
        assert_eq!(sample_power_multiplier(499, 500), 0.0);
        assert!(sample_power_multiplier(500, 500) < 0.6);
        assert_eq!(sample_power_multiplier(2_000, 500), 1.0);
        assert_eq!(sample_power_multiplier(3_000, 500), 1.0);
    }
}
