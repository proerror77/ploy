//! Hyperparameter optimization for PM5D strategy variants using TPE (Bayesian).
//!
//! Usage:
//!   cargo run --release -p ploy-strategy-bundles --example optimize_backtest -- \
//!     --db-url postgresql://postgres:postgres@localhost:15432/ploy \
//!     --strategy-variant directional \
//!     --train-start 2026-04-01 \
//!     --train-end   2026-04-03 \
//!     --val-start   2026-04-04 \
//!     --val-end     2026-04-04 \
//!     --trials 200
//!
//!   cargo run --release -p ploy-strategy-bundles --example optimize_backtest -- \
//!     --db-url postgresql://postgres:postgres@localhost:15432/ploy \
//!     --strategy-variant reversal \
//!     --train-start 2026-04-10 \
//!     --train-end   2026-04-10 \
//!     --val-start   2026-04-11 \
//!     --val-end     2026-04-11 \
//!     --symbols BTCUSDT,DOGEUSDT \
//!     --trials 80

use chrono::{NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    feed::{HistoricalLoadOptions, load_from_database_with_options},
    DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder, ReversalStrategy,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyLogic,
    StrategyRuntime,
};
use ploy_trading::TradeSide;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::sync::Arc;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn parse_date_start(raw: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {raw}"))
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    )
}

fn parse_date_end(raw: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {raw}"))
            .and_hms_opt(23, 59, 59)
            .unwrap(),
    )
}

fn parse_timestamp(raw: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("Invalid timestamp: {raw}"))
        .with_timezone(&Utc)
}

fn canonical_strategy_variant(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "directional" | "v1" | "v2" | "v3" | "pm5d_v1" | "pm5d_v2" | "pm5d_v3" => {
            "directional".to_string()
        }
        "reversal" | "pm5d_reversal" | "pm-5m-reversal" => "reversal".to_string(),
        other => other.to_string(),
    }
}

fn build_strategy(strategy_variant: &str, config: DirectionalConfig) -> Box<dyn StrategyLogic> {
    match strategy_variant {
        "directional" => Box::new(DirectionalStrategy::new(config)),
        "reversal" => Box::new(ReversalStrategy::new(config.into())),
        other => panic!("unsupported strategy_variant: {other}"),
    }
}

/// Run a single backtest and return (net_pnl, trade_count, sharpe).
fn run_backtest(
    strategy_variant: &str,
    config: DirectionalConfig,
    data: &[MarketUpdate],
) -> (f64, usize, f64) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let strategy = build_strategy(strategy_variant, config);
    let feed = HistoricalFeed::new(data.to_vec());
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig::default());
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = rt.block_on(runtime.run());

    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    let cashflow = snapshot.fill_cashflow_summary();
    let net_pnl = cashflow.net_pnl().to_string().parse::<f64>().unwrap_or(0.0);
    let trade_count = result.fills_recorded as usize / 2;

    let fills = &snapshot.fills;
    let mut per_trade_pnl = Vec::new();
    let mut index = 0;
    while index + 1 < fills.len() {
        let entry = &fills[index];
        let exit = &fills[index + 1];
        if entry.side == TradeSide::Buy && exit.side == TradeSide::Sell {
            let pnl = (exit.price - entry.price) * entry.quantity - entry.fee;
            per_trade_pnl.push(pnl.to_string().parse::<f64>().unwrap_or(0.0));
            index += 2;
        } else {
            index += 1;
        }
    }

    let sharpe = if per_trade_pnl.len() < 5 {
        -10.0
    } else {
        let n = per_trade_pnl.len() as f64;
        let mean = per_trade_pnl.iter().sum::<f64>() / n;
        let variance = per_trade_pnl
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / n;
        let std_dev = variance.sqrt();
        if std_dev < 1e-9 {
            0.0
        } else {
            mean / std_dev * (87.0_f64 * 365.0).sqrt()
        }
    };

    (net_pnl, trade_count, sharpe)
}

