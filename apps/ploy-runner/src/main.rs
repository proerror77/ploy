mod collector;
mod discovery;
mod feeds;
mod reference_prices;
mod scanner;
mod sports_feed;

use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ploy_strategy_bundles::feed::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_strategy_bundles::{
    BufferedRecorder, CallbackExecutor, DirectionalStrategy, ExecutionReport, Feed, FullConfig,
    HistoricalFeed, LiveFeed, NullRecorder, RecordedFeed, Recorder, RecordingFeed, RuntimeMode,
    SignalRecord, SimulatedExecutor, StrategyRuntime,
};
use ploy_trading::TradingIntent;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use collector::{CollectorConfig, QuoteCollector};
use feeds::{spawn_chainlink_feed, spawn_db_spot_feed, spawn_pyth_reference_feed, spawn_spot_feed};
use reference_prices::new_reference_price_registry;
use scanner::spawn_market_scanner;
use sports_feed::spawn_sports_feed;

fn print_usage() {
    eprintln!("Usage: ploy-runner [COMMAND] [OPTIONS]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  run               Run the strategy (default)");
    eprintln!("  check-db          Check database data completeness");
    eprintln!("  collect-quotes    Collect orderbook quotes from Polymarket CLOB WebSocket");
    eprintln!();
    eprintln!("Options for 'run':");
    eprintln!("  --config <path>   Unified TOML config file (required)");
    eprintln!("  --dry-run         Force dry-run mode (simulated execution)");
    eprintln!("  --foreground      Run in foreground (default, kept for compat)");
    eprintln!();
    eprintln!("Options for 'check-db':");
    eprintln!(
        "  --db-url <url>    Database URL (default: postgresql://postgres:postgres@localhost:5432/ploy)"
    );
    eprintln!();
    eprintln!("Options for 'collect-quotes':");
    eprintln!("  --symbols <list>  Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT)");
    eprintln!("  --timeframe <tf>  Market timeframe: 5m or 15m (default: 5m)");
    eprintln!(
        "  --db-url <url>    Database URL (default: postgresql://postgres:postgres@localhost:5432/ploy)"
    );
}

async fn check_database(db_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await?;

    println!("=== Database Data Completeness Check ===\n");

    // Check table existence
    let tables = vec![
        "sync_records",
        "binance_price_ticks",
        "clob_quote_ticks",
        "pm_market_metadata",
        "pm_market_catalog",
        "reference_price_ticks",
        "sports_state_events",
        "binance_lob_ticks",
    ];

    for table in &tables {
        let exists: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = '{}')",
            table
        ))
        .fetch_one(&pool)
        .await?;

        println!(
            "Table '{}': {}",
            table,
            if exists { "EXISTS" } else { "MISSING" }
        );
    }

    println!("\n=== Data Range Analysis ===\n");

    let symbols = vec![
        "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT",
    ];

    // Check binance_price_ticks
    println!("--- binance_price_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM binance_price_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            println!(
                "  {}: {} rows, {} to {}",
                symbol,
                count,
                min_ts
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
                max_ts
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "N/A".to_string())
            );
        }
    }

    // Check clob_quote_ticks
    println!("\n--- clob_quote_ticks ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM clob_quote_ticks")
            .fetch_optional(&pool)
            .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!(
            "  Total: {} rows, {} to {}",
            count,
            min_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            max_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string())
        );
    }

    // Check pm_market_metadata
    println!("\n--- pm_market_metadata ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT COUNT(*), MIN(start_time), MAX(end_time) FROM pm_market_metadata")
            .fetch_optional(&pool)
            .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!(
            "  Total: {} markets, {} to {}",
            count,
            min_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            max_ts
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "N/A".to_string())
        );
    }

    // Check binance_lob_ticks
    println!("\n--- binance_lob_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM binance_lob_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            if count > 0 {
                println!(
                    "  {}: {} rows, {} to {}",
                    symbol,
                    count,
                    min_ts
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "N/A".to_string()),
                    max_ts
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "N/A".to_string())
                );
            }
        }
    }

    println!("\n=== Recommendation ===");
    println!("Based on the data ranges above, choose a backtest period where:");
    println!("1. All required symbols have continuous data");
    println!("2. pm_market_metadata has sufficient markets");
    println!("3. clob_quote_ticks has good coverage");

    Ok(())
}

