//! Hyperparameter optimization for pm_5m_directional using TPE (Bayesian).
//!
//! Usage:
//!   cargo run --release -p ploy-strategy-bundles --example optimize_backtest -- \
//!     --db-url postgresql://postgres:postgres@localhost:15432/ploy \
//!     --train-start 2026-04-01 \
//!     --train-end   2026-04-03 \
//!     --val-start   2026-04-04 \
//!     --val-end     2026-04-04 \
//!     --trials 200
//!
//! Optimizes: min_probability, min_edge, cooldown_secs, min_time_remaining_secs, max_time_remaining_secs
//! Objective: Sharpe ratio on training window, validated on held-out window
//! Algorithm: TPE (Tree-structured Parzen Estimator) — same as Optuna default

use chrono::{NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_strategy_bundles::{
    feed::{load_from_database_with_options, HistoricalLoadOptions},
    DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyRuntime,
};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_trading::TradeSide;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::sync::Arc;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn parse_date_start(s: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {s}"))
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    )
}

fn parse_date_end(s: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {s}"))
            .and_hms_opt(23, 59, 59)
            .unwrap(),
    )
}

/// Run a single backtest and return (net_pnl, trade_count, sharpe).
fn run_backtest(config: DirectionalConfig, data: &[MarketUpdate]) -> (f64, usize, f64) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let strategy = DirectionalStrategy::new(config);
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
    let trade_count = result.fills_recorded as usize / 2; // entry + settlement = 2 fills per trade

    // Per-trade PnL for Sharpe calculation
    let fills = &snapshot.fills;
    let mut per_trade_pnl: Vec<f64> = Vec::new();
    let mut i = 0;
    while i + 1 < fills.len() {
        let entry = &fills[i];
        let exit = &fills[i + 1];
        if entry.side == TradeSide::Buy && exit.side == TradeSide::Sell {
            let pnl = (exit.price - entry.price) * entry.quantity - entry.fee;
            per_trade_pnl.push(pnl.to_string().parse::<f64>().unwrap_or(0.0));
            i += 2;
        } else {
            i += 1;
        }
    }

    let sharpe = if per_trade_pnl.len() < 5 {
        -10.0 // penalize too-few-trades configs
    } else {
        let n = per_trade_pnl.len() as f64;
        let mean = per_trade_pnl.iter().sum::<f64>() / n;
        let variance = per_trade_pnl.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let std_dev = variance.sqrt();
        if std_dev < 1e-9 {
            0.0
        } else {
            // Annualize: ~87 trades/day × 365 days
            mean / std_dev * (87.0_f64 * 365.0).sqrt()
        }
    };

    (net_pnl, trade_count, sharpe)
}

