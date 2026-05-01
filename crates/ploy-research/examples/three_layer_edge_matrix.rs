//! three_layer_edge_matrix - deterministic PM5D edge matrix over a research snapshot.
//!
//! This runner is intentionally not an optimizer. It evaluates a fixed matrix of
//! low-degree hypotheses and records gate attrition plus executable outcomes so
//! research can identify where an apparent edge disappears.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::{
    FactorObservationV2, FactorReviewOptions, build_data_health_report,
    build_factor_observations_v2_with_deribit_and_pm_books, load_research_snapshot,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DirectionMode {
    Model,
    Inverted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FillMode {
    FullDepthRoundTrip,
    ExecutableRoundTrip,
    EntryOnlyExecutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PmMode {
    None,
    SoftDynamics,
    PositiveExitBid,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct MatrixHypothesis {
    name: &'static str,
    direction_mode: DirectionMode,
    fill_mode: FillMode,
    pm_mode: PmMode,
    min_direction_prob: f64,
    min_ev_per_stake: f64,
    min_reward_risk: f64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
    probability_shrink: f64,
    probability_haircut: f64,
    cooldown_secs: i64,
}

#[derive(Debug, Clone, Serialize)]
struct GateRow {
    hypothesis: String,
    split: String,
    gate_index: usize,
    gate: String,
    rows: usize,
    event_sides: usize,
    executable_pnl_rows: usize,
    full_depth_pnl_rows: usize,
    entry_fill_rate: f64,
    roundtrip_fill_rate: f64,
    total_executable_pnl: f64,
    avg_executable_pnl: f64,
}

#[derive(Debug, Clone, Serialize)]
struct MatrixResult {
    hypothesis: String,
    split: String,
    direction_mode: DirectionMode,
    fill_mode: FillMode,
    pm_mode: PmMode,
    rows_after_gates: usize,
    event_sides_after_gates: usize,
    selected: usize,
    trades: usize,
    rejected_duplicate: usize,
    rejected_cooldown: usize,
    rejected_non_executable: usize,
    net_pnl: f64,
    avg_pnl: f64,
    fill_rate: f64,
    win_rate: f64,
    avg_entry_price: f64,
    avg_expected_value_per_stake: f64,
    avg_realized_return_per_stake: f64,
    expectancy_calibration_gap: f64,
    positive_day_rate: f64,
    positive_symbol_rate: f64,
    min_trades: usize,
    underpowered: bool,
    deployable_candidate: bool,
}

#[derive(Debug, Serialize)]
struct MatrixSummary {
    snapshot_hash: String,
    snapshot_generated_at: DateTime<Utc>,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    symbols: Vec<String>,
    min_trades: usize,
    source_rows: usize,
    v2_rows: usize,
    hypothesis_count: usize,
    gate_rows: Vec<GateRow>,
    results: Vec<MatrixResult>,
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
    parse_date_start(raw) + chrono::Duration::days(1)
}

fn parse_window(args: &[String], day_flag: &str, ts_flag: &str, end_of_day: bool) -> DateTime<Utc> {
    if let Some(raw) = flag_value(args, ts_flag) {
        return DateTime::parse_from_rfc3339(&raw)
            .unwrap_or_else(|_| panic!("invalid timestamp for {ts_flag}: {raw}"))
            .with_timezone(&Utc);
    }
    let raw = flag_value(args, day_flag).unwrap_or_else(|| panic!("{day_flag} is required"));
    if end_of_day {
        parse_date_end(&raw)
    } else {
        parse_date_start(&raw)
    }
}

fn slice_by_time(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<FactorObservationV2> {
    rows.iter()
        .filter(|row| row.tick_ts >= start && row.tick_ts < end)
        .cloned()
        .collect()
}

fn transformed_probability(row: &FactorObservationV2, mode: DirectionMode) -> f64 {
    match mode {
        DirectionMode::Model => row.side_model_prob,
        DirectionMode::Inverted => 1.0 - row.side_model_prob,
    }
}

fn calibrated_probability(row: &FactorObservationV2, h: MatrixHypothesis) -> f64 {
    let raw = transformed_probability(row, h.direction_mode);
    if !raw.is_finite() {
        return f64::NAN;
    }
    (0.5 + (raw - 0.5) * h.probability_shrink - h.probability_haircut).clamp(0.01, 0.99)
}

fn fee_cost(entry_price: f64) -> f64 {
    0.02 * entry_price * (1.0 - entry_price)
}

fn expected_value_per_share(probability: f64, entry_price: f64) -> f64 {
    if !probability.is_finite()
        || !entry_price.is_finite()
        || !(0.0..=1.0).contains(&probability)
        || !(0.0..1.0).contains(&entry_price)
    {
        return f64::NAN;
    }
    let fee = fee_cost(entry_price);
    probability * (1.0 - entry_price - fee) - (1.0 - probability) * (entry_price + fee)
}

fn expected_value_per_stake(probability: f64, entry_price: f64) -> f64 {
    let ev = expected_value_per_share(probability, entry_price);
    if !ev.is_finite() || !entry_price.is_finite() || entry_price <= 0.0 {
        return f64::NAN;
    }
    ev / entry_price
}

fn reward_risk_ratio(entry_price: f64) -> f64 {
    if !entry_price.is_finite() || entry_price <= 0.0 || entry_price >= 1.0 {
        return f64::NAN;
    }
    let fee = fee_cost(entry_price);
    let reward = 1.0 - entry_price - fee;
    let risk = entry_price + fee;
    if risk <= 0.0 { f64::NAN } else { reward / risk }
}

fn executable_pnl(row: &FactorObservationV2) -> Option<f64> {
    row.label_full_depth_executable_pnl_15u
        .or(row.label_executable_pnl_15u)
        .filter(|pnl| pnl.is_finite())
}

fn full_depth_roundtrip_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable && row.label_full_depth_exit_fillable
}

fn executable_roundtrip_fillable(row: &FactorObservationV2) -> bool {
    full_depth_roundtrip_fillable(row) || (row.label_executable_fillable && row.label_exit_fillable)
}

fn entry_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable || row.label_executable_fillable
}

fn fill_mode_passes(row: &FactorObservationV2, mode: FillMode) -> bool {
    match mode {
        FillMode::FullDepthRoundTrip => full_depth_roundtrip_fillable(row),
        FillMode::ExecutableRoundTrip => executable_roundtrip_fillable(row),
        FillMode::EntryOnlyExecutable => entry_fillable(row),
    }
}

fn pm_dynamics_score(row: &FactorObservationV2) -> f64 {
    let exit_bid = finite_or_zero(row.exit_bid_change_30s / 0.08).clamp(-1.0, 1.0);
    let entry_ask = finite_or_zero(row.entry_ask_change_30s / 0.08).clamp(-1.0, 1.0);
    let reprice = finite_or_zero(row.pm_reprice_speed_30s * 30.0 / 0.08).clamp(-1.0, 1.0);
    let cex_edge = finite_or_zero(row.cex_continuation_edge_gate / 0.08).clamp(-1.0, 1.0);
    0.42 * exit_bid.max(entry_ask).max(reprice) + 0.23 * exit_bid + 0.20 * reprice + 0.15 * cex_edge
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn pm_mode_passes(row: &FactorObservationV2, mode: PmMode) -> bool {
    match mode {
        PmMode::None => true,
        PmMode::SoftDynamics => pm_dynamics_score(row) > -0.25,
        PmMode::PositiveExitBid => {
            row.exit_bid_change_30s.is_finite() && row.exit_bid_change_30s > 0.0
        }
    }
}

fn event_side_count(rows: &[&FactorObservationV2]) -> usize {
    rows.iter()
        .map(|row| format!("{}:{}", row.event_id, row.side.as_str()))
        .collect::<HashSet<_>>()
        .len()
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn positive_bucket_rate(map: &HashMap<String, f64>) -> f64 {
    if map.is_empty() {
        return 0.0;
    }
    ratio(
        map.values().filter(|value| **value > 0.0).count(),
        map.len(),
    )
}

fn gate_row(
    h: MatrixHypothesis,
    split: &str,
    gate_index: usize,
    gate: &str,
    rows: &[&FactorObservationV2],
) -> GateRow {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    GateRow {
        hypothesis: h.name.to_string(),
        split: split.to_string(),
        gate_index,
        gate: gate.to_string(),
        rows: rows.len(),
        event_sides: event_side_count(rows),
        executable_pnl_rows: pnl_values.len(),
        full_depth_pnl_rows: rows
            .iter()
            .filter(|row| {
                row.label_full_depth_executable_pnl_15u
                    .is_some_and(f64::is_finite)
            })
            .count(),
        entry_fill_rate: ratio(
            rows.iter().filter(|row| entry_fillable(row)).count(),
            rows.len(),
        ),
        roundtrip_fill_rate: ratio(
            rows.iter()
                .filter(|row| executable_roundtrip_fillable(row))
                .count(),
            rows.len(),
        ),
        total_executable_pnl: total_pnl,
        avg_executable_pnl: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
    }
}

fn apply_gate<'a, F>(
    rows: Vec<&'a FactorObservationV2>,
    predicate: F,
) -> Vec<&'a FactorObservationV2>
where
    F: Fn(&FactorObservationV2) -> bool,
{
    rows.into_iter().filter(|row| predicate(row)).collect()
}

fn evaluate_hypothesis(
    rows: &[FactorObservationV2],
    h: MatrixHypothesis,
    split: &str,
    min_trades: usize,
    gate_rows: &mut Vec<GateRow>,
) -> MatrixResult {
    let mut current = rows.iter().collect::<Vec<_>>();
    gate_rows.push(gate_row(h, split, 0, "base", &current));

    current = apply_gate(current, |row| {
        row.time_remaining_secs >= h.min_time_remaining_secs
            && row.time_remaining_secs <= h.max_time_remaining_secs
            && row.entry_ask.is_finite()
            && (0.10..=0.85).contains(&row.entry_ask)
            && row.pm_lag_secs.is_finite()
            && (0.0..=15.0).contains(&row.pm_lag_secs)
    });
    gate_rows.push(gate_row(h, split, 1, "time_price_lag", &current));

    current = apply_gate(current, |row| {
        let p = transformed_probability(row, h.direction_mode);
        p.is_finite() && p >= h.min_direction_prob && row.side_distance_over_sigma.is_finite()
    });
    gate_rows.push(gate_row(h, split, 2, "direction_probability", &current));

    current = apply_gate(current, |row| {
        let p = calibrated_probability(row, h);
        let ev = expected_value_per_share(p, row.entry_ask);
        let ev_stake = expected_value_per_stake(p, row.entry_ask);
        ev.is_finite() && ev > 0.0 && ev_stake.is_finite() && ev_stake >= h.min_ev_per_stake
    });
    gate_rows.push(gate_row(h, split, 3, "ev_per_stake", &current));

    current = apply_gate(current, |row| fill_mode_passes(row, h.fill_mode));
    gate_rows.push(gate_row(h, split, 4, "fillability", &current));

    current = apply_gate(current, |row| pm_mode_passes(row, h.pm_mode));
    gate_rows.push(gate_row(h, split, 5, "pm_dynamics", &current));

    current = apply_gate(current, |row| {
        let rr = reward_risk_ratio(row.entry_ask);
        rr.is_finite() && rr >= h.min_reward_risk
    });
    gate_rows.push(gate_row(h, split, 6, "reward_risk", &current));

    selected_metrics(&current, h, split, min_trades)
}

fn selected_metrics(
    rows: &[&FactorObservationV2],
    h: MatrixHypothesis,
    split: &str,
    min_trades: usize,
) -> MatrixResult {
    let mut last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut traded_event_sides: HashSet<String> = HashSet::new();
    let mut selected = 0usize;
    let mut rejected_duplicate = 0usize;
    let mut rejected_cooldown = 0usize;
    let mut rejected_non_executable = 0usize;
    let mut pnls = Vec::new();
    let mut entry_sum = 0.0;
    let mut ev_stake_sum = 0.0;
    let mut pnl_by_day: HashMap<String, f64> = HashMap::new();
    let mut pnl_by_symbol: HashMap<String, f64> = HashMap::new();

    for row in rows {
        selected += 1;
        let event_side_key = format!("{}:{}", row.event_id, row.side.as_str());
        if traded_event_sides.contains(&event_side_key) {
            rejected_duplicate += 1;
            continue;
        }
        if let Some(last_ts) = last_trade_by_symbol.get(&row.symbol) {
            if (row.tick_ts - *last_ts).num_seconds() < h.cooldown_secs {
                rejected_cooldown += 1;
                continue;
            }
        }
        let Some(pnl) = executable_pnl(row) else {
            rejected_non_executable += 1;
            continue;
        };
        traded_event_sides.insert(event_side_key);
        last_trade_by_symbol.insert(row.symbol.clone(), row.tick_ts);
        pnls.push(pnl);
        entry_sum += row.entry_ask;
        ev_stake_sum += expected_value_per_stake(calibrated_probability(row, h), row.entry_ask);
        *pnl_by_day
            .entry(row.tick_ts.date_naive().to_string())
            .or_default() += pnl;
        *pnl_by_symbol.entry(row.symbol.clone()).or_default() += pnl;
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
        entry_sum / trades as f64
    };
    let avg_expected_value_per_stake = if trades == 0 {
        f64::NAN
    } else {
        ev_stake_sum / trades as f64
    };
    let avg_realized_return_per_stake = if trades == 0 {
        f64::NAN
    } else {
        net_pnl / 15.0 / trades as f64
    };
    let expectancy_calibration_gap =
        if avg_expected_value_per_stake.is_finite() && avg_realized_return_per_stake.is_finite() {
            (avg_expected_value_per_stake - avg_realized_return_per_stake).max(0.0)
        } else {
            f64::NAN
        };
    let fill_rate = ratio(trades, selected);
    let win_rate = ratio(pnls.iter().filter(|pnl| **pnl > 0.0).count(), trades);
    let positive_day_rate = positive_bucket_rate(&pnl_by_day);
    let positive_symbol_rate = positive_bucket_rate(&pnl_by_symbol);
    let underpowered = trades < min_trades;
    let deployable_candidate = !underpowered
        && net_pnl > 0.0
        && fill_rate >= 0.95
        && avg_realized_return_per_stake > 0.0
        && expectancy_calibration_gap <= 0.30
        && positive_day_rate >= 0.70
        && positive_symbol_rate >= 0.70;

    MatrixResult {
        hypothesis: h.name.to_string(),
        split: split.to_string(),
        direction_mode: h.direction_mode,
        fill_mode: h.fill_mode,
        pm_mode: h.pm_mode,
        rows_after_gates: rows.len(),
        event_sides_after_gates: event_side_count(rows),
        selected,
        trades,
        rejected_duplicate,
        rejected_cooldown,
        rejected_non_executable,
        net_pnl,
        avg_pnl,
        fill_rate,
        win_rate,
        avg_entry_price,
        avg_expected_value_per_stake,
        avg_realized_return_per_stake,
        expectancy_calibration_gap,
        positive_day_rate,
        positive_symbol_rate,
        min_trades,
        underpowered,
        deployable_candidate,
    }
}

fn hypotheses() -> Vec<MatrixHypothesis> {
    let mut out = Vec::new();
    let directions = [
        ("model", DirectionMode::Model, 0.55),
        ("inverted", DirectionMode::Inverted, 0.535),
    ];
    let fills = [
        ("full_depth", FillMode::FullDepthRoundTrip),
        ("roundtrip", FillMode::ExecutableRoundTrip),
        ("entry_only", FillMode::EntryOnlyExecutable),
    ];
    let pms = [
        ("pm_none", PmMode::None),
        ("pm_soft", PmMode::SoftDynamics),
        ("pm_exit", PmMode::PositiveExitBid),
    ];
    let time_windows = [("short", 30, 90), ("middle", 60, 180), ("wide", 30, 240)];
    let ev_floors = [0.05, 0.10, 0.20];

    for (direction_name, direction_mode, min_direction_prob) in directions {
        for (fill_name, fill_mode) in fills {
            for (pm_name, pm_mode) in pms {
                for (time_name, min_time, max_time) in time_windows {
                    for min_ev_per_stake in ev_floors {
                        out.push(MatrixHypothesis {
                            name: Box::leak(
                                format!(
                                    "{direction_name}_{fill_name}_{pm_name}_{time_name}_ev{:.2}",
                                    min_ev_per_stake
                                )
                                .into_boxed_str(),
                            ),
                            direction_mode,
                            fill_mode,
                            pm_mode,
                            min_direction_prob,
                            min_ev_per_stake,
                            min_reward_risk: 0.20,
                            min_time_remaining_secs: min_time,
                            max_time_remaining_secs: max_time,
                            probability_shrink: 0.38,
                            probability_haircut: 0.04,
                            cooldown_secs: 30,
                        });
                    }
                }
            }
        }
    }
    out
}

fn write_csv(path: PathBuf, header: &[&str], rows: &[Vec<String>]) -> Result<()> {
    let mut body = String::new();
    body.push_str(&header.join(","));
    body.push('\n');
    for row in rows {
        body.push_str(&row.join(","));
        body.push('\n');
    }
    fs::write(path, body).context("write csv")
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let snapshot_dir = PathBuf::from(flag_value(&args, "--snapshot-dir").unwrap_or_else(|| {
        eprintln!("ERROR: --snapshot-dir is required");
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
    let stake_usd = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);
    let min_trades = flag_value(&args, "--min-trades")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(80usize);
    let output_dir = PathBuf::from(
        flag_value(&args, "--output-dir").unwrap_or_else(|| "artifacts/edge-matrix".to_string()),
    );
    fs::create_dir_all(&output_dir).context("create output dir")?;

    let started = Instant::now();
    let snapshot = load_research_snapshot(&snapshot_dir)
        .with_context(|| format!("load research snapshot {}", snapshot_dir.display()))?;
    let snapshot_hash = snapshot
        .manifest
        .snapshot_hash
        .clone()
        .unwrap_or_else(|| "<missing>".to_string());
    eprintln!(
        "Loaded research snapshot hash={} observations={} pm_books={} in {}ms",
        snapshot_hash,
        snapshot.observations.len(),
        snapshot.pm_book_snapshots.len(),
        started.elapsed().as_millis()
    );

    let review_options = FactorReviewOptions {
        stake_usd,
        min_observations: min_trades,
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
        "Data health: source_obs={} v2_rows={} executable_pnl_rows={} full_depth_pnl_rows={} entry_fill={:.2}% full_depth_entry={:.2}%",
        health.source_observations,
        health.v2_rows,
        health.executable_pnl_rows,
        health.full_depth_executable_pnl_rows,
        health.entry_fill_rate() * 100.0,
        health.full_depth_entry_fill_rate() * 100.0,
    );

    let train_rows = slice_by_time(&v2_rows, train_start, train_end);
    let val_rows = slice_by_time(&v2_rows, val_start, val_end);
    let hypotheses = hypotheses();
    let mut gate_rows = Vec::new();
    let mut results = Vec::new();
    for h in &hypotheses {
        results.push(evaluate_hypothesis(
            &train_rows,
            *h,
            "train",
            min_trades,
            &mut gate_rows,
        ));
        results.push(evaluate_hypothesis(
            &val_rows,
            *h,
            "validation",
            min_trades,
            &mut gate_rows,
        ));
    }

    let summary = MatrixSummary {
        snapshot_hash,
        snapshot_generated_at: snapshot.manifest.generated_at,
        train_start,
        train_end,
        val_start,
        val_end,
        symbols,
        min_trades,
        source_rows: snapshot.observations.len(),
        v2_rows: v2_rows.len(),
        hypothesis_count: hypotheses.len(),
        gate_rows,
        results,
    };

    fs::write(
        output_dir.join("edge-matrix-summary.json"),
        serde_json::to_string_pretty(&summary).context("serialize summary")?,
    )
    .context("write summary json")?;

    let result_rows = summary
        .results
        .iter()
        .map(|row| {
            vec![
                row.hypothesis.clone(),
                row.split.clone(),
                format!("{:?}", row.direction_mode),
                format!("{:?}", row.fill_mode),
                format!("{:?}", row.pm_mode),
                row.trades.to_string(),
                format!("{:.6}", row.net_pnl),
                format!("{:.6}", row.fill_rate),
                format!("{:.6}", row.win_rate),
                format!("{:.6}", row.avg_realized_return_per_stake),
                format!("{:.6}", row.avg_expected_value_per_stake),
                format!("{:.6}", row.expectancy_calibration_gap),
                format!("{:.6}", row.positive_day_rate),
                format!("{:.6}", row.positive_symbol_rate),
                row.underpowered.to_string(),
                row.deployable_candidate.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    write_csv(
        output_dir.join("strategy-matrix-results.csv"),
        &[
            "hypothesis",
            "split",
            "direction_mode",
            "fill_mode",
            "pm_mode",
            "trades",
            "net_pnl",
            "fill_rate",
            "win_rate",
            "avg_realized_return_per_stake",
            "avg_expected_value_per_stake",
            "expectancy_calibration_gap",
            "positive_day_rate",
            "positive_symbol_rate",
            "underpowered",
            "deployable_candidate",
        ],
        &result_rows,
    )?;

    let gate_csv_rows = summary
        .gate_rows
        .iter()
        .map(|row| {
            vec![
                row.hypothesis.clone(),
                row.split.clone(),
                row.gate_index.to_string(),
                row.gate.clone(),
                row.rows.to_string(),
                row.event_sides.to_string(),
                row.executable_pnl_rows.to_string(),
                row.full_depth_pnl_rows.to_string(),
                format!("{:.6}", row.entry_fill_rate),
                format!("{:.6}", row.roundtrip_fill_rate),
                format!("{:.6}", row.total_executable_pnl),
                format!("{:.6}", row.avg_executable_pnl),
            ]
        })
        .collect::<Vec<_>>();
    write_csv(
        output_dir.join("gate-attrition.csv"),
        &[
            "hypothesis",
            "split",
            "gate_index",
            "gate",
            "rows",
            "event_sides",
            "executable_pnl_rows",
            "full_depth_pnl_rows",
            "entry_fill_rate",
            "roundtrip_fill_rate",
            "total_executable_pnl",
            "avg_executable_pnl",
        ],
        &gate_csv_rows,
    )?;

    let mut best_validation = summary
        .results
        .iter()
        .filter(|row| row.split == "validation")
        .collect::<Vec<_>>();
    best_validation.sort_by(|a, b| {
        b.deployable_candidate
            .cmp(&a.deployable_candidate)
            .then_with(|| b.trades.cmp(&a.trades))
            .then_with(|| b.net_pnl.total_cmp(&a.net_pnl))
    });
    eprintln!("=== Top validation hypotheses ===");
    for row in best_validation.iter().take(12) {
        eprintln!(
            "{} trades={} pnl=${:.2} fill={:.1}% ev_gap={:.3} pos_day={:.1}% pos_symbol={:.1}% deployable={}",
            row.hypothesis,
            row.trades,
            row.net_pnl,
            row.fill_rate * 100.0,
            row.expectancy_calibration_gap,
            row.positive_day_rate * 100.0,
            row.positive_symbol_rate * 100.0,
            row.deployable_candidate
        );
    }

    if !summary
        .results
        .iter()
        .any(|row| row.deployable_candidate && row.split == "validation")
    {
        anyhow::bail!("edge matrix found no deployable validation candidate");
    }
    Ok(())
}