fn prepare_feed_symbols(mode: RuntimeMode, strategy_symbols: &[String]) -> Vec<String> {
    match mode {
        RuntimeMode::Backtest | RuntimeMode::Replay => strategy_symbols.to_vec(),
        RuntimeMode::Live | RuntimeMode::DryRun => strategy_symbols.to_vec(),
    }
}

#[tokio::main]
async fn main() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            "info,hyper_util=off,hyper=off,reqwest=off,h2=off,rustls=off"
                .parse()
                .unwrap()
        })
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let args: Vec<String> = env::args().collect();

    // Check for command
    let command = args.get(1).map(|s| s.as_str());

    match command {
        Some("check-db") => {
            let db_url = args
                .iter()
                .position(|s| s == "--db-url")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str())
                .unwrap_or("postgresql://postgres:postgres@localhost:5432/ploy");

            if let Err(e) = check_database(db_url).await {
                eprintln!("Database check failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("collect-quotes") => {
            let db_url = args
                .iter()
                .position(|s| s == "--db-url")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str())
                .unwrap_or("postgresql://postgres:postgres@localhost:5432/ploy");

            let symbols_str = args
                .iter()
                .position(|s| s == "--symbols")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str())
                .unwrap_or("BTCUSDT,ETHUSDT,SOLUSDT");

            let timeframe = args
                .iter()
                .position(|s| s == "--timeframe")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str())
                .unwrap_or("5m");

            let symbols: Vec<String> = symbols_str
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();

            let pool = match PgPoolOptions::new()
                .max_connections(5)
                .connect(db_url)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Failed to connect to database: {e}");
                    std::process::exit(1);
                }
            };

            let config = CollectorConfig {
                symbols,
                timeframe: timeframe.to_string(),
                refresh_interval_secs: 300, // 5 minutes
            };

            let collector = QuoteCollector::new(config, pool);

            if let Err(e) = collector.run().await {
                eprintln!("Quote collector failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("run") | None => {
            // Continue with normal strategy execution
        }
        Some("--help") | Some("-h") => {
            print_usage();
            return;
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            print_usage();
            std::process::exit(1);
        }
    }

    let mut config_path: Option<String> = None;
    let mut force_dry_run = false;
    let mut i = if command == Some("run") { 2 } else { 1 };
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--dry-run" => force_dry_run = true,
            "--foreground" => {} // compat, always foreground
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("Error: --config is required");
        print_usage();
        std::process::exit(1);
    });

    let config = match FullConfig::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config {config_path}: {e}");
            std::process::exit(1);
        }
    };

    let mut runtime_config = config.runtime_config();
    if force_dry_run {
        runtime_config.mode = RuntimeMode::DryRun;
    }

    info!(
        mode = ?runtime_config.mode,
        config = %config_path,
        symbols = ?config.strategy.symbols,
        "ploy-runner starting",
    );

    // Prepare symbols for feeds.
    // Historical/replay modes keep uppercase symbols; live RTDS feeds use lowercase.
    let symbols = prepare_feed_symbols(runtime_config.mode, &config.strategy.symbols);

    // Build strategy
    let strategy = DirectionalStrategy::new(config.strategy.clone());

    // Build feed and executor based on mode
    let (result, snapshot) = match runtime_config.mode {
        RuntimeMode::Backtest => {
            // Load historical data from database
            let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgresql://postgres:postgres@localhost:5432/ploy".to_string()
            });

            let pool = PgPoolOptions::new()
                .max_connections(5)
                .connect(&db_url)
                .await
                .expect("Failed to connect to database");

            // Get time range from config, or use default
            let (from, to) = config.backtest_time_range().unwrap_or_else(|| {
                // Default: April 1st 2026 (today's collected data with full Polymarket quotes)
                let from = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc);
                let to = chrono::DateTime::parse_from_rfc3339("2026-04-01T13:30:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc);
                (from, to)
            });

            info!(
                from = %from,
                to = %to,
                symbols = ?symbols,
                "Loading historical data from database",
            );

            let backtest_options = HistoricalLoadOptions {
                include_reference_prices: config.backtest_data.include_reference_prices,
                reference_symbols: config
                    .backtest_data
                    .reference_symbols(&config.reference_data),
                include_sports_state: config.backtest_data.include_sports_state,
            };

            let updates =
                load_from_database_with_options(&pool, &symbols, from, to, &backtest_options)
                    .await
                    .expect("Failed to load historical data");

            info!(updates = updates.len(), "Historical data loaded");

            let feed = HistoricalFeed::new(updates);
            let sim_config = config.sim_executor_config();
            let executor = SimulatedExecutor::new(sim_config);
            let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
            let mut runtime =
                StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config.clone());
            let result = runtime.run().await;
            let snapshot = runtime.trading().snapshot(&BTreeMap::new());
            (result, snapshot)
        }
        RuntimeMode::Replay => {
            let replay_path = config.replay_market_updates_path().unwrap_or_else(|| {
                eprintln!(
                    "Replay mode requires [runtime].replay_market_updates_from in the config"
                );
                std::process::exit(1);
            });

            info!(
                path = %replay_path.display(),
                "Loading recorded market-update log for replay",
            );

            let feed = RecordedFeed::from_path(replay_path).unwrap_or_else(|error| {
                eprintln!(
                    "Failed to load replay market updates from {}: {error}",
                    replay_path.display()
                );
                std::process::exit(1);
            });
            let sim_config = config.sim_executor_config();
            let executor = SimulatedExecutor::new(sim_config);
            let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
            let mut runtime =
                StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config.clone());
            let result = runtime.run().await;
            let snapshot = runtime.trading().snapshot(&BTreeMap::new());
            (result, snapshot)
        }
        RuntimeMode::Live | RuntimeMode::DryRun => {
            // Use live feeds for dry-run and live modes.
            // Connect to DB if DATABASE_URL is set so that discovered markets and
            // quotes are persisted for future backtest replay.
            let db_pool: Option<sqlx::PgPool> = match env::var("DATABASE_URL") {
                Ok(url) => match PgPoolOptions::new().max_connections(5).connect(&url).await {
                    Ok(p) => {
                        info!("DB connected — market metadata and quotes will be persisted");
                        Some(p)
                    }
                    Err(e) => {
                        warn!(error = %e, "DB connection failed; running without persistence");
                        None
                    }
                },
                Err(_) => {
                    info!("DATABASE_URL not set — running without DB persistence");
                    None
                }
            };

            let (tx, rx) = broadcast::channel(8192);
            let tx = Arc::new(tx);

            let reference_prices = new_reference_price_registry();

            // Spawn feed producers
            // 1. Spot feed — Binance via RTDS WebSocket (primary)
            let spot_handle = spawn_spot_feed(
                tx.clone(),
                reference_prices.clone(),
                symbols.clone(),
                db_pool.clone(),
            );

            // 1b. DB spot fallback — polls binance_price_ticks every 5s.
            //     Ensures strategy has spot prices even when RTDS is unavailable.
            let _db_spot_handle = if let Some(ref db) = db_pool {
                Some(spawn_db_spot_feed(tx.clone(), symbols.clone(), db.clone()))
            } else {
                None
            };

            // 2. Chainlink feed — canonical price source for S0 at eventStartTime
            let chainlink_handle = spawn_chainlink_feed(
                tx.clone(),
                reference_prices.clone(),
                symbols.clone(),
                db_pool.clone(),
            );

            // 3. Pyth feed — additive reference-data plane for non-crypto markets.
            let pyth_handle = spawn_pyth_reference_feed(
                tx.clone(),
                reference_prices.clone(),
                config.reference_data.pyth_symbols.clone(),
                db_pool.clone(),
            );

            // 4. Market scanner — discovers events and injects EventDiscovered/EventExpired
            //    Also spawns quote feeds dynamically as new tokens are discovered.
            let scanner_handle = spawn_market_scanner(
                tx.clone(),
                reference_prices.clone(),
                symbols.clone(),
                db_pool.clone(),
            );

            let sports_handle = if config.reference_data.capture_sports_state {
                Some(spawn_sports_feed(tx.clone(), db_pool.clone()))
            } else {
                None
            };

            let feed: Box<dyn Feed> = if let Some(record_path) = config.record_market_updates_path()
            {
                Box::new(
                    RecordingFeed::new(LiveFeed::new(rx), record_path).unwrap_or_else(|error| {
                        eprintln!(
                            "Failed to open market-update log {}: {error}",
                            record_path.display()
                        );
                        std::process::exit(1);
                    }),
                )
            } else {
                Box::new(LiveFeed::new(rx))
            };
            let recorder = build_signal_recorder(db_pool.clone(), runtime_config.mode);

            let result = if runtime_config.mode == RuntimeMode::Live {
                let executor = build_live_executor();
                let mut runtime = StrategyRuntime::new(
                    strategy,
                    feed,
                    executor,
                    recorder,
                    runtime_config.clone(),
                );
                let result = runtime.run().await;
                let snapshot = runtime.trading().snapshot(&BTreeMap::new());
                (result, snapshot)
            } else {
                let sim_config = config.sim_executor_config();
                let executor = SimulatedExecutor::new(sim_config);
                let mut runtime = StrategyRuntime::new(
                    strategy,
                    feed,
                    executor,
                    recorder,
                    runtime_config.clone(),
                );
                let result = runtime.run().await;
                let snapshot = runtime.trading().snapshot(&BTreeMap::new());
                (result, snapshot)
            };

            // Clean up feed tasks
            spot_handle.abort();
            chainlink_handle.abort();
            pyth_handle.abort();
            scanner_handle.abort();
            if let Some(handle) = sports_handle {
                handle.abort();
            }

            result
        }
    };

    info!(
        updates = result.updates_processed,
        intents = result.intents_submitted,
        fills = result.fills_recorded,
        net_pnl = %result.pnl.net_pnl(),
        elapsed = format!("{:.1}s", result.elapsed_secs),
        "ploy-runner finished",
    );

    if matches!(
        runtime_config.mode,
        RuntimeMode::Backtest | RuntimeMode::Replay
    ) {
        let cashflow = snapshot.fill_cashflow_summary();
        let roi_on_deployed_capital = cashflow
            .roi_on_deployed_capital()
            .map(|roi| format!("{}%", (roi * Decimal::from(100)).round_dp(2)))
            .unwrap_or_else(|| "n/a".to_string());

        info!(
            buy_shares = %cashflow.buy_shares,
            sell_shares = %cashflow.sell_shares,
            deployed_capital = %cashflow.deployed_capital(),
            gross_sell_proceeds = %cashflow.gross_sell_proceeds,
            fees = %cashflow.total_fees,
            roi_on_deployed_capital = %roi_on_deployed_capital,
            "Replay/backtest cashflow summary",
        );
    }
}

