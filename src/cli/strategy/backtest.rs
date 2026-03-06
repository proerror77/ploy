use anyhow::{Context, Result};
use std::io::Write;
use tracing::{info, warn};

use crate::cli::strategy::{self, StrategyBacktestMode};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_backtest(
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
    database_url: Option<String>,
) -> Result<()> {
    use chrono::DateTime;
    use ploy_backtest::report as backtest_report;
    use ploy_backtest::{BacktestRecorder, HistoricalFeed, NullRecorder, PgBacktestRecorder};
    use rust_decimal::prelude::*;

    use crate::adapters::PostgresStore;
    use crate::strategy::directional_backtest::{
        DirectionalBacktestConfig, DirectionalBacktestEngine,
    };
    use crate::strategy::momentum_backtest::{MomentumBacktestConfig, MomentumBacktestEngine};

    match name {
        "momentum" | "directional" | "staggered-arb" => {}
        other => anyhow::bail!(
            "Unknown backtest strategy: '{}'. Supported: momentum, directional, staggered-arb",
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
        return strategy::backtest_directional_signals_pm_settlement(
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

    let store = PostgresStore::new(&db_url, 5).await?;
    info!("Loading historical data from database");
    let mut feed =
        HistoricalFeed::from_database(store.pool(), &symbol_list, from_dt, to_dt).await?;

    let initial_capital = Decimal::from_f64(capital).unwrap_or_else(|| Decimal::new(10000, 0));

    let results = match name {
        "directional" => {
            let mut config = DirectionalBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn BacktestRecorder> = if save {
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

            if !json_output {
                engine.print_directional_summary();
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
        "staggered-arb" => {
            use crate::strategy::staggered_arb_backtest::{
                StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
            };

            let mut config = StaggeredArbBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn BacktestRecorder> = if save {
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
        _ => {
            let config =
                MomentumBacktestConfig::default_with_symbols(symbol_list.clone(), initial_capital);
            let mut engine = MomentumBacktestEngine::new(config);
            let results = engine.run(&mut feed);

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
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else if !json_output && !save {
        println!("{}", results.report());
    }

    Ok(())
}

/// Verify backtest trades against Polymarket official settlement via Gamma API.
///
/// 1. Map backtest trades (symbol + entry_time) -> token_ids via pm_market_metadata
/// 2. Refresh unresolved tokens via Gamma API -> pm_token_settlements
/// 3. Update backtest_trades with gamma_settled_price, gamma_resolved, gamma_match
async fn verify_backtest_trades_gamma(pool: &sqlx::PgPool, run_id: uuid::Uuid) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

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

    let slugs_vec: Vec<String> = slugs_needed.into_iter().collect();
    let existing_settlements = sqlx::query(
        "SELECT token_id, market_slug, outcome, resolved, settled_price
         FROM pm_token_settlements WHERE market_slug = ANY($1)",
    )
    .bind(&slugs_vec)
    .fetch_all(pool)
    .await?;

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

    let mut unresolved_tokens: Vec<String> = Vec::new();
    for settlements in slug_settlements.values() {
        for info in settlements.values() {
            if !info.resolved {
                unresolved_tokens.push(info.token_id.clone());
            }
        }
    }

    let mut missing_slugs: Vec<&str> = Vec::new();
    for slug in &slugs_vec {
        if !slug_settlements.contains_key(slug) {
            missing_slugs.push(slug);
        }
    }

    if !missing_slugs.is_empty() {
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

            let resolved = market.closed.unwrap_or(false) && strategy::is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
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

    let mut verified = 0usize;
    let mut matched = 0usize;
    let mut mismatched = 0usize;

    for mapping in &mappings {
        let Some(outcomes) = slug_settlements.get(&mapping.market_slug) else {
            continue;
        };

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

pub(crate) async fn run_backtest_list(database_url: Option<String>, limit: usize) -> Result<()> {
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

pub(crate) async fn run_backtest_diff(
    run1: &str,
    run2: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use ploy_backtest::report as backtest_report;

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

/// Backfill Binance klines into the database for historical backtesting.
pub(crate) async fn backfill_klines(
    symbols: &str,
    from: &str,
    to: &str,
    interval: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::collector::BinanceKlineClient;
    use chrono::DateTime;

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();
    if symbol_list.is_empty() {
        anyhow::bail!("No symbols provided");
    }

    let from_dt = DateTime::parse_from_rfc3339(from)
        .or_else(|_| DateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --from date (expected ISO 8601, e.g. 2026-02-20T00:00:00Z)")?;

    let to_dt = DateTime::parse_from_rfc3339(to)
        .or_else(|_| DateTime::parse_from_str(to, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --to date (expected ISO 8601, e.g. 2026-02-28T00:00:00Z)")?;

    if to_dt <= from_dt {
        anyhow::bail!("--to must be after --from");
    }

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_klines (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            interval TEXT NOT NULL,
            open_time TIMESTAMPTZ NOT NULL,
            close_time TIMESTAMPTZ NOT NULL,
            open NUMERIC(20,10) NOT NULL,
            high NUMERIC(20,10) NOT NULL,
            low NUMERIC(20,10) NOT NULL,
            close NUMERIC(20,10) NOT NULL,
            volume NUMERIC(20,10) NOT NULL,
            quote_volume NUMERIC(20,10) NOT NULL,
            trades BIGINT NOT NULL DEFAULT 0,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (symbol, interval, open_time)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure binance_klines table")?;

    let client = BinanceKlineClient::new();

    println!(
        "\nBackfilling klines: {} symbols, interval={}, {} -> {}",
        symbol_list.len(),
        interval,
        from_dt.format("%Y-%m-%d"),
        to_dt.format("%Y-%m-%d")
    );

    let mut grand_total = 0usize;
    for sym in &symbol_list {
        print!("  {} ... ", sym);
        std::io::stdout().flush().ok();

        let klines = client
            .fetch_klines_range(sym, interval, from_dt, to_dt)
            .await
            .with_context(|| format!("Failed to fetch klines for {}", sym))?;

        let fetched = klines.len();
        let saved = BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines)
            .await
            .with_context(|| format!("Failed to save klines for {}", sym))?;

        println!("{} fetched, {} new", fetched, saved);
        grand_total += saved;
    }

    println!("\nDone. {} new klines inserted total.\n", grand_total);
    Ok(())
}
