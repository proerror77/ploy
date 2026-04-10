mod collector;
mod discovery;
mod feeds;
mod reference_prices;
mod scanner;
mod sports_feed;

use async_trait::async_trait;
use ploy_claimer::ensure_account_claimer_daemon;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::{
    BayesianDirectionalStrategy, CallbackExecutor, DirectionalStrategy, ExecutionReport, Feed,
    FullConfig, HistoricalFeed, LiveFeed, NullRecorder, RecordedFeed, Recorder, RecordingFeed,
    RuntimeMode, SignalRecord, SimulatedExecutor, StrategyLogic, StrategyRuntime,
};
use ploy_trading::{FillRecord, TradeSide, TradingIntent};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
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

fn database_unavailable_is_fatal(mode: RuntimeMode, database_url_present: bool) -> bool {
    database_url_present && matches!(mode, RuntimeMode::Live | RuntimeMode::DryRun)
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

    // Build strategy (variant selected via [runtime].strategy_variant)
    let strategy: Box<dyn StrategyLogic> = match config.runtime.strategy_variant.as_str() {
        "directional_bayes" => {
            info!("Using Bayesian directional strategy variant");
            // Both config structs have identical fields; round-trip through JSON.
            let json = serde_json::to_value(&config.strategy).expect("serialize DirectionalConfig");
            let bayes_config: ploy_strategy_bundles::strategies::directional_bayes::BayesianDirectionalConfig =
                serde_json::from_value(json).expect("deserialize BayesianDirectionalConfig");
            Box::new(BayesianDirectionalStrategy::new(bayes_config))
        }
        _ => Box::new(DirectionalStrategy::new(config.strategy.clone())),
    };

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
            let db_url = env::var("DATABASE_URL").ok();
            let db_pool: Option<sqlx::PgPool> = match db_url.as_deref() {
                Some(url) => match PgPoolOptions::new().max_connections(5).connect(url).await {
                    Ok(p) => {
                        info!("DB connected — market metadata and quotes will be persisted");
                        Some(p)
                    }
                    Err(e) => {
                        if database_unavailable_is_fatal(runtime_config.mode, true) {
                            error!(
                                error = %e,
                                "DB connection failed for configured runtime; refusing to start without persistence"
                            );
                            std::process::exit(1);
                        }

                        warn!(error = %e, "DB connection failed; running without persistence");
                        None
                    }
                },
                None => {
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
                // Start auto-claimer daemon for live mode (singleton, safe to call multiple times)
                if let Err(e) = ensure_account_claimer_daemon().await {
                    warn!("Auto-claimer daemon failed to start: {e}");
                }
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

#[derive(Clone, Default)]
struct TokenExecutionContext {
    event_id: Option<String>,
    symbol: Option<String>,
    market_side: Option<String>,
}

struct RuntimeDbRecorder {
    pool: sqlx::PgPool,
    mode_label: String,
    token_context: HashMap<String, TokenExecutionContext>,
}

impl RuntimeDbRecorder {
    fn new(pool: sqlx::PgPool, mode_label: String) -> Self {
        Self {
            pool,
            mode_label,
            token_context: HashMap::new(),
        }
    }

    fn merge_context(
        &self,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
    ) -> TokenExecutionContext {
        let mut context = self
            .token_context
            .get(&intent.token_id)
            .cloned()
            .unwrap_or_default();

        if context.event_id.is_none() && !intent.market_id.is_empty() {
            context.event_id = Some(intent.market_id.clone());
        }

        if let Some(signal) = signal {
            if context.event_id.is_none() {
                context.event_id = signal.event_id.clone();
            }
            if context.symbol.is_none() {
                context.symbol = Some(signal.symbol.clone());
            }
            if context.market_side.is_none() {
                context.market_side = Some(signal.direction.clone());
            }
        }

        context
    }

    fn remember_context(&mut self, token_id: &str, context: &TokenExecutionContext) {
        if context.event_id.is_none() && context.symbol.is_none() && context.market_side.is_none() {
            return;
        }
        self.token_context
            .insert(token_id.to_string(), context.clone());
    }

    fn remember_signal_context(&mut self, signal: &SignalRecord) {
        let Some(token_id) = signal.token_id.as_deref() else {
            return;
        };

        self.token_context.insert(
            token_id.to_string(),
            TokenExecutionContext {
                event_id: signal.event_id.clone(),
                symbol: Some(signal.symbol.clone()),
                market_side: Some(signal.direction.clone()),
            },
        );
    }

    async fn persist_signal(&self, signal: &SignalRecord) {
        let confidence = Decimal::from_f64(signal.p_hat);
        let edge = Decimal::from_f64(signal.edge);
        let context = json!({
            "runtime_mode": self.mode_label,
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
        .bind(signal.strategy.as_str())
        .bind(signal.decision.as_str())
        .bind(signal.token_id.as_deref())
        .bind(signal.symbol.as_str())
        .bind(signal.direction.as_str())
        .bind(confidence)
        .bind(signal.entry_price)
        .bind(edge)
        .bind(context)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, "Failed to persist signal record");
        }
    }

    async fn persist_order(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) {
        let fill = report.fill.as_ref();
        let status = if report.rejected {
            "REJECTED"
        } else if let Some(fill) = fill {
            if fill.quantity >= intent.quantity {
                "FILLED"
            } else {
                "PARTIALLY_FILLED"
            }
        } else {
            "ACKNOWLEDGED"
        };
        let exchange_order_id = if report.order_id.is_empty() || report.order_id == order_id {
            None
        } else {
            Some(report.order_id.as_str())
        };
        let filled_quantity = fill.map(|record| record.quantity).unwrap_or(Decimal::ZERO);
        let avg_fill_price = fill.map(|record| record.price);
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "signal_decision": signal.map(|record| record.decision.as_str()),
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO strategy_runtime_orders (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                venue_order_id,
                event_id,
                symbol,
                token_id,
                market_side,
                order_side,
                quantity,
                limit_price,
                filled_quantity,
                avg_fill_price,
                status,
                rejection_reason,
                slippage,
                market_impact,
                context
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
            )
            ON CONFLICT (order_id) DO UPDATE
            SET venue_order_id = COALESCE(EXCLUDED.venue_order_id, strategy_runtime_orders.venue_order_id),
                event_id = COALESCE(EXCLUDED.event_id, strategy_runtime_orders.event_id),
                symbol = COALESCE(EXCLUDED.symbol, strategy_runtime_orders.symbol),
                market_side = COALESCE(EXCLUDED.market_side, strategy_runtime_orders.market_side),
                filled_quantity = EXCLUDED.filled_quantity,
                avg_fill_price = COALESCE(EXCLUDED.avg_fill_price, strategy_runtime_orders.avg_fill_price),
                status = EXCLUDED.status,
                rejection_reason = COALESCE(EXCLUDED.rejection_reason, strategy_runtime_orders.rejection_reason),
                slippage = COALESCE(EXCLUDED.slippage, strategy_runtime_orders.slippage),
                market_impact = COALESCE(EXCLUDED.market_impact, strategy_runtime_orders.market_impact),
                context = EXCLUDED.context
            "#,
        )
        .bind(intent.created_at)
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(order_id)
        .bind(exchange_order_id)
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(intent.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match intent.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(intent.quantity)
        .bind(intent.limit_price)
        .bind(filled_quantity)
        .bind(avg_fill_price)
        .bind(status)
        .bind(report.rejection_reason.as_deref())
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(context_json)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, order_id, "Failed to persist execution order");
        }
    }

    async fn persist_fill(
        &self,
        strategy: &str,
        intent: &TradingIntent,
        context: &TokenExecutionContext,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) {
        let context_json = json!({
            "runtime_mode": self.mode_label,
            "slippage": report.slippage.map(|value| value.to_string()),
            "market_impact": report.market_impact.map(|value| value.to_string()),
        });

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO strategy_runtime_fills (
                recorded_at,
                runtime_mode,
                strategy_id,
                deployment_id,
                intent_id,
                order_id,
                fill_id,
                event_id,
                symbol,
                token_id,
                market_side,
                fill_side,
                quantity,
                price,
                fee,
                slippage,
                market_impact,
                fill_timestamp,
                context
            )
            VALUES (
                NOW(), $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (fill_id) DO NOTHING
            "#,
        )
        .bind(self.mode_label.as_str())
        .bind(strategy)
        .bind(intent.deployment_id.as_str())
        .bind(intent.intent_id.as_str())
        .bind(fill.order_id.as_str())
        .bind(fill.fill_id.as_str())
        .bind(context.event_id.as_deref())
        .bind(context.symbol.as_deref())
        .bind(fill.token_id.as_str())
        .bind(context.market_side.as_deref())
        .bind(match fill.side {
            TradeSide::Buy => "BUY",
            TradeSide::Sell => "SELL",
        })
        .bind(fill.quantity)
        .bind(fill.price)
        .bind(fill.fee)
        .bind(report.slippage)
        .bind(report.market_impact)
        .bind(fill.timestamp)
        .bind(context_json)
        .execute(&self.pool)
        .await
        {
            warn!(error = %error, fill_id = %fill.fill_id, "Failed to persist execution fill");
        }
    }
}

#[async_trait]
impl Recorder for RuntimeDbRecorder {
    async fn record_signal(&mut self, signal: &SignalRecord) {
        self.remember_signal_context(signal);
        self.persist_signal(signal).await;
    }

    async fn record_order(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        order_id: &str,
    ) {
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_order(strategy, intent, &context, signal, report, order_id)
            .await;
    }

    async fn record_fill(
        &mut self,
        strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        fill: &FillRecord,
        report: &ExecutionReport,
    ) {
        let context = self.merge_context(intent, signal);
        self.remember_context(&intent.token_id, &context);
        self.persist_fill(strategy, intent, &context, fill, report)
            .await;
    }

    async fn flush(&mut self) {}
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

    Box::new(RuntimeDbRecorder::new(pool, mode_label))
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
                order_type: ploy_connectivity::OrderExecutionType::FAK,
                aggressive_ticks: 2,
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
    use super::{database_unavailable_is_fatal, prepare_feed_symbols};
    use ploy_strategy_bundles::RuntimeMode;

    #[test]
    fn keeps_strategy_symbols_canonical_for_live_feeds() {
        let symbols = vec!["BTCUSDT".to_string(), "ethusdt".to_string()];

        let prepared = prepare_feed_symbols(RuntimeMode::DryRun, &symbols);

        assert_eq!(prepared, vec!["BTCUSDT".to_string(), "ethusdt".to_string()]);
    }

    #[test]
    fn treats_live_and_dry_run_db_connection_failures_as_fatal_when_configured() {
        assert!(database_unavailable_is_fatal(RuntimeMode::Live, true));
        assert!(database_unavailable_is_fatal(RuntimeMode::DryRun, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Backtest, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::Replay, true));
        assert!(!database_unavailable_is_fatal(RuntimeMode::DryRun, false));
    }
}