fn build_signal_recorder(db_pool: Option<sqlx::PgPool>, mode: RuntimeMode) -> Box<dyn Recorder> {
    let Some(pool) = db_pool else {
        info!("Signal recorder disabled — DATABASE_URL unavailable");
        return Box::new(NullRecorder);
    };

    let mode_label = match mode {
        RuntimeMode::Backtest => "backtest",
        RuntimeMode::Replay => "replay",
        RuntimeMode::DryRun => "dry_run",
        RuntimeMode::Live => "live",
    }
    .to_string();

    let flush_fn = Box::new(
        move |batch: Vec<SignalRecord>| -> std::pin::Pin<
            Box<dyn std::future::Future<Output = ()> + Send>,
        > {
            let pool = pool.clone();
            let mode_label = mode_label.clone();
            Box::pin(async move {
                persist_signal_batch(&pool, &mode_label, batch).await;
            })
        },
    );

    Box::new(BufferedRecorder::new(25, Some(flush_fn)))
}

async fn persist_signal_batch(pool: &sqlx::PgPool, mode_label: &str, batch: Vec<SignalRecord>) {
    for signal in batch {
        let confidence = Decimal::from_f64(signal.p_hat);
        let edge = Decimal::from_f64(signal.edge);
        let context = json!({
            "runtime_mode": mode_label,
            "event_id": signal.event_id,
            "intent_id": signal.intent_id,
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO signal_history (
                recorded_at,
                intent_id,
                agent_id,
                strategy_id,
                domain,
                signal_type,
                token_id,
                symbol,
                side,
                confidence,
                market_price,
                edge,
                context
            )
            VALUES (
                $1,
                NULL,
                'ploy-runner',
                $2,
                'polymarket',
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10
            )
            "#,
        )
        .bind(signal.ts)
        .bind(signal.strategy)
        .bind(signal.decision)
        .bind(signal.token_id)
        .bind(signal.symbol)
        .bind(signal.direction)
        .bind(confidence)
        .bind(signal.entry_price)
        .bind(edge)
        .bind(context)
        .execute(pool)
        .await
        {
            warn!(error = %error, "Failed to persist signal record");
        }
    }
}

/// Build a CallbackExecutor that routes orders through PolymarketExecutionGateway.
fn build_live_executor() -> CallbackExecutor {
    use ploy_connectivity::{ExecutionRequest, LiveExecutionGateway, PolymarketExecutionGateway};

    let gateway = Arc::new(PolymarketExecutionGateway::from_env());

    CallbackExecutor::new(Box::new(move |intent: TradingIntent| {
        let gw = gateway.clone();
        Box::pin(async move {
            let request = ExecutionRequest {
                order_id: intent.intent_id.clone(),
                token_id: intent.token_id.clone(),
                side: intent.side,
                quantity: intent.quantity,
                limit_price: intent.limit_price,
            };

            match tokio::task::spawn_blocking(move || gw.submit(&request)).await {
                Ok(Ok(outcome)) => {
                    use ploy_connectivity::ExecutionOutcome;
                    match outcome {
                        ExecutionOutcome::Acknowledged { venue_order_id } => {
                            info!(venue_order_id = %venue_order_id, "Order acknowledged");
                            ExecutionReport {
                                order_id: venue_order_id,
                                fill: None, // fills come from reconciliation
                                rejected: false,
                                rejection_reason: None,
                                slippage: None,
                                market_impact: None,
                            }
                        }
                        ExecutionOutcome::Rejected { reason } => {
                            error!(reason = %reason, "Order rejected by venue");
                            ExecutionReport {
                                order_id: String::new(),
                                fill: None,
                                rejected: true,
                                rejection_reason: Some(reason),
                                slippage: None,
                                market_impact: None,
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!(error = %e, "Execution gateway error");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: true,
                        rejection_reason: Some(e.to_string()),
                        slippage: None,
                        market_impact: None,
                    }
                }
                Err(e) => {
                    error!(error = %e, "Spawn blocking failed");
                    ExecutionReport {
                        order_id: String::new(),
                        fill: None,
                        rejected: true,
                        rejection_reason: Some(format!("internal: {e}")),
                        slippage: None,
                        market_impact: None,
                    }
                }
            }
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::prepare_feed_symbols;
    use ploy_strategy_bundles::RuntimeMode;

    #[test]
    fn keeps_strategy_symbols_canonical_for_live_feeds() {
        let symbols = vec!["BTCUSDT".to_string(), "ethusdt".to_string()];

        let prepared = prepare_feed_symbols(RuntimeMode::DryRun, &symbols);

        assert_eq!(prepared, vec!["BTCUSDT".to_string(), "ethusdt".to_string()]);
    }
}
