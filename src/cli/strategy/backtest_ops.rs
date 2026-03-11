use super::settlement_ops::backtest_directional_signals_pm_settlement;
use super::*;

mod diagnostics;
mod reporting;

use diagnostics::{
    print_backtest_db_diagnostics, resolve_pm5_replay_window, verify_backtest_trades_gamma,
    Pm5mReplayWindow,
};

pub(super) use reporting::{run_backtest_diff, run_backtest_list, run_live_backtest_compare};

fn normalize_backtest_strategy_name(name: &str) -> Result<&'static str> {
    match name {
        "momentum" => Ok("momentum"),
        "directional" => Ok("directional"),
        "prob-garch" | "prob_garch" => Ok("prob-garch"),
        "liquidity-vacuum" | "liquidity_vacuum" => Ok("liquidity-vacuum"),
        "staggered-arb" | "staggered_arb" => Ok("staggered-arb"),
        "gamma_scalping" | "gamma-scalping" => Ok("gamma_scalping"),
        "pm_5m_directional" | "pm-5m-directional" | "pm5m-directional" | "pm5m_directional" => {
            Ok("pm_5m_directional")
        }
        other => anyhow::bail!(
            "Unknown backtest strategy: '{}'. Supported: momentum, directional, prob-garch (alias: prob_garch), liquidity-vacuum (alias: liquidity_vacuum), staggered-arb (aliases: staggered_arb, gamma_scalping, gamma-scalping), pm_5m_directional (aliases: pm-5m-directional, pm5m-directional, pm5m_directional)",
            other
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_backtest(
    name: &str,
    mode: StrategyBacktestMode,
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    capital: f64,
    save: bool,
    json_output: bool,
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    skip_gamma: bool,
    verify_run: Option<String>,
    diagnose_db: bool,
    pm5_auto_trim_window: bool,
    database_url: Option<String>,
    lv_profile: Option<LiquidityVacuumProfile>,
    lv_price_move_threshold: Option<f64>,
    lv_volume_multiplier_threshold: Option<f64>,
    lv_order_concentration_threshold: Option<f64>,
    lv_entry_deviation_threshold: Option<f64>,
    lv_entry_zscore_threshold: Option<f64>,
    lv_take_profit_zscore_threshold: Option<f64>,
    lv_stop_loss_zscore_threshold: Option<f64>,
    lv_take_profit_ema_band_pct: Option<f64>,
    lv_stop_loss_pct: Option<f64>,
    lv_min_edge_buffer: Option<f64>,
    lv_zscore_lookback_samples: Option<usize>,
    lv_max_holding_secs: Option<u64>,
    sa_entry_after_start_max_secs: Option<u64>,
) -> Result<()> {
    use chrono::DateTime;
    use rust_decimal::prelude::*;
    use rust_decimal_macros::dec;

    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_feed::HistoricalFeed;
    use crate::strategy::backtest_recorder::{NullRecorder, PgBacktestRecorder};
    use crate::strategy::backtest_report;
    use crate::strategy::directional_backtest::{
        DirectionalBacktestConfig, DirectionalBacktestEngine,
    };
    use crate::strategy::garch_probability_backtest::{
        GarchProbabilityBacktestConfig, GarchProbabilityBacktestEngine,
    };
    use crate::strategy::liquidity_vacuum_backtest::{
        LiquidityVacuumBacktestConfig, LiquidityVacuumBacktestEngine,
    };
    use crate::strategy::momentum_backtest::{MomentumBacktestConfig, MomentumBacktestEngine};
    use crate::strategy::pm_5m_directional_backtest::{
        Pm5mDirectionalBacktestConfig, Pm5mDirectionalBacktestEngine,
    };

    let canonical_name = normalize_backtest_strategy_name(name)?;

    if mode == StrategyBacktestMode::Settlement {
        if canonical_name != "directional" {
            anyhow::bail!("Settlement mode is only supported for directional strategy");
        }
        if json_output {
            warn!("--json is not supported in settlement mode yet; falling back to text output");
        }
        if save {
            warn!("--save has no effect in settlement mode");
        }
        return backtest_directional_signals_pm_settlement(
            lookback_hours,
            account_id,
            agent_id,
            live_only,
            limit,
            no_refresh,
            database_url,
        )
        .await;
    }

    // Handle --verify-run: load and print an existing report
    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    if let Some(ref run_id_str) = verify_run {
        let run_id: uuid::Uuid = run_id_str.parse().context("Invalid run UUID")?;
        let store = PostgresStore::new(&db_url, 5).await?;
        let report = backtest_report::load_report(store.pool(), run_id).await?;
        if json_output {
            println!("{}", report.to_json()?);
        } else {
            println!("{}", report.print_report());
        }
        return Ok(());
    }

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();

    let from_dt = from
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --from date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let to_dt = to
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --to date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&chrono::Utc));

    // Unified backtest feed path: database only.
    let store = PostgresStore::new(&db_url, 5).await?;
    if diagnose_db {
        print_backtest_db_diagnostics(store.pool(), &symbol_list, from_dt, to_dt).await?;
        return Ok(());
    }
    let pm5_replay_window = if canonical_name == "pm_5m_directional" {
        let resolved = resolve_pm5_replay_window(
            store.pool(),
            &symbol_list,
            from_dt,
            to_dt,
            pm5_auto_trim_window,
        )
        .await?;
        if let Some(message) = resolved.auto_trim_message.as_deref() {
            warn!("{message}");
        }
        Some(resolved)
    } else {
        None
    };
    let (effective_from, effective_to) = pm5_replay_window
        .as_ref()
        .map(|window| (window.from, window.to))
        .unwrap_or((from_dt, to_dt));
    info!("Loading historical data from database");
    let mut feed =
        HistoricalFeed::from_database(store.pool(), &symbol_list, effective_from, effective_to).await?;

    let initial_capital = Decimal::from_f64(capital).unwrap_or_else(|| Decimal::new(10000, 0));

    let results = match canonical_name {
        "directional" => {
            let mut config = DirectionalBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            // Create recorder: PgBacktestRecorder if --save, else NullRecorder
            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "directional",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = DirectionalBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            // Print directional-specific summary (includes exit reasons, calibration)
            if !json_output {
                engine.print_directional_summary();
            }

            // Finalize recorder with summary metrics if saving
            if save {
                // Take the recorder back from the engine and downcast to PgBacktestRecorder
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                // Load and print report
                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "pm_5m_directional" => {
            let mut config = Pm5mDirectionalBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = build_pm5_backtest_config_json(
                    &config,
                    from_dt,
                    to_dt,
                    pm5_auto_trim_window,
                    pm5_replay_window.as_ref(),
                );
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "pm_5m_directional",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(
                    run_id = %pg_recorder.run_id(),
                    "Recording pm_5m_directional backtest signals to DB"
                );
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = Pm5mDirectionalBacktestEngine::new(config, recorder)?;
            let results = engine.run(&mut feed);

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    let replay_start = pm5_replay_window.as_ref().and_then(|window| window.from);
                    let replay_end = pm5_replay_window.as_ref().and_then(|window| window.to);
                    pg.finalize(
                        replay_start.or(Some(results.start_time)),
                        replay_end.or(Some(results.end_time)),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");
                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "prob-garch" | "prob_garch" => {
            let mut config = GarchProbabilityBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;
            // PM BTC up/down 5m events only by default
            config.allowed_window_durations = vec![300];

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "prob_garch",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording prob_garch backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = GarchProbabilityBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "liquidity-vacuum" | "liquidity_vacuum" => {
            let mut config = LiquidityVacuumBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            match lv_profile.unwrap_or(LiquidityVacuumProfile::Prod) {
                LiquidityVacuumProfile::Prod => {}
                LiquidityVacuumProfile::Research => {
                    // Looser exploratory thresholds to discover candidate regimes quickly.
                    config.price_move_threshold = dec!(0.003);
                    config.volume_multiplier_threshold = dec!(1.2);
                    config.order_concentration_threshold = dec!(0.15);
                    // Deviation gate uses z-score in research mode.
                    config.entry_deviation_threshold = Decimal::ZERO;
                    config.entry_zscore_threshold = dec!(1.2);
                    config.take_profit_zscore_threshold = dec!(0.3);
                    config.stop_loss_zscore_threshold = dec!(2.5);
                    config.zscore_lookback_samples = 180;
                    config.max_holding_secs = 900;
                    config.max_spread_bps = 3000;
                }
                LiquidityVacuumProfile::ResearchV2 => {
                    // Research preset tuned for better trade quality / count balance
                    // on short-dated binary contracts.
                    config.price_move_threshold = dec!(0.0015);
                    config.volume_multiplier_threshold = dec!(0.9);
                    config.order_concentration_threshold = dec!(0.10);
                    config.entry_deviation_threshold = Decimal::ZERO;
                    config.entry_zscore_threshold = dec!(0.40);
                    config.take_profit_zscore_threshold = Decimal::ZERO;
                    config.stop_loss_zscore_threshold = Decimal::ZERO;
                    config.take_profit_ema_band_pct = dec!(0.10);
                    config.stop_loss_pct = dec!(0.35);
                    config.min_edge_buffer = dec!(0.018);
                    config.zscore_lookback_samples = 180;
                    config.max_holding_secs = 7200;
                    config.max_spread_bps = 3000;
                }
            }

            if let Some(v) = lv_price_move_threshold {
                config.price_move_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-price-move-threshold value")?;
            }
            if let Some(v) = lv_volume_multiplier_threshold {
                config.volume_multiplier_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-volume-multiplier-threshold value")?;
            }
            if let Some(v) = lv_order_concentration_threshold {
                config.order_concentration_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-order-concentration-threshold value")?;
            }
            if let Some(v) = lv_entry_deviation_threshold {
                config.entry_deviation_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-entry-deviation-threshold value")?;
            }
            if let Some(v) = lv_entry_zscore_threshold {
                config.entry_zscore_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-entry-zscore-threshold value")?;
            }
            if let Some(v) = lv_take_profit_zscore_threshold {
                config.take_profit_zscore_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-take-profit-zscore-threshold value")?;
            }
            if let Some(v) = lv_stop_loss_zscore_threshold {
                config.stop_loss_zscore_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-stop-loss-zscore-threshold value")?;
            }
            if let Some(v) = lv_take_profit_ema_band_pct {
                config.take_profit_ema_band_pct =
                    Decimal::from_f64(v).context("Invalid --lv-take-profit-ema-band-pct value")?;
            }
            if let Some(v) = lv_stop_loss_pct {
                config.stop_loss_pct =
                    Decimal::from_f64(v).context("Invalid --lv-stop-loss-pct value")?;
            }
            if let Some(v) = lv_min_edge_buffer {
                config.min_edge_buffer =
                    Decimal::from_f64(v).context("Invalid --lv-min-edge-buffer value")?;
            }
            if let Some(v) = lv_zscore_lookback_samples {
                config.zscore_lookback_samples = v.max(2);
            }
            if let Some(v) = lv_max_holding_secs {
                config.max_holding_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "liquidity_vacuum",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording liquidity-vacuum backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = LiquidityVacuumBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_liquidity_vacuum_summary();
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "staggered-arb" | "staggered_arb" => {
            use crate::strategy::staggered_arb_backtest::{
                StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
            };

            let config_path = PathBuf::from("config/strategies/staggered_arb.toml");
            let config_content = fs::read_to_string(&config_path).with_context(|| {
                format!(
                    "Failed to read staggered-arb backtest config from {}",
                    config_path.display()
                )
            })?;
            let mut config = StaggeredArbBacktestConfig::from_toml_str(&config_content)?;
            config.initial_capital = initial_capital;
            if !symbol_list.is_empty() {
                config.symbols = symbol_list.clone();
            }
            if let Some(v) = sa_entry_after_start_max_secs {
                config.entry_after_start_max_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "staggered-arb",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording staggered-arb backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = StaggeredArbBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_staggered_summary();
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "gamma_scalping" | "gamma-scalping" => {
            use crate::strategy::staggered_arb_backtest::{
                StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
            };

            let mut config = StaggeredArbBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;
            // PM 5m events only
            config.allowed_window_durations = vec![300];
            if let Some(v) = sa_entry_after_start_max_secs {
                config.entry_after_start_max_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "gamma_scalping",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording gamma_scalping backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = StaggeredArbBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_summary("Gamma Scalping (PM 5m)");
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        _ => {
            let config =
                MomentumBacktestConfig::default_with_symbols(symbol_list.clone(), initial_capital);
            let mut engine = MomentumBacktestEngine::new(config);
            let results = engine.run(&mut feed);

            // Optionally save momentum results to DB
            if save {
                crate::strategy::momentum_backtest::save_backtest_results(
                    store.pool(),
                    &engine.config(),
                    &results,
                )
                .await?;
                info!("Backtest results saved to database");
            }
            results
        }
    };

    if json_output && !save {
        // Only print raw JSON if we didn't already print a report above
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else if !json_output && !save {
        println!("{}", results);
    }

    Ok(())
}

fn build_pm5_backtest_config_json(
    config: &crate::strategy::pm_5m_directional_backtest::Pm5mDirectionalBacktestConfig,
    requested_from: Option<chrono::DateTime<chrono::Utc>>,
    requested_to: Option<chrono::DateTime<chrono::Utc>>,
    auto_trim_requested: bool,
    replay_window: Option<&Pm5mReplayWindow>,
) -> serde_json::Value {
    let mut config_json = serde_json::to_value(config).unwrap_or_default();
    if let Some(map) = config_json.as_object_mut() {
        map.insert(
            "replay_window".to_string(),
            serde_json::json!({
                "requested_from": requested_from.map(|ts| ts.to_rfc3339()),
                "requested_to": requested_to.map(|ts| ts.to_rfc3339()),
                "effective_from": replay_window.and_then(|window| window.from).map(|ts| ts.to_rfc3339()),
                "effective_to": replay_window.and_then(|window| window.to).map(|ts| ts.to_rfc3339()),
                "auto_trim_requested": auto_trim_requested,
                "auto_trim_applied": replay_window
                    .and_then(|window| window.auto_trim_message.as_ref())
                    .is_some(),
                "auto_trim_message": replay_window.and_then(|window| window.auto_trim_message.clone()),
            }),
        );
    }
    config_json
}

#[cfg(test)]
mod tests {
    use super::normalize_backtest_strategy_name;

    #[test]
    fn normalize_backtest_strategy_name_accepts_pm_5m_directional_aliases() {
        assert_eq!(
            normalize_backtest_strategy_name("pm_5m_directional")
                .expect("canonical alias should parse"),
            "pm_5m_directional"
        );
        assert_eq!(
            normalize_backtest_strategy_name("pm-5m-directional")
                .expect("dash alias should parse"),
            "pm_5m_directional"
        );
        assert_eq!(
            normalize_backtest_strategy_name("pm5m_directional")
                .expect("compact alias should parse"),
            "pm_5m_directional"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Backtest list handler
// ─────────────────────────────────────────────────────────────