fn make_config(
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
        min_time_remaining_secs: min_time as u64,
        max_time_remaining_secs: max_time as u64,
        cooldown_secs: cooldown_secs as u64,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300, 900],
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
    let train_start = flag_value(&args, "--train-start").unwrap_or_else(|| "2026-04-01".into());
    let train_end   = flag_value(&args, "--train-end").unwrap_or_else(|| "2026-04-03".into());
    let val_start   = flag_value(&args, "--val-start").unwrap_or_else(|| "2026-04-04".into());
    let val_end     = flag_value(&args, "--val-end").unwrap_or_else(|| "2026-04-04".into());
    let symbols_arg = flag_value(&args, "--symbols").unwrap_or_else(|| {
        "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT".into()
    });
    let require_official_settlement = args.iter().any(|arg| arg == "--require-official-settlement");
    let n_trials: usize = flag_value(&args, "--trials")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let symbols: Vec<String> = symbols_arg
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    eprintln!("=== pm_5m_directional Hyperparameter Optimization ===");
    eprintln!("Train: {} → {}", train_start, train_end);
    eprintln!("Val:   {} → {}", val_start, val_end);
    eprintln!("Symbols: {:?}", symbols);
    eprintln!("Official-only settlement: {}", require_official_settlement);
    eprintln!("Trials: {n_trials}  Algorithm: TPE");
    eprintln!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let pool = rt.block_on(
        PgPoolOptions::new().max_connections(3).connect(&db_url)
    ).expect("DB connection failed");

    eprintln!("Loading training data ({} → {})...", train_start, train_end);
    let train_data = rt.block_on(
        load_from_database_with_options(
            &pool,
            &symbols,
            parse_date_start(&train_start),
            parse_date_end(&train_end),
            &HistoricalLoadOptions {
                require_official_settlement,
                ..HistoricalLoadOptions::default()
            },
        )
    ).expect("Failed to load training data");
    eprintln!("  {} updates loaded", train_data.len());

    eprintln!("Loading validation data ({} → {})...", val_start, val_end);
    let val_data = rt.block_on(
        load_from_database_with_options(
            &pool,
            &symbols,
            parse_date_start(&val_start),
            parse_date_end(&val_end),
            &HistoricalLoadOptions {
                require_official_settlement,
                ..HistoricalLoadOptions::default()
            },
        )
    ).expect("Failed to load validation data");
    eprintln!("  {} updates loaded\n", val_data.len());

    let train_data = Arc::new(train_data);

    // Define search space — keep references for extracting best values later
    let p_min_prob = FloatParam::new(0.50, 0.72).name("min_probability");
    let p_min_edge = FloatParam::new(0.005, 0.06).name("min_edge");
    let p_max_entry = FloatParam::new(0.35, 0.85).name("max_entry_price");
    let p_cooldown = IntParam::new(5, 90).name("cooldown_secs");
    let p_min_time = IntParam::new(20, 120).name("min_time_remaining_secs");
    let p_max_time = IntParam::new(180, 300).name("max_time_remaining_secs");

    // TPE sampler (Bayesian, same as Optuna default)
    let study: Study<f64> = Study::maximize(TpeSampler::new());

    // Clone params for use inside closure (originals kept for best.get())
    let p_min_prob_c = p_min_prob.clone();
    let p_min_edge_c = p_min_edge.clone();
    let p_max_entry_c = p_max_entry.clone();
    let p_cooldown_c = p_cooldown.clone();
    let p_min_time_c = p_min_time.clone();
    let p_max_time_c = p_max_time.clone();
    let train_ref = Arc::clone(&train_data);
    let symbols_ref = Arc::new(symbols.clone());
    let symbols_ref_c = Arc::clone(&symbols_ref);

    study.optimize(n_trials, move |trial: &mut Trial| {
        let min_prob = p_min_prob_c.suggest(trial)?;
        let min_edge = p_min_edge_c.suggest(trial)?;
        let max_entry = p_max_entry_c.suggest(trial)?;
        let cooldown = p_cooldown_c.suggest(trial)?;
        let min_time = p_min_time_c.suggest(trial)?;
        let max_time = p_max_time_c.suggest(trial)?;

        // Constraint: max_time must be > min_time
        if max_time <= min_time {
            return Ok::<f64, Error>(-10.0);
        }
        if max_entry <= 0.45 {
            return Ok::<f64, Error>(-10.0);
        }

        let config = make_config(
            symbols_ref_c.as_slice(),
            min_prob,
            min_edge,
            max_entry,
            cooldown,
            min_time,
            max_time,
        );
        let (net_pnl, trades, sharpe) = run_backtest(config, &train_ref);

        eprintln!(
            "  Trial {:>3}: sharpe={:>7.3}  pnl=${:>8.2}  trades={:>4}  p={:.3}  edge={:.4}  max={:.2}  cd={}s",
            trial.id(), sharpe, net_pnl, trades, min_prob, min_edge, max_entry, cooldown
        );

        Ok(sharpe)
    }).expect("Optimization failed");

    // Extract best params using original param objects (same IDs)
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

    // Validate on held-out window
    eprintln!("\n=== Validation (held-out) ===");
    let val_config = make_config(
        symbols_ref.as_slice(),
        best_min_prob,
        best_min_edge,
        best_max_entry,
        best_cooldown,
        best_min_time,
        best_max_time,
    );
    let (val_pnl, val_trades, val_sharpe) = run_backtest(val_config, &val_data);
    eprintln!("Val Sharpe:  {val_sharpe:.3}");
    eprintln!("Val PnL:     ${val_pnl:.2}");
    eprintln!("Val Trades:  {val_trades}");

    if val_sharpe >= best.value * 0.5 {
        eprintln!("\n✓ Val Sharpe ≥50% of train — params look robust");
    } else {
        eprintln!("\n⚠ Val Sharpe dropped >50% — possible overfitting");
    }

    // Print TOML snippet for easy copy-paste
    eprintln!("\n=== Config Snippet ===");
    eprintln!("min_probability = {best_min_prob:.4}");
    eprintln!("min_edge = {best_min_edge:.4}");
    eprintln!("max_entry_price = {best_max_entry:.4}");
    eprintln!("cooldown_secs = {best_cooldown}");
    eprintln!("min_time_remaining_secs = {best_min_time}");
    eprintln!("max_time_remaining_secs = {best_max_time}");

    // Top 10 trials
    let mut all_trials = study.trials();
    all_trials.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal));
    eprintln!("\n=== Top 10 Trials ===");
    eprintln!("{:<6} {:<8} {:<8} {:<8} {:<8} {:<10} {:<10}",
        "Trial", "Sharpe", "p_min", "edge", "max_px", "cooldown", "min_time");
    for t in all_trials.iter().take(10) {
        eprintln!("{:<6} {:<8.3} {:<8.3} {:<8.4} {:<8.3} {:<10} {:<10}",
            t.id,
            t.value,
            t.get(&p_min_prob).unwrap_or(0.0),
            t.get(&p_min_edge).unwrap_or(0.0),
            t.get(&p_max_entry).unwrap_or(0.0),
            t.get(&p_cooldown).unwrap_or(0),
            t.get(&p_min_time).unwrap_or(0),
        );
    }
}