fn make_directional_config(
    symbols: &[String],
    min_probability: f64,
    min_edge: f64,
    max_entry_price: f64,
    cooldown_secs: i64,
    min_time: i64,
    max_time: i64,
) -> DirectionalConfig {
    DirectionalConfig {
        symbols: symbols.to_vec(),
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        min_time_remaining_secs: min_time as u64,
        max_time_remaining_secs: max_time as u64,
        cooldown_secs: cooldown_secs as u64,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300, 900],
    three_layer_min_direction_prob: 0.56,
    three_layer_min_distance_over_sigma: 0.3,
    three_layer_min_confirmation_score: 0.10,
    three_layer_min_drift_confirmation: 0.0002,
    three_layer_min_edge: 0.03,
    three_layer_min_reward_risk: 1.2,
    three_layer_take_profit_ask: 0.70,
    three_layer_stop_distance_pct: 0.020,
    three_layer_max_pm_lag_secs: 15,
}
}

struct ReversalSearchParams {
    max_distance_pct: f64,
    max_drift_flip_age_secs: i64,
    min_post_flip_drift: f64,
    min_lob_depth_ratio: f64,
    max_ask_for_reversal: f64,
    max_pm_lag_secs: i64,
    min_edge: f64,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

fn make_reversal_config(symbols: &[String], params: &ReversalSearchParams) -> DirectionalConfig {
    DirectionalConfig {
        symbols: symbols.to_vec(),
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: params.min_edge,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: params.max_distance_pct,
        reversal_max_drift_flip_age_secs: params.max_drift_flip_age_secs as u64,
        reversal_min_post_flip_drift: params.min_post_flip_drift,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: params.min_lob_depth_ratio,
        reversal_max_ask_for_reversal: params.max_ask_for_reversal,
        reversal_max_pm_lag_secs: params.max_pm_lag_secs as u64,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        min_time_remaining_secs: params.min_time_remaining_secs as u64,
        max_time_remaining_secs: params.max_time_remaining_secs as u64,
        cooldown_secs: params.cooldown_secs as u64,
        stake_usd: dec!(10),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300],
    three_layer_min_direction_prob: 0.56,
    three_layer_min_distance_over_sigma: 0.3,
    three_layer_min_confirmation_score: 0.10,
    three_layer_min_drift_confirmation: 0.0002,
    three_layer_min_edge: 0.03,
    three_layer_min_reward_risk: 1.2,
    three_layer_take_profit_ask: 0.70,
    three_layer_stop_distance_pct: 0.020,
    three_layer_max_pm_lag_secs: 15,
}
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
    let strategy_variant = canonical_strategy_variant(
        &flag_value(&args, "--strategy-variant").unwrap_or_else(|| "directional".into()),
    );
    let train_start = flag_value(&args, "--train-start").unwrap_or_else(|| "2026-04-01".into());
    let train_end = flag_value(&args, "--train-end").unwrap_or_else(|| "2026-04-03".into());
    let val_start = flag_value(&args, "--val-start").unwrap_or_else(|| "2026-04-04".into());
    let val_end = flag_value(&args, "--val-end").unwrap_or_else(|| "2026-04-04".into());
    let train_start_ts = flag_value(&args, "--train-start-ts");
    let train_end_ts = flag_value(&args, "--train-end-ts");
    let val_start_ts = flag_value(&args, "--val-start-ts");
    let val_end_ts = flag_value(&args, "--val-end-ts");
    let symbols_arg = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT".into());
    let require_official_settlement = args
        .iter()
        .any(|arg| arg == "--require-official-settlement");
    let n_trials: usize = flag_value(&args, "--trials")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(200);
    let symbols: Vec<String> = symbols_arg
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    eprintln!("=== PM5D Hyperparameter Optimization ===");
    eprintln!("Variant: {strategy_variant}");
    eprintln!(
        "Train: {} → {}",
        train_start_ts.as_deref().unwrap_or(&train_start),
        train_end_ts.as_deref().unwrap_or(&train_end)
    );
    eprintln!(
        "Val:   {} → {}",
        val_start_ts.as_deref().unwrap_or(&val_start),
        val_end_ts.as_deref().unwrap_or(&val_end)
    );
    eprintln!("Symbols: {:?}", symbols);
    eprintln!("Official-only settlement: {}", require_official_settlement);
    eprintln!("Trials: {n_trials}  Algorithm: TPE");
    eprintln!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let pool = rt
        .block_on(PgPoolOptions::new().max_connections(3).connect(&db_url))
        .expect("DB connection failed");

