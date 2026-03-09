use super::*;

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

    match name {
            "momentum"
            | "directional"
            | "prob-garch"
            | "prob_garch"
            | "liquidity-vacuum"
            | "liquidity_vacuum"
            | "staggered-arb"
            | "staggered_arb"
            | "gamma_scalping"
            | "gamma-scalping" => {}
            other => anyhow::bail!(
            "Unknown backtest strategy: '{}'. Supported: momentum, directional, prob-garch (alias: prob_garch), liquidity-vacuum (alias: liquidity_vacuum), staggered-arb (aliases: staggered_arb, gamma_scalping, gamma-scalping)",
            other
        ),
    }

    if mode == StrategyBacktestMode::Settlement {
        if name != "directional" {
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
    info!("Loading historical data from database");
    let mut feed =
        HistoricalFeed::from_database(store.pool(), &symbol_list, from_dt, to_dt).await?;

    let initial_capital = Decimal::from_f64(capital).unwrap_or_else(|| Decimal::new(10000, 0));

    let results = match name {
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

async fn print_backtest_db_diagnostics(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    use chrono::{DateTime, Utc};

    fn fmt_ts(ts: Option<DateTime<Utc>>) -> String {
        ts.map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    }

    async fn table_exists(pool: &sqlx::PgPool, table: &str) -> Result<bool> {
        let reg: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(format!("public.{table}"))
            .fetch_one(pool)
            .await?;
        Ok(reg.is_some())
    }

    println!("\n=== Backtest DB diagnostics ===");
    println!("symbols: {}", symbols.join(", "));
    println!("from: {}", fmt_ts(from));
    println!("to:   {}", fmt_ts(to));

    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

    // ── sync_records (best: integrated BN+PM view) ───────────
    if !table_exists(pool, "sync_records").await? {
        println!("\n[sync_records] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(timestamp),
              MAX(timestamp),
              COUNT(DISTINCT pm_market_slug)::bigint
            FROM sync_records
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR timestamp >= $2)
              AND ($3::timestamptz IS NULL OR timestamp <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, slugs)) => {
                println!("\n[sync_records]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct pm_market_slug: {slugs}");
            }
            Err(e) => {
                println!("\n[sync_records] query failed: {e}");
            }
        }
    }

    // ── binance_price_ticks (fallback spot) ──────────────────
    if !table_exists(pool, "binance_price_ticks").await? {
        println!("\n[binance_price_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT COUNT(*)::bigint, MIN(trade_time), MAX(trade_time)
            FROM binance_price_ticks
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR trade_time >= $2)
              AND ($3::timestamptz IS NULL OR trade_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts)) => {
                println!("\n[binance_price_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[binance_price_ticks] query failed: {e}");
            }
        }
    }

    // ── binance_klines (supplement spot) ─────────────────────
    if !table_exists(pool, "binance_klines").await? {
        println!("\n[binance_klines] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(open_time),
              MAX(close_time),
              COUNT(DISTINCT interval)::bigint
            FROM binance_klines
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR close_time >= $2)
              AND ($3::timestamptz IS NULL OR open_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, intervals)) => {
                println!("\n[binance_klines]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct intervals: {intervals}");
            }
            Err(e) => {
                println!("\n[binance_klines] query failed: {e}");
            }
        }
    }

    // ── clob_quote_ticks (PM quotes) ─────────────────────────
    if !table_exists(pool, "clob_quote_ticks").await? {
        println!("\n[clob_quote_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_quote_ticks
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_quote_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_quote_ticks] query failed: {e}");
            }
        }
    }

    // ── clob_orderbook_snapshots (PM depth) ──────────────────
    if !table_exists(pool, "clob_orderbook_snapshots").await? {
        println!("\n[clob_orderbook_snapshots] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_orderbook_snapshots
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_orderbook_snapshots]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_orderbook_snapshots] query failed: {e}");
            }
        }
    }

    // ── pm_market_metadata (event windows) ───────────────────
    if !table_exists(pool, "pm_market_metadata").await? {
        println!("\n[pm_market_metadata] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(*) FILTER (WHERE start_time IS NOT NULL AND end_time IS NOT NULL)::bigint,
              COUNT(*) FILTER (WHERE price_to_beat IS NOT NULL AND price_to_beat > 0)::bigint,
              MIN(start_time),
              MAX(end_time)
            FROM pm_market_metadata
            WHERE ($1::timestamptz IS NULL OR end_time >= $1)
              AND ($2::timestamptz IS NULL OR start_time <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, windows, with_s0, min_ts, max_ts)) => {
                println!("\n[pm_market_metadata]");
                println!("rows: {count}, window_rows: {windows}, with price_to_beat>0: {with_s0}");
                println!("ts_range: {} .. {}", fmt_ts(min_ts), fmt_ts(max_ts));
            }
            Err(e) => {
                println!("\n[pm_market_metadata] query failed: {e}");
            }
        }
    }

    // ── pm_token_settlements (token→slug mapping + outcomes) ─
    if !table_exists(pool, "pm_token_settlements").await? {
        println!("\n[pm_token_settlements] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(DISTINCT market_slug)::bigint,
              COUNT(*) FILTER (WHERE resolved = true)::bigint,
              MIN(resolved_at),
              MAX(resolved_at)
            FROM pm_token_settlements
            WHERE ($1::timestamptz IS NULL OR resolved_at >= $1 OR resolved_at IS NULL)
              AND ($2::timestamptz IS NULL OR resolved_at <= $2 OR resolved_at IS NULL)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, slugs, resolved, min_ts, max_ts)) => {
                println!("\n[pm_token_settlements]");
                println!("rows: {count}, distinct market_slug: {slugs}, resolved_rows: {resolved}");
                println!(
                    "resolved_at range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[pm_token_settlements] query failed: {e}");
            }
        }
    }

    // ── deribit_iv_ticks (Deribit IV baseline) ──────────────
    if !table_exists(pool, "deribit_iv_ticks").await? {
        println!("\n[deribit_iv_ticks] MISSING");
    } else {
        let mut printed = false;

        if let Ok((count, min_ts, max_ts, ccy)) =
            sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(timestamp),
                  MAX(timestamp),
                  COUNT(DISTINCT currency)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR timestamp >= $1)
                  AND ($2::timestamptz IS NULL OR timestamp <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
        {
            printed = true;
            println!("\n[deribit_iv_ticks]");
            println!(
                "rows: {count}, ts_range: {} .. {}",
                fmt_ts(min_ts),
                fmt_ts(max_ts)
            );
            println!("distinct currency: {ccy}");
        }

        if !printed {
            match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(ts),
                  MAX(ts),
                  COUNT(DISTINCT symbol)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR ts >= $1)
                  AND ($2::timestamptz IS NULL OR ts <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
            {
                Ok((count, min_ts, max_ts, symbols)) => {
                    println!("\n[deribit_iv_ticks]");
                    println!(
                        "rows: {count}, ts_range: {} .. {}",
                        fmt_ts(min_ts),
                        fmt_ts(max_ts)
                    );
                    println!("distinct symbol: {symbols}");
                }
                Err(e) => {
                    println!("\n[deribit_iv_ticks] query failed: {e}");
                }
            }
        }
    }

    println!("\nHint:");
    println!("- PM 5m backtest needs: clob_quote_ticks + pm_market_metadata (or pm_token_settlements.raw_market) + spot (sync_records or binance_price_ticks/klines).");
    println!("- Deribit IV (optional): populate deribit_iv_ticks (e.g. `ploy deribit-iv-backfill`) to enable IV-aware research/backtests.");

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Gamma verification for backtest trades
// ─────────────────────────────────────────────────────────────