    eprintln!("Loading training data ({} → {})...", train_start, train_end);
    let train_from = train_start_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_start(&train_start));
    let train_to = train_end_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_end(&train_end));
    let train_data = rt
        .block_on(load_from_database_with_options(
            &pool,
            &symbols,
            train_from,
            train_to,
            &HistoricalLoadOptions {
                require_official_settlement,
                ..HistoricalLoadOptions::default()
            },
        ))
        .expect("Failed to load training data");
    eprintln!("  {} updates loaded", train_data.len());

    eprintln!("Loading validation data ({} → {})...", val_start, val_end);
    let val_from = val_start_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_start(&val_start));
    let val_to = val_end_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_end(&val_end));
    let val_data = rt
        .block_on(load_from_database_with_options(
            &pool,
            &symbols,
            val_from,
            val_to,
            &HistoricalLoadOptions {
                require_official_settlement,
                ..HistoricalLoadOptions::default()
            },
        ))
        .expect("Failed to load validation data");
    eprintln!("  {} updates loaded\n", val_data.len());

    let train_data = Arc::new(train_data);
    let symbols_ref = Arc::new(symbols.clone());
    let study: Study<f64> = Study::maximize(TpeSampler::new());

    if strategy_variant == "reversal" {
        let p_max_distance =
            FloatParam::new(0.015, 0.05).name("reversal_max_distance_pct");
        let p_max_ask =
            FloatParam::new(0.45, 0.85).name("reversal_max_ask_for_reversal");
        let p_pm_lag = IntParam::new(20, 120).name("reversal_max_pm_lag_secs");
        let p_min_edge = FloatParam::new(-0.05, 0.01).name("min_edge");

        let train_ref = Arc::clone(&train_data);
        let symbols_ref_c = Arc::clone(&symbols_ref);

        let p_max_distance_c = p_max_distance.clone();
        let p_max_ask_c = p_max_ask.clone();
        let p_pm_lag_c = p_pm_lag.clone();
        let p_min_edge_c = p_min_edge.clone();

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let params = ReversalSearchParams {
                    max_distance_pct: p_max_distance_c.suggest(trial)?,
                    max_drift_flip_age_secs: 60,
                    min_post_flip_drift: 0.0,
                    min_lob_depth_ratio: 0.0,
                    max_ask_for_reversal: p_max_ask_c.suggest(trial)?,
                    max_pm_lag_secs: p_pm_lag_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    cooldown_secs: 0,
                    min_time_remaining_secs: 15,
                    max_time_remaining_secs: 300,
                };

                let config = make_reversal_config(symbols_ref_c.as_slice(), &params);
                let (net_pnl, trades, sharpe) = run_backtest("reversal", config, &train_ref);
                let score = if trades == 0 {
                    -100.0
                } else if trades < 5 {
                    net_pnl
                } else {
                    sharpe
                };

                eprintln!(
                    "  Trial {:>3}: score={:>7.3} sharpe={:>7.3} pnl=${:>8.2} trades={:>4} dist={:.4} flip={} drift={:.5} lob={:.2} ask={:.3} lag={}",
                    trial.id(),
                    score,
                    sharpe,
                    net_pnl,
                    trades,
                    params.max_distance_pct,
                    params.max_drift_flip_age_secs,
                    params.min_post_flip_drift,
                    params.min_lob_depth_ratio,
                    params.max_ask_for_reversal,
                    params.max_pm_lag_secs,
                );

                Ok::<f64, Error>(score)
            })
            .expect("Optimization failed");

        let best = study.best_trial().expect("No completed trials");
        let best_params = ReversalSearchParams {
            max_distance_pct: best.get(&p_max_distance).unwrap_or(0.015),
            max_drift_flip_age_secs: 60,
            min_post_flip_drift: 0.0,
            min_lob_depth_ratio: 0.0,
            max_ask_for_reversal: best.get(&p_max_ask).unwrap_or(0.25),
            max_pm_lag_secs: best.get(&p_pm_lag).unwrap_or(30),
            min_edge: best.get(&p_min_edge).unwrap_or(0.02),
            cooldown_secs: 0,
            min_time_remaining_secs: 15,
            max_time_remaining_secs: 300,
        };

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Objective score:                 {:.3}", best.value);
        eprintln!(
            "reversal_max_distance_pct:     {:.4}",
            best_params.max_distance_pct
        );
        eprintln!(
            "reversal_max_drift_flip_age:   {}",
            best_params.max_drift_flip_age_secs
        );
        eprintln!(
            "reversal_min_post_flip_drift:  {:.5}",
            best_params.min_post_flip_drift
        );
        eprintln!(
            "reversal_min_lob_depth_ratio:  {:.3}",
            best_params.min_lob_depth_ratio
        );
        eprintln!(
            "reversal_max_ask_for_reversal: {:.4}",
            best_params.max_ask_for_reversal
        );
        eprintln!(
            "reversal_max_pm_lag_secs:      {}",
            best_params.max_pm_lag_secs
        );
        eprintln!("min_edge:                      {:.4}", best_params.min_edge);
        eprintln!(
            "cooldown_secs:                 {}",
            best_params.cooldown_secs
        );
        eprintln!(
            "min_time_remaining_secs:       {}",
            best_params.min_time_remaining_secs
        );
        eprintln!(
            "max_time_remaining_secs:       {}",
            best_params.max_time_remaining_secs
        );

        eprintln!("\n=== Validation (held-out) ===");
        let val_config = make_reversal_config(symbols_ref.as_slice(), &best_params);
        let (val_pnl, val_trades, val_sharpe) = run_backtest("reversal", val_config, &val_data);
        eprintln!("Val Sharpe:  {val_sharpe:.3}");
        eprintln!("Val PnL:     ${val_pnl:.2}");
        eprintln!("Val Trades:  {val_trades}");

        eprintln!("\n=== Config Snippet ===");
        eprintln!(
            "reversal_max_distance_pct = {:.4}",
            best_params.max_distance_pct
        );
        eprintln!(
            "reversal_max_drift_flip_age_secs = {}",
            best_params.max_drift_flip_age_secs
        );
        eprintln!(
            "reversal_min_post_flip_drift = {:.5}",
            best_params.min_post_flip_drift
        );
        eprintln!(
            "reversal_min_lob_depth_ratio = {:.3}",
            best_params.min_lob_depth_ratio
        );
        eprintln!(
            "reversal_max_ask_for_reversal = {:.4}",
            best_params.max_ask_for_reversal
        );
        eprintln!(
            "reversal_max_pm_lag_secs = {}",
            best_params.max_pm_lag_secs
        );
        eprintln!("min_edge = {:.4}", best_params.min_edge);
        eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
        eprintln!(
            "min_time_remaining_secs = {}",
            best_params.min_time_remaining_secs
        );
        eprintln!(
            "max_time_remaining_secs = {}",
            best_params.max_time_remaining_secs
        );

        let mut all_trials = study.trials();
        all_trials.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        eprintln!("\n=== Top 10 Trials ===");
        eprintln!(
            "{:<6} {:<8} {:<8} {:<8} {:<8} {:<8} {:<8}",
            "Trial", "Score", "dist", "flip", "lob", "ask", "edge"
        );
        for trial in all_trials.iter().take(10) {
            eprintln!(
                "{:<6} {:<8.3} {:<8.4} {:<8} {:<8.2} {:<8.3} {:<8.4}",
                trial.id,
                trial.value,
                trial.get(&p_max_distance).unwrap_or(0.0),
                60,
                0.0,
                trial.get(&p_max_ask).unwrap_or(0.0),
                trial.get(&p_min_edge).unwrap_or(0.0),
            );
        }
    } else {
        let p_min_prob = FloatParam::new(0.50, 0.72).name("min_probability");
        let p_min_edge = FloatParam::new(0.005, 0.06).name("min_edge");
        let p_max_entry = FloatParam::new(0.35, 0.85).name("max_entry_price");
        let p_cooldown = IntParam::new(5, 90).name("cooldown_secs");
        let p_min_time = IntParam::new(20, 120).name("min_time_remaining_secs");
        let p_max_time = IntParam::new(180, 300).name("max_time_remaining_secs");

        let train_ref = Arc::clone(&train_data);
        let symbols_ref_c = Arc::clone(&symbols_ref);
        let p_min_prob_c = p_min_prob.clone();
        let p_min_edge_c = p_min_edge.clone();
        let p_max_entry_c = p_max_entry.clone();
        let p_cooldown_c = p_cooldown.clone();
        let p_min_time_c = p_min_time.clone();
        let p_max_time_c = p_max_time.clone();

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let min_prob = p_min_prob_c.suggest(trial)?;
                let min_edge = p_min_edge_c.suggest(trial)?;
                let max_entry = p_max_entry_c.suggest(trial)?;
                let cooldown = p_cooldown_c.suggest(trial)?;
                let min_time = p_min_time_c.suggest(trial)?;
                let max_time = p_max_time_c.suggest(trial)?;

                if max_time <= min_time || max_entry <= 0.45 {
                    return Ok::<f64, Error>(-10.0);
                }

                let config = make_directional_config(
                    symbols_ref_c.as_slice(),
                    min_prob,
                    min_edge,
                    max_entry,
                    cooldown,
                    min_time,
                    max_time,
                );
                let (net_pnl, trades, sharpe) =
                    run_backtest("directional", config, &train_ref);

                eprintln!(
                    "  Trial {:>3}: sharpe={:>7.3}  pnl=${:>8.2}  trades={:>4}  p={:.3}  edge={:.4}  max={:.2}  cd={}s",
                    trial.id(),
                    sharpe,
                    net_pnl,
                    trades,
                    min_prob,
                    min_edge,
                    max_entry,
                    cooldown
                );

                Ok(sharpe)
            })
            .expect("Optimization failed");

        let best = study.best_trial().expect("No completed trials");
        let best_min_prob = best.get(&p_min_prob).unwrap_or(0.55);
        let best_min_edge = best.get(&p_min_edge).unwrap_or(0.02);
        let best_max_entry = best.get(&p_max_entry).unwrap_or(0.85);
        let best_cooldown = best.get(&p_cooldown).unwrap_or(15);
        let best_min_time = best.get(&p_min_time).unwrap_or(60);
        let best_max_time = best.get(&p_max_time).unwrap_or(300);

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Sharpe:                {:.3}", best.value);
        eprintln!("min_probability:       {best_min_prob:.4}");
        eprintln!("min_edge:              {best_min_edge:.4}");
        eprintln!("max_entry_price:       {best_max_entry:.4}");
        eprintln!("cooldown_secs:         {best_cooldown}");
        eprintln!("min_time_remaining:    {best_min_time}");
        eprintln!("max_time_remaining:    {best_max_time}");

        eprintln!("\n=== Validation (held-out) ===");
        let val_config = make_directional_config(
            symbols_ref.as_slice(),
            best_min_prob,
            best_min_edge,
            best_max_entry,
            best_cooldown,
            best_min_time,
            best_max_time,
        );
        let (val_pnl, val_trades, val_sharpe) =
            run_backtest("directional", val_config, &val_data);
        eprintln!("Val Sharpe:  {val_sharpe:.3}");
        eprintln!("Val PnL:     ${val_pnl:.2}");
        eprintln!("Val Trades:  {val_trades}");

        eprintln!("\n=== Config Snippet ===");
        eprintln!("min_probability = {best_min_prob:.4}");
        eprintln!("min_edge = {best_min_edge:.4}");
        eprintln!("max_entry_price = {best_max_entry:.4}");
        eprintln!("cooldown_secs = {best_cooldown}");
        eprintln!("min_time_remaining_secs = {best_min_time}");
        eprintln!("max_time_remaining_secs = {best_max_time}");

        let mut all_trials = study.trials();
        all_trials.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        eprintln!("\n=== Top 10 Trials ===");
        eprintln!(
            "{:<6} {:<8} {:<8} {:<8} {:<8} {:<10} {:<10}",
            "Trial", "Sharpe", "p_min", "edge", "max_px", "cooldown", "min_time"
        );
        for trial in all_trials.iter().take(10) {
            eprintln!(
                "{:<6} {:<8.3} {:<8.3} {:<8.4} {:<8.3} {:<10} {:<10}",
                trial.id,
                trial.value,
                trial.get(&p_min_prob).unwrap_or(0.0),
                trial.get(&p_min_edge).unwrap_or(0.0),
                trial.get(&p_max_entry).unwrap_or(0.0),
                trial.get(&p_cooldown).unwrap_or(0),
                trial.get(&p_min_time).unwrap_or(0),
            );
        }
    }
}