/// Verify backtest trades against Polymarket official settlement via Gamma API.
///
/// 1. Map backtest trades (symbol + entry_time) → token_ids via pm_market_metadata
/// 2. Refresh unresolved tokens via Gamma API → pm_token_settlements
/// 3. Update backtest_trades with gamma_settled_price, gamma_resolved, gamma_match
async fn verify_backtest_trades_gamma(pool: &sqlx::PgPool, run_id: uuid::Uuid) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    crate::persistence::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    // 1. Load trades for this run
    let trade_rows = sqlx::query(
        "SELECT id, symbol, direction, entry_time, exit_time, exit_reason, won
         FROM backtest_trades WHERE run_id = $1 ORDER BY entry_time",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .context("Failed to load backtest trades")?;

    if trade_rows.is_empty() {
        info!("No trades to verify");
        return Ok(());
    }

    // 2. Map trades to token_ids via pm_market_metadata
    //    Each trade's symbol + entry_time falls within a specific market window
    //    pm_market_metadata has: market_slug, symbol, start_time, end_time
    //    pm_token_settlements has: token_id, market_slug, outcome, settled_price
    struct TradeMapping {
        trade_id: i64,
        won: bool,
        direction: String,
        market_slug: String,
    }

    let mut mappings: Vec<TradeMapping> = Vec::new();
    let mut slugs_needed: HashSet<String> = HashSet::new();

    for row in &trade_rows {
        let trade_id: i64 = row.get("id");
        let symbol: String = row.get("symbol");
        let direction: String = row.get("direction");
        let entry_time: DateTime<Utc> = row.get("entry_time");
        let won: bool = row.get("won");

        // Find the market window that contains this trade's entry_time
        let slug_row = sqlx::query_scalar::<_, String>(
            "SELECT market_slug FROM pm_market_metadata
             WHERE symbol = $1 AND start_time <= $2 AND end_time >= $2
             LIMIT 1",
        )
        .bind(&symbol)
        .bind(entry_time)
        .fetch_optional(pool)
        .await?;

        if let Some(slug) = slug_row {
            slugs_needed.insert(slug.clone());
            mappings.push(TradeMapping {
                trade_id,
                won,
                direction,
                market_slug: slug,
            });
        }
    }

    if mappings.is_empty() {
        info!("No trades could be mapped to market slugs");
        return Ok(());
    }

    // 3. Collect token_ids for these slugs from pm_token_settlements
    let slugs_vec: Vec<String> = slugs_needed.into_iter().collect();
    let existing_settlements = sqlx::query(
        "SELECT token_id, market_slug, outcome, resolved, settled_price
         FROM pm_token_settlements WHERE market_slug = ANY($1)",
    )
    .bind(&slugs_vec)
    .fetch_all(pool)
    .await?;

    // Build slug → {outcome → (token_id, resolved, settled_price)}
    struct SettlementInfo {
        token_id: String,
        resolved: bool,
        settled_price: Option<Decimal>,
    }
    let mut slug_settlements: HashMap<String, HashMap<String, SettlementInfo>> = HashMap::new();
    for row in &existing_settlements {
        let slug: String = row.get("market_slug");
        let outcome: Option<String> = row.get("outcome");
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        let settled_price: Option<Decimal> = row.get("settled_price");
        if let Some(outcome) = outcome {
            slug_settlements.entry(slug).or_default().insert(
                outcome,
                SettlementInfo {
                    token_id,
                    resolved,
                    settled_price,
                },
            );
        }
    }

    // 4. Find unresolved token_ids that need Gamma refresh
    let mut unresolved_tokens: Vec<String> = Vec::new();
    for settlements in slug_settlements.values() {
        for info in settlements.values() {
            if !info.resolved {
                unresolved_tokens.push(info.token_id.clone());
            }
        }
    }
    // Also find slugs with NO settlement rows at all
    let mut missing_slugs: Vec<&str> = Vec::new();
    for slug in &slugs_vec {
        if !slug_settlements.contains_key(slug) {
            missing_slugs.push(slug);
        }
    }

    // For missing slugs, try to find token_ids from clob_quote_ticks or pm_market_metadata
    if !missing_slugs.is_empty() {
        // Try to get token_ids from clob_quote_ticks via market_slug join
        let extra_tokens: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT s.token_id FROM pm_token_settlements s
             WHERE s.market_slug = ANY($1) AND s.resolved = false",
        )
        .bind(
            &missing_slugs
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        unresolved_tokens.extend(extra_tokens);
    }

    unresolved_tokens.sort();
    unresolved_tokens.dedup();

    // 5. Refresh via Gamma API
    if !unresolved_tokens.is_empty() {
        const MAX_REFRESH: usize = 500;
        let to_refresh = if unresolved_tokens.len() > MAX_REFRESH {
            &unresolved_tokens[..MAX_REFRESH]
        } else {
            &unresolved_tokens
        };

        println!(
            "\n  Refreshing settlement status for {} token(s) via Gamma...",
            to_refresh.len()
        );

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "gamma fetch failed");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                if !seen_conditions.insert(cond.to_string()) {
                    continue;
                }
            }

            let clob_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default();
            let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
            let price_strs: Vec<String> = market
                .outcome_prices
                .as_ref()
                .map(|ps| ps.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();

            if clob_ids.is_empty() || price_strs.is_empty() {
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            // Store only essential fields, not the full raw_market (avoids "input too long" error)
            let raw_market = serde_json::json!({
                "id": market.id,
                "slug": market.slug,
                "closed": market.closed,
                "condition_id": market.condition_id,
            });

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                let _ = sqlx::query(
                    r#"INSERT INTO pm_token_settlements (
                        token_id, condition_id, market_id, market_slug, outcome,
                        settled_price, resolved, resolved_at, fetched_at, raw_market
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                    ON CONFLICT (token_id) DO UPDATE SET
                        settled_price = EXCLUDED.settled_price,
                        resolved = EXCLUDED.resolved,
                        resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                        fetched_at = NOW(),
                        raw_market = EXCLUDED.raw_market"#,
                )
                .bind(tid)
                .bind(condition_id.as_deref())
                .bind(&market.id)
                .bind(market_slug.as_deref())
                .bind(outcome.as_deref())
                .bind(settled_price)
                .bind(resolved)
                .bind(resolved_at)
                .bind(sqlx::types::Json(raw_market.clone()))
                .execute(pool)
                .await;
            }
            refreshed += 1;
        }

        if refreshed > 0 {
            println!("  Refreshed {} market(s)\n", refreshed);
        }

        // Reload settlements after refresh
        let refreshed_rows = sqlx::query(
            "SELECT token_id, market_slug, outcome, resolved, settled_price
             FROM pm_token_settlements WHERE market_slug = ANY($1)",
        )
        .bind(&slugs_vec)
        .fetch_all(pool)
        .await?;

        slug_settlements.clear();
        for row in &refreshed_rows {
            let slug: String = row.get("market_slug");
            let outcome: Option<String> = row.get("outcome");
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            let settled_price: Option<Decimal> = row.get("settled_price");
            if let Some(outcome) = outcome {
                slug_settlements.entry(slug).or_default().insert(
                    outcome,
                    SettlementInfo {
                        token_id,
                        resolved,
                        settled_price,
                    },
                );
            }
        }
    }

    // 6. Update backtest_trades with gamma verification results
    let mut verified = 0usize;
    let mut matched = 0usize;
    let mut mismatched = 0usize;

    for mapping in &mappings {
        let Some(outcomes) = slug_settlements.get(&mapping.market_slug) else {
            continue;
        };

        // For directional trades: direction "UP" → check "Up" outcome, "DOWN" → check "Down"
        let outcome_key = if mapping.direction == "UP" {
            "Up"
        } else {
            "Down"
        };
        let Some(info) = outcomes.get(outcome_key) else {
            continue;
        };

        if !info.resolved {
            continue;
        }

        let Some(settled_price) = info.settled_price else {
            continue;
        };

        // gamma_match: does the tick-based outcome agree with Gamma settlement?
        // Trade "won" in tick replay ↔ settled_price >= 0.99 for the chosen direction
        let gamma_won = settled_price >= dec!(0.99);
        let gamma_match = mapping.won == gamma_won;

        sqlx::query(
            "UPDATE backtest_trades
             SET gamma_settled_price = $2, gamma_resolved = true, gamma_match = $3
             WHERE id = $1",
        )
        .bind(mapping.trade_id)
        .bind(settled_price)
        .bind(gamma_match)
        .execute(pool)
        .await?;

        verified += 1;
        if gamma_match {
            matched += 1;
        } else {
            mismatched += 1;
        }
    }

    let unverified = mappings.len() - verified;
    println!(
        "  Gamma verification: {} verified ({} matched, {} mismatched), {} unverified\n",
        verified, matched, mismatched, unverified
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backtest list handler
// ─────────────────────────────────────────────────────────────

pub(super) async fn run_backtest_list(database_url: Option<String>, limit: usize) -> Result<()> {
    use crate::adapters::PostgresStore;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        Vec<String>,
        Option<i32>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT run_id, strategy, mode, symbols, total_trades, win_rate,
                total_pnl, sharpe_ratio, max_drawdown, profit_factor, created_at
         FROM backtest_runs ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        println!("No backtest runs found.");
        return Ok(());
    }

    println!(
        "\n  {:<36} {:<14} {:<10} {:<8} {:<7} {:<10} {:<7} {:<7} {}",
        "RUN_ID", "STRATEGY", "MODE", "SYMBOLS", "TRADES", "PNL", "WIN%", "SHARPE", "CREATED"
    );
    println!("  {}", "-".repeat(110));

    for (run_id, strategy, mode, symbols, trades, win_rate, pnl, sharpe, _dd, _pf, created) in &rows
    {
        let sym_str = if symbols.len() > 2 {
            format!("{}+{}", symbols[0], symbols.len() - 1)
        } else {
            symbols.join(",")
        };
        println!(
            "  {:<36} {:<14} {:<10} {:<8} {:<7} ${:<9.2} {:<6.1}% {:<7.2} {}",
            run_id,
            strategy,
            mode,
            sym_str,
            trades.unwrap_or(0),
            pnl.unwrap_or(rust_decimal::Decimal::ZERO),
            win_rate.unwrap_or(0.0) * 100.0,
            sharpe.unwrap_or(0.0),
            created.format("%Y-%m-%d %H:%M"),
        );
    }
    println!();

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backtest diff handler
// ─────────────────────────────────────────────────────────────

pub(super) async fn run_backtest_diff(
    run1: &str,
    run2: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let id1: uuid::Uuid = run1.parse().context("Invalid run1 UUID")?;
    let id2: uuid::Uuid = run2.parse().context("Invalid run2 UUID")?;

    let r1 = backtest_report::load_report(store.pool(), id1).await?;
    let r2 = backtest_report::load_report(store.pool(), id2).await?;

    let w = 64;
    let bar = "=".repeat(w);
    let thin = "-".repeat(w);

    println!("\n{}", bar);
    println!("  BACKTEST COMPARISON");
    println!("{}\n", bar);

    println!("  {:<24} {:<20} {:<20}", "METRIC", "RUN A", "RUN B");
    println!("  {}", thin);
    println!(
        "  {:<24} {:<20} {:<20}",
        "Run ID",
        &r1.run.run_id.to_string()[..8],
        &r2.run.run_id.to_string()[..8]
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Strategy", r1.run.strategy, r2.run.strategy
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Trades", r1.run.total_trades, r2.run.total_trades
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Win Rate",
        format!("{:.1}%", r1.run.win_rate * 100.0),
        format!("{:.1}%", r2.run.win_rate * 100.0)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "PnL",
        format!("${:.2}", r1.run.total_pnl),
        format!("${:.2}", r2.run.total_pnl)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Sharpe",
        format!("{:.2}", r1.run.sharpe_ratio),
        format!("{:.2}", r2.run.sharpe_ratio)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Max Drawdown",
        format!(
            "{:.2}%",
            r1.run.max_drawdown * rust_decimal_macros::dec!(100)
        ),
        format!(
            "{:.2}%",
            r2.run.max_drawdown * rust_decimal_macros::dec!(100)
        )
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Profit Factor",
        format!("{:.2}", r1.run.profit_factor),
        format!("{:.2}", r2.run.profit_factor)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Fee Drag",
        format!("{:.1}%", r1.fee_impact.fee_drag_pct),
        format!("{:.1}%", r2.fee_impact.fee_drag_pct)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Calibration Bias",
        format!("{:+.1}%", r1.calibration.overall_bias * 100.0),
        format!("{:+.1}%", r2.calibration.overall_bias * 100.0)
    );
    println!("\n{}\n", bar);

    Ok(())
}

pub(super) async fn run_live_backtest_compare(
    run_id: &str,
    lookback_hours: u64,
    account_id: Option<String>,
    strategy_id: Option<String>,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use sqlx::Row;
    use std::collections::HashSet;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    crate::persistence::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;

    let bt_run_id: uuid::Uuid = run_id.parse().context("Invalid run UUID")?;
    let report = backtest_report::load_report(store.pool(), bt_run_id).await?;

    let signal_types = vec![
        "live_order_submit_result".to_string(),
        "live_order_poll_update".to_string(),
        "live_order_rejected".to_string(),
        "live_order_submit_error".to_string(),
    ];

    let rows = sqlx::query(
        r#"
        SELECT
            signal_type,
            side,
            fair_value,
            market_price,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = ANY($2)
          AND ($3::text IS NULL OR account_id = $3)
          AND ($4::text IS NULL OR strategy_id = $4)
        ORDER BY recorded_at DESC
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(&signal_types)
    .bind(account_id.as_deref())
    .bind(strategy_id.as_deref())
    .fetch_all(store.pool())
    .await
    .context("Failed to query live order observations from signal_history")?;

    let mut submitted: HashSet<String> = HashSet::new();
    let mut rejected: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut touched_fill: HashSet<String> = HashSet::new();
    let mut full_fill: HashSet<String> = HashSet::new();
    let mut slippage_bps_weighted_sum = 0.0f64;
    let mut slippage_weight = 0.0f64;

    for row in rows {
        let signal_type: String = row.get("signal_type");
        let side: Option<String> = row.get("side");
        let limit_price: Option<Decimal> = row.get("fair_value");
        let fill_price: Option<Decimal> = row.get("market_price");
        let context: serde_json::Value = row.get("context");

        let order_key = context
            .get("client_order_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| {
                context
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
            });

        let Some(order_key) = order_key else { continue };
        let status = context
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let filled_qty = context
            .get("filled_qty")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match signal_type.as_str() {
            "live_order_submit_result" => {
                submitted.insert(order_key.clone());
            }
            "live_order_rejected" => {
                submitted.insert(order_key.clone());
                rejected.insert(order_key.clone());
            }
            "live_order_submit_error" => {
                submitted.insert(order_key.clone());
                failed.insert(order_key.clone());
            }
            _ => {}
        }

        if filled_qty > 0
            || status.eq_ignore_ascii_case("filled")
            || status.eq_ignore_ascii_case("partiallyfilled")
        {
            touched_fill.insert(order_key.clone());
        }
        if status.eq_ignore_ascii_case("filled") {
            full_fill.insert(order_key.clone());
        }

        if filled_qty > 0 {
            if let (Some(limit_px), Some(fill_px)) = (limit_price, fill_price) {
                if limit_px > Decimal::ZERO {
                    if let (Some(limit_f64), Some(fill_f64)) = (limit_px.to_f64(), fill_px.to_f64())
                    {
                        let side_lower = side.unwrap_or_else(|| "buy".to_string()).to_lowercase();
                        let slip_bps = if side_lower == "sell" {
                            (limit_f64 - fill_f64) / limit_f64 * 10_000.0
                        } else {
                            (fill_f64 - limit_f64) / limit_f64 * 10_000.0
                        };
                        let weight = filled_qty as f64;
                        slippage_bps_weighted_sum += slip_bps * weight;
                        slippage_weight += weight;
                    }
                }
            }
        }
    }

    let submitted_n = submitted.len();
    let rejected_n = rejected.len();
    let failed_n = failed.len();
    let touched_fill_n = touched_fill.len();
    let full_fill_n = full_fill.len();

    let live_fill_rate = if submitted_n > 0 {
        touched_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_full_fill_rate = if submitted_n > 0 {
        full_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_reject_rate = if submitted_n > 0 {
        rejected_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_failed_rate = if submitted_n > 0 {
        failed_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let avg_slippage_bps = if slippage_weight > 0.0 {
        slippage_bps_weighted_sum / slippage_weight
    } else {
        0.0
    };

    let bt_trades = report.run.total_trades.max(0) as usize;
    let live_vs_bt_trade_ratio = if bt_trades > 0 {
        touched_fill_n as f64 / bt_trades as f64
    } else {
        0.0
    };

    println!("\n{}", "=".repeat(78));
    println!("  LIVE VS BACKTEST");
    println!("{}", "=".repeat(78));
    println!(
        "  backtest_run={}  lookback_hours={}  account_id={}  strategy_id={}",
        report.run.run_id,
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        strategy_id.as_deref().unwrap_or("all")
    );
    println!();
    println!("  Backtest:");
    println!(
        "    strategy={} mode={} trades={} win_rate={:.1}% pnl=${:.2} sharpe={:.2}",
        report.run.strategy,
        report.run.mode,
        report.run.total_trades,
        report.run.win_rate * 100.0,
        report.run.total_pnl,
        report.run.sharpe_ratio
    );
    println!("  Live:");
    println!(
        "    submitted={} touched_fill={} full_fill={} rejected={} failed={}",
        submitted_n, touched_fill_n, full_fill_n, rejected_n, failed_n
    );
    println!(
        "    fill_rate={:.1}% full_fill_rate={:.1}% reject_rate={:.1}% failed_rate={:.1}% avg_slippage_bps={:.2}",
        live_fill_rate * 100.0,
        live_full_fill_rate * 100.0,
        live_reject_rate * 100.0,
        live_failed_rate * 100.0,
        avg_slippage_bps
    );
    println!(
        "  Coverage (live_filled_orders / backtest_trades): {:.2}x",
        live_vs_bt_trade_ratio
    );
    println!();

    Ok(())
}
