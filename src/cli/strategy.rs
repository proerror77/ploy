//! Strategy management commands
//!
//! ploy strategy list              - List all strategies and their status
//! ploy strategy start <name>      - Start a strategy
//! ploy strategy stop <name>       - Stop a strategy
//! ploy strategy status [name]     - Show strategy status
//! ploy strategy logs <name>       - View strategy logs
//! ploy strategy reload <name>     - Reload strategy config

use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::{PolymarketClient, PostgresStore};
use crate::config::ExecutionConfig;
use crate::signing::Wallet;
use crate::strategy::execution::executor::OrderExecutor;
use crate::strategy::{StrategyFactory, StrategyManager};
use analysis_commands::{
    AccuracyArgs, BacktestArgs, BacktestDiffArgs, BacktestListArgs, DirectionalSignalBacktestArgs,
    ExportCryptoLobDatasetArgs, LiveBacktestCompareArgs,
};
use backtest_ops::{run_backtest, run_backtest_diff, run_backtest_list, run_live_backtest_compare};
use maintenance_ops::{
    backfill_klines, backfill_pm_replay_tables, backfill_pm_token_settlements, run_integrity_check,
    run_nba_comeback, seed_nba_stats,
};
use runtime_ops::{
    list_strategies, reload_strategy, show_logs, show_status, start_strategy, stop_strategy,
};
use settlement_ops::{export_crypto_lob_dataset, report_accuracy_pm_settlement};

mod analysis_commands;
mod backtest_ops;
mod maintenance_ops;
mod runtime_ops;
mod settlement_ops;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoLobDatasetFormat {
    Csv,
    Parquet,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyBacktestMode {
    Replay,
    Settlement,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityVacuumProfile {
    Prod,
    Research,
    ResearchV2,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmReplayQuality {
    Strict,
    Research,
}

impl Default for CryptoLobDatasetFormat {
    fn default() -> Self {
        #[cfg(feature = "analysis")]
        {
            Self::Parquet
        }
        #[cfg(not(feature = "analysis"))]
        {
            Self::Csv
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum StrategyCommands {
    /// List all available strategies
    List,

    /// Start a strategy
    Start {
        /// Strategy name (momentum, split_arb, sports)
        name: String,

        /// Config file path (optional, uses default if not specified)
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Run in dry-run mode (no real orders)
        #[arg(long)]
        dry_run: bool,

        /// Run in foreground (don't daemonize)
        #[arg(long)]
        foreground: bool,
    },

    /// Stop a running strategy
    Stop {
        /// Strategy name
        name: String,

        /// Force stop (SIGKILL instead of SIGTERM)
        #[arg(long)]
        force: bool,
    },

    /// Show status of strategies
    Status {
        /// Specific strategy name (optional, shows all if not specified)
        name: Option<String>,
    },

    /// View strategy logs
    Logs {
        /// Strategy name
        name: String,

        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        tail: usize,

        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },

    /// Reload strategy configuration
    Reload {
        /// Strategy name
        name: String,
    },

    /// Seed NBA team comeback stats into the database
    NbaSeedStats {
        /// Season string (e.g. "2025-26")
        #[arg(long, default_value = "2025-26")]
        season: String,

        /// Database URL (uses config default if not specified)
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Deprecated: standalone NBA comeback runtime (use managed deployments)
    NbaComeback {
        /// Config file path
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Run in dry-run mode
        #[arg(long)]
        dry_run: bool,
    },

    /// Report prediction accuracy using Polymarket official settlement (token pays 1/0)
    Accuracy(AccuracyArgs),

    /// Backtest directional signals (signal_history) using Polymarket official settlement (token pays 1/0)
    ///
    /// Legacy alias for:
    /// `ploy strategy backtest directional --mode settlement ...`
    DirectionalSignalBacktest(DirectionalSignalBacktestArgs),

    /// Export a labeled dataset for crypto LOB model training (uses Polymarket settlement y_up).
    ExportCryptoLobDataset(ExportCryptoLobDatasetArgs),

    /// Run data integrity checks on the database
    IntegrityCheck {
        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Run a strategy backtest against the integrated DB pipeline
    Backtest {
        /// Strategy name (momentum, directional, prob-garch/prob_garch, liquidity-vacuum/liquidity_vacuum, staggered-arb/staggered_arb/gamma_scalping)
        name: String,

        /// Backtest mode: replay historical feed, or score directional signals by settlement
        #[arg(long, value_enum, default_value_t = StrategyBacktestMode::Replay)]
        mode: StrategyBacktestMode,

        /// Start date (ISO 8601)
        #[arg(long)]
        from: Option<String>,

        /// End date (ISO 8601)
        #[arg(long)]
        to: Option<String>,

        /// Symbols (comma-separated)
        #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT")]
        symbols: String,

        /// Initial capital (USD)
        #[arg(long, default_value = "10000")]
        capital: f64,

        /// Save results to DB
        #[arg(long)]
        save: bool,

        /// Output JSON
        #[arg(long)]
        json: bool,

        /// Settlement-mode lookback window in hours
        #[arg(long, default_value = "168")]
        lookback_hours: u64,

        /// Settlement-mode filter by account_id (defaults to all)
        #[arg(long)]
        account_id: Option<String>,

        /// Settlement-mode filter by agent_id (defaults to all)
        #[arg(long)]
        agent_id: Option<String>,

        /// Settlement-mode only include live signals (exclude dry-run)
        #[arg(long)]
        live_only: bool,

        /// Settlement-mode max number of signals to score (latest first)
        #[arg(long, default_value = "50000")]
        limit: usize,

        /// Settlement-mode skip Gamma refresh and use cached settlement rows only
        #[arg(long)]
        no_refresh: bool,

        /// Skip Gamma verification phase after replay backtest
        #[arg(long)]
        skip_gamma: bool,

        /// Verify an existing backtest run by UUID
        #[arg(long)]
        verify_run: Option<String>,

        /// Print a database data-availability summary for this backtest and exit
        #[arg(long)]
        diagnose_db: bool,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,

        /// Liquidity-vacuum profile preset.
        ///
        /// `prod` keeps strict thresholds; `research` is exploratory;
        /// `research-v2` is a higher-quality research baseline.
        #[arg(long, value_enum, default_value_t = LiquidityVacuumProfile::Prod)]
        lv_profile: LiquidityVacuumProfile,

        /// Override liquidity-vacuum price move threshold (fraction, e.g. 0.02 = 2%)
        #[arg(long)]
        lv_price_move_threshold: Option<f64>,

        /// Override liquidity-vacuum volume multiplier threshold (e.g. 3.0)
        #[arg(long)]
        lv_volume_multiplier_threshold: Option<f64>,

        /// Override liquidity-vacuum order concentration threshold (e.g. 0.7)
        #[arg(long)]
        lv_order_concentration_threshold: Option<f64>,

        /// Override liquidity-vacuum entry deviation threshold (fraction)
        #[arg(long)]
        lv_entry_deviation_threshold: Option<f64>,

        /// Override liquidity-vacuum entry z-score threshold (0 disables z-score entry gate)
        #[arg(long)]
        lv_entry_zscore_threshold: Option<f64>,

        /// Override liquidity-vacuum take-profit z-score threshold (0 disables z-score TP)
        #[arg(long)]
        lv_take_profit_zscore_threshold: Option<f64>,

        /// Override liquidity-vacuum stop-loss z-score threshold (0 disables z-score SL)
        #[arg(long)]
        lv_stop_loss_zscore_threshold: Option<f64>,

        /// Override liquidity-vacuum EMA-band take-profit threshold (fraction, e.g. 0.03)
        #[arg(long)]
        lv_take_profit_ema_band_pct: Option<f64>,

        /// Override liquidity-vacuum stop-loss threshold (fraction, e.g. 0.25)
        #[arg(long)]
        lv_stop_loss_pct: Option<f64>,

        /// Override liquidity-vacuum minimum edge buffer above fees (fraction)
        #[arg(long)]
        lv_min_edge_buffer: Option<f64>,

        /// Override liquidity-vacuum z-score lookback sample size
        #[arg(long)]
        lv_zscore_lookback_samples: Option<usize>,

        /// Override liquidity-vacuum max holding seconds (0 disables max-hold exit)
        #[arg(long)]
        lv_max_holding_secs: Option<u64>,

        /// Override staggered-arb entry window (seconds after event start; 0 disables)
        #[arg(long)]
        sa_entry_after_start_max_secs: Option<u64>,
    },

    /// List historical backtest runs
    BacktestList(BacktestListArgs),

    /// Compare two backtest runs side by side
    BacktestDiff(BacktestDiffArgs),

    /// Compare one backtest run against recent live order outcomes
    LiveBacktestCompare(LiveBacktestCompareArgs),

    /// Backfill Binance klines into the database for historical backtesting
    BackfillKlines {
        /// Symbols (comma-separated, e.g. BTCUSDT,ETHUSDT,SOLUSDT)
        #[arg(long)]
        symbols: String,

        /// Start date (ISO 8601, e.g. 2026-02-20T00:00:00Z)
        #[arg(long)]
        from: String,

        /// End date (ISO 8601, e.g. 2026-02-28T00:00:00Z)
        #[arg(long)]
        to: String,

        /// Kline interval (default: 1m)
        #[arg(long, default_value = "1m")]
        interval: String,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Backfill PM replay tables from sync_records for backtesting
    BackfillPmReplayTables {
        /// Start date (ISO 8601)
        #[arg(long)]
        from: Option<String>,

        /// End date (ISO 8601)
        #[arg(long)]
        to: Option<String>,

        /// Symbols filter (comma-separated)
        #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
        symbols: String,

        /// Synthetic orderbook depth per snapshot side (shares)
        #[arg(long, default_value = "1000")]
        synthetic_depth: u64,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Backfill official Polymarket token settlements into pm_token_settlements
    BackfillPmTokenSettlements {
        /// Start date (ISO 8601)
        #[arg(long)]
        from: Option<String>,

        /// End date (ISO 8601)
        #[arg(long)]
        to: Option<String>,

        /// Symbols filter (comma-separated)
        #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
        symbols: String,

        /// Max distinct token_ids to refresh from sync_records
        #[arg(long, default_value = "5000")]
        limit: usize,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },
}

impl StrategyCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List => list_strategies().await,
            Self::Start {
                name,
                config,
                dry_run,
                foreground,
            } => start_strategy(&name, config, dry_run, foreground).await,
            Self::Stop { name, force } => stop_strategy(&name, force).await,
            Self::Status { name } => show_status(name.as_deref()).await,
            Self::Logs { name, tail, follow } => show_logs(&name, tail, follow).await,
            Self::Reload { name } => reload_strategy(&name).await,
            Self::NbaSeedStats {
                season,
                database_url,
            } => seed_nba_stats(&season, database_url).await,
            Self::NbaComeback { config, dry_run } => run_nba_comeback(config, dry_run).await,
            Self::Accuracy(args) => args.run().await,
            Self::DirectionalSignalBacktest(args) => args.run().await,
            Self::ExportCryptoLobDataset(args) => args.run().await,
            Self::IntegrityCheck { json, database_url } => {
                run_integrity_check(json, database_url).await
            }
            Self::Backtest(args) => args.run().await,
            Self::BacktestList(args) => args.run().await,
            Self::BacktestDiff(args) => args.run().await,
            Self::LiveBacktestCompare(args) => args.run().await,
            Self::BackfillKlines {
                symbols,
                from,
                to,
                interval,
                database_url,
            } => backfill_klines(&symbols, &from, &to, &interval, database_url).await,
            Self::BackfillPmReplayTables {
                from,
                to,
                symbols,
                synthetic_depth,
                database_url,
            } => backfill_pm_replay_tables(from, to, &symbols, synthetic_depth, database_url).await,
            Self::BackfillPmTokenSettlements {
                from,
                to,
                symbols,
                limit,
                database_url,
            } => backfill_pm_token_settlements(from, to, &symbols, limit, database_url).await,
        }
    }
}

/// Get the config directory path
fn config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

/// Get the run directory for PID files
fn run_dir() -> PathBuf {
    std::env::var("PLOY_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/run"))
}

/// Get the log directory
fn log_dir() -> PathBuf {
    std::env::var("PLOY_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/logs"))
}

/// Run strategy in foreground using StrategyManager
async fn run_strategy_foreground(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    use crate::adapters::PolymarketWebSocket;
    use crate::strategy::DataFeedManager;

    // Load config
    let config_content = fs::read_to_string(config_path)
        .context(format!("Failed to read config: {}", config_path.display()))?;

    println!(
        "\x1b[32m▶ Running {} in foreground (Ctrl+C to stop)\x1b[0m\n",
        name
    );

    // Create strategy via factory
    let strategy = StrategyFactory::from_toml(&config_content, dry_run)
        .context("Failed to create strategy from config")?;

    let strategy_id = strategy.id().to_string();
    let required_feeds = strategy.required_feeds();

    println!("  Strategy ID: {}", strategy_id);
    println!("  Strategy: {}", strategy.name());
    println!("  Description: {}", strategy.description());
    println!("  Dry Run: {}", dry_run);
    println!("  Required Feeds: {:?}", required_feeds);
    println!();

    // Create order executor (authenticated client for live trading)
    let executor = if dry_run {
        println!("  \x1b[33m⚠ DRY RUN MODE - Orders will be simulated\x1b[0m");
        let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
        Some(Arc::new(OrderExecutor::new(
            client,
            ExecutionConfig::default(),
        )))
    } else {
        // For live trading, need authenticated client
        match Wallet::from_env(POLYGON_CHAIN_ID) {
            Ok(wallet) => {
                println!("  \x1b[32m✓ Wallet loaded: {:?}\x1b[0m", wallet.address());
                let funder = std::env::var("POLYMARKET_FUNDER").ok();
                let auth_result = if let Some(ref funder_addr) = funder {
                    println!("  Using proxy wallet, funder: {}", funder_addr);
                    PolymarketClient::new_authenticated_proxy(
                        "https://clob.polymarket.com",
                        wallet,
                        funder_addr,
                        true, // neg_risk: crypto binary options use NegRisk exchange
                    )
                    .await
                } else {
                    PolymarketClient::new_authenticated(
                        "https://clob.polymarket.com",
                        wallet,
                        true, // neg_risk: crypto binary options use NegRisk exchange
                    )
                    .await
                };
                match auth_result {
                    Ok(client) => {
                        println!("  \x1b[32m✓ Authenticated with Polymarket CLOB\x1b[0m");
                        Some(Arc::new(OrderExecutor::new(
                            client,
                            ExecutionConfig::default(),
                        )))
                    }
                    Err(e) => {
                        error!("Failed to authenticate: {}", e);
                        println!("  \x1b[31m✗ Authentication failed: {}\x1b[0m", e);
                        println!("  \x1b[33m⚠ Falling back to dry-run mode\x1b[0m");
                        let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
                        Some(Arc::new(OrderExecutor::new(
                            client,
                            ExecutionConfig::default(),
                        )))
                    }
                }
            }
            Err(e) => {
                warn!("No wallet configured: {}", e);
                println!("  \x1b[33m⚠ POLYMARKET_PRIVATE_KEY not set\x1b[0m");
                println!("  \x1b[33m⚠ Running in observation mode (no orders)\x1b[0m");
                None
            }
        }
    };

    // Create strategy manager
    let manager = Arc::new(StrategyManager::new(1000)); // 1 second tick interval

    // Take the action receiver before starting strategy
    let action_rx = manager
        .take_action_receiver()
        .await
        .expect("Action receiver should be available");

    // Extract Binance feed requirements from strategy feeds
    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only: bool = true;

    for f in &required_feeds {
        match f {
            crate::strategy::DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            crate::strategy::DataFeed::BinanceKlines {
                symbols,
                intervals,
                closed_only,
            } => {
                binance_kline_symbols.extend(symbols.clone());
                binance_kline_intervals.extend(intervals.clone());
                if !*closed_only {
                    binance_kline_closed_only = false;
                }
            }
            _ => {}
        }
    }

    binance_spot_symbols.sort();
    binance_spot_symbols.dedup();
    binance_kline_symbols.sort();
    binance_kline_symbols.dedup();
    binance_kline_intervals.sort();
    binance_kline_intervals.dedup();

    // Create data feed manager with required feeds
    let mut feed_manager = DataFeedManager::new(manager.clone());

    if !binance_spot_symbols.is_empty() {
        println!(
            "  \x1b[36mConfiguring Binance spot feed: {:?}\x1b[0m",
            binance_spot_symbols
        );
        feed_manager = feed_manager.with_binance(binance_spot_symbols);
    }

    if !binance_kline_symbols.is_empty() && !binance_kline_intervals.is_empty() {
        let backfill_limit = std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(300);

        println!(
            "  \x1b[36mConfiguring Binance kline feed: symbols={:?} intervals={:?} closed_only={} backfill_limit={}\x1b[0m",
            binance_kline_symbols, binance_kline_intervals, binance_kline_closed_only, backfill_limit
        );
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    // Configure Polymarket if needed
    let has_polymarket_feed = required_feeds.iter().any(|f| {
        matches!(
            f,
            crate::strategy::DataFeed::PolymarketEvents { .. }
                | crate::strategy::DataFeed::PolymarketQuotes { .. }
        )
    });

    if has_polymarket_feed {
        println!("  \x1b[36mConfiguring Polymarket feed\x1b[0m");
        let pm_client = PolymarketClient::new("https://clob.polymarket.com", dry_run)?;
        let pm_ws =
            PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client);
    }

    // Start the strategy
    manager
        .start_strategy(strategy, Some(config_path.display().to_string()))
        .await
        .context("Failed to start strategy")?;

    println!("\x1b[32m✓ Strategy started\x1b[0m");

    #[cfg(feature = "claimer_daemon")]
    if !dry_run {
        if let Err(e) = crate::strategy::ensure_account_claimer_daemon().await {
            warn!("Failed to start account-level auto-claimer daemon: {}", e);
        }
    }

    // Start data feeds
    println!("  \x1b[36mStarting data feeds...\x1b[0m");
    feed_manager.start().await?;

    // Discover and subscribe to events based on strategy feeds
    let tokens = feed_manager.start_for_feeds(required_feeds).await?;
    if !tokens.is_empty() {
        println!("  \x1b[36mSubscribed to {} tokens\x1b[0m", tokens.len());
    }

    println!("\x1b[32m✓ Data feeds started\x1b[0m\n");

    // Initialize optional order persistence for strategy orders
    let order_store = init_order_store().await;

    // Spawn action handler task with executor + optional order store
    let action_handle = tokio::spawn(handle_strategy_actions(action_rx, executor, order_store));

    // Wait for shutdown signal
    println!("Press Ctrl+C to stop...\n");
    tokio::signal::ctrl_c().await?;

    println!("\n\x1b[33m⚠ Shutdown signal received\x1b[0m");

    // Graceful shutdown
    println!("Stopping strategy gracefully...");
    manager
        .stop_strategy(&strategy_id, true)
        .await
        .context("Failed to stop strategy")?;

    // Cancel action handler
    action_handle.abort();

    println!("\x1b[32m✓ Strategy stopped\x1b[0m");

    Ok(())
}

async fn init_order_store() -> Option<Arc<PostgresStore>> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            warn!("DATABASE_URL not set; strategy order persistence is disabled");
            println!("  \x1b[33m⚠ DATABASE_URL not set - order persistence disabled\x1b[0m");
            return None;
        }
    };

    match PostgresStore::new(&database_url, 3).await {
        Ok(store) => {
            println!("  \x1b[32m✓ Order persistence enabled (PostgreSQL)\x1b[0m");
            Some(Arc::new(store))
        }
        Err(e) => {
            error!(
                "Failed to connect to PostgreSQL for strategy order persistence: {}",
                e
            );
            println!(
                "  \x1b[33m⚠ Failed to connect PostgreSQL for order persistence: {}\x1b[0m",
                e
            );
            None
        }
    }
}

/// Handle actions emitted by strategies
async fn handle_strategy_actions(
    mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    executor: Option<Arc<OrderExecutor>>,
    store: Option<Arc<PostgresStore>>,
) {
    use crate::strategy::StrategyAction;

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitOrder {
                client_order_id,
                mut order,
                priority: _,
                ..
            } => {
                if order.client_order_id != client_order_id {
                    warn!(
                        "Mismatched order IDs in strategy action: action={}, request={}; using action ID",
                        client_order_id, order.client_order_id
                    );
                    order.client_order_id = client_order_id.clone();
                }

                let tracked_order_id = order.client_order_id.clone();
                let price_cents = order.limit_price * rust_decimal::Decimal::from(100);
                println!("\n  \x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
                println!("  \x1b[36m║\x1b[0m  📤 ORDER SUBMISSION                                          \x1b[36m║\x1b[0m");
                println!("  \x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
                println!(
                    "  \x1b[36m║\x1b[0m  Strategy: {:<47}\x1b[36m║\x1b[0m",
                    strategy_id
                );
                println!(
                    "  \x1b[36m║\x1b[0m  Order ID: {:<47}\x1b[36m║\x1b[0m",
                    tracked_order_id
                );
                println!(
                    "  \x1b[36m║\x1b[0m  Token: {:<50}\x1b[36m║\x1b[0m",
                    &order.token_id[..order.token_id.len().min(50)]
                );
                println!("  \x1b[36m║\x1b[0m  Side: {:?}, Shares: {}, Price: {:.2}¢{:<20}\x1b[36m║\x1b[0m",
                    order.market_side, order.shares, price_cents, "");
                println!("  \x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");

                if let Some(ref store) = store {
                    let db_order = crate::domain::Order::from_request(
                        &order,
                        None,
                        1,
                        Some(strategy_id.clone()),
                    );
                    if let Err(e) = store.insert_order(&db_order).await {
                        warn!(
                            "Failed to persist strategy order {}: {}",
                            tracked_order_id, e
                        );
                    }
                }

                // Execute order if executor is available
                if let Some(ref exec) = executor {
                    info!(
                        "Executing order: {} @ {:.2}¢",
                        tracked_order_id, price_cents
                    );
                    match exec.execute(&order).await {
                        Ok(result) => {
                            println!("  \x1b[32m✓ Order executed!\x1b[0m");
                            println!("    Order ID: {}", result.order_id);
                            println!("    Status: {:?}", result.status);
                            println!("    Filled: {} shares", result.filled_shares);
                            if let Some(avg_price) = result.avg_fill_price {
                                println!(
                                    "    Avg Price: {:.2}¢",
                                    avg_price * rust_decimal::Decimal::from(100)
                                );
                            }
                            println!("    Time: {}ms\n", result.elapsed_ms);
                            info!(
                                "Order {} filled: {} shares @ {:?}",
                                result.order_id, result.filled_shares, result.avg_fill_price
                            );

                            if let Some(ref store) = store {
                                if let Err(e) = store
                                    .update_order_status(
                                        &tracked_order_id,
                                        crate::domain::OrderStatus::Submitted,
                                        Some(&result.order_id),
                                    )
                                    .await
                                {
                                    warn!(
                                        "Failed to update order {} to Submitted: {}",
                                        tracked_order_id, e
                                    );
                                }

                                if result.filled_shares > 0 {
                                    let fill_price =
                                        result.avg_fill_price.unwrap_or(order.limit_price);
                                    if let Err(e) = store
                                        .update_order_fill(
                                            &tracked_order_id,
                                            result.filled_shares,
                                            fill_price,
                                            result.status,
                                        )
                                        .await
                                    {
                                        warn!(
                                            "Failed to update order fill for {}: {}",
                                            tracked_order_id, e
                                        );
                                    }
                                } else if let Err(e) = store
                                    .update_order_status(&tracked_order_id, result.status, None)
                                    .await
                                {
                                    warn!(
                                        "Failed to update order {} status to {:?}: {}",
                                        tracked_order_id, result.status, e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            println!("  \x1b[31m✗ Order failed: {}\x1b[0m\n", e);
                            error!("Order execution failed: {}", e);
                            if let Some(ref store) = store {
                                if let Err(db_err) = store
                                    .update_order_status(
                                        &tracked_order_id,
                                        crate::domain::OrderStatus::Failed,
                                        None,
                                    )
                                    .await
                                {
                                    warn!(
                                        "Failed to mark order {} as Failed: {}",
                                        tracked_order_id, db_err
                                    );
                                }
                            }
                        }
                    }
                } else {
                    println!("  \x1b[33m⚠ No executor - order logged but not submitted\x1b[0m\n");
                    warn!(
                        "Order {} not executed - no executor configured",
                        tracked_order_id
                    );
                    if let Some(ref store) = store {
                        if let Err(e) = store
                            .update_order_status(
                                &tracked_order_id,
                                crate::domain::OrderStatus::Failed,
                                None,
                            )
                            .await
                        {
                            warn!(
                                "Failed to mark non-executed order {} as Failed: {}",
                                tracked_order_id, e
                            );
                        }
                    }
                }
            }
            StrategyAction::CancelOrder { order_id } => {
                println!("  \x1b[33m[{}]\x1b[0m Cancel: {}", strategy_id, order_id);
                if let Some(ref exec) = executor {
                    match exec.cancel(&order_id).await {
                        Ok(true) => println!("  \x1b[32m✓ Order cancelled\x1b[0m"),
                        Ok(false) => {
                            println!("  \x1b[33m⚠ Order not found or already cancelled\x1b[0m")
                        }
                        Err(e) => println!("  \x1b[31m✗ Cancel failed: {}\x1b[0m", e),
                    }
                }
            }
            StrategyAction::ModifyOrder {
                order_id,
                new_price,
                new_size,
            } => {
                println!(
                    "  \x1b[33m[{}]\x1b[0m Modify: {} price={:?} size={:?}",
                    strategy_id, order_id, new_price, new_size
                );
                warn!("Order modification not yet implemented");
            }
            StrategyAction::Alert { level, message } => {
                let color = match level {
                    crate::strategy::AlertLevel::Info => "\x1b[36m",
                    crate::strategy::AlertLevel::Warning => "\x1b[33m",
                    crate::strategy::AlertLevel::Error => "\x1b[31m",
                    crate::strategy::AlertLevel::Critical => "\x1b[31;1m",
                };
                println!(
                    "  {}[{}] {:?}: {}\x1b[0m",
                    color, strategy_id, level, message
                );
            }
            StrategyAction::LogEvent { event } => {
                println!(
                    "  \x1b[90m[{}] {:?}: {}\x1b[0m",
                    strategy_id, event.event_type, event.message
                );
            }
        }
    }
}

/// Run strategy as daemon
async fn run_strategy_daemon(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    // Ensure run directory exists
    let run_dir = run_dir();
    fs::create_dir_all(&run_dir)?;

    let pid_file = run_dir.join(format!("{}.pid", name));
    let log_file = log_dir().join(format!("{}.log", name));

    // Build command
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("strategy")
        .arg("start")
        .arg(name)
        .arg("--config")
        .arg(config_path)
        .arg("--foreground");

    if dry_run {
        cmd.arg("--dry-run");
    }

    // Redirect output to log file
    fs::create_dir_all(log_dir())?;
    let log = fs::File::create(&log_file)?;
    let log_err = log.try_clone()?;

    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    // Spawn daemon
    let child = cmd.spawn().context("Failed to spawn strategy process")?;

    // Write PID file
    fs::write(&pid_file, child.id().to_string())?;

    println!(
        "\x1b[32m✓ Strategy '{}' started (PID: {})\x1b[0m",
        name,
        child.id()
    );
    println!("  Log file: {}", log_file.display());
    println!("  PID file: {}", pid_file.display());
    println!("\n  Use 'ploy strategy logs {} -f' to follow logs", name);

    Ok(())
}


// === Helper Types and Functions ===

#[derive(Debug)]
enum StrategyStatus {
    Running(u32),
    Stopped,
    Error(String),
}

fn get_strategy_status(name: &str) -> StrategyStatus {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if pid_file.exists() {
        match fs::read_to_string(&pid_file) {
            Ok(content) => match content.trim().parse::<u32>() {
                Ok(pid) => {
                    if is_process_running(pid) {
                        return StrategyStatus::Running(pid);
                    }
                    // Stale PID file: fall through to other detection paths.
                    let _ = fs::remove_file(&pid_file);
                }
                Err(_) => {
                    let _ = fs::remove_file(&pid_file);
                }
            },
            Err(e) => return StrategyStatus::Error(e.to_string()),
        }
    }

    // If the strategy is run under systemd (recommended on EC2), we won't have pidfiles.
    // Detect `ploy-strategy-<name>-dryrun.service` (and a few variants) and show it as running.
    #[cfg(target_os = "linux")]
    {
        if let Some(status) = systemd_strategy_status(name) {
            return status;
        }
    }

    StrategyStatus::Stopped
}

fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), Signal::SIGCONT).is_ok()
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, just assume it's running if PID file exists
        true
    }
}

fn get_process_uptime(_pid: u32) -> Option<String> {
    // TODO: Implement actual uptime calculation
    Some("--".into())
}

#[cfg(target_os = "linux")]
fn systemd_strategy_status(name: &str) -> Option<StrategyStatus> {
    if Command::new("systemctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return None;
    }

    let slug = name.replace('_', "-");
    let mut candidates = vec![
        format!("ploy-strategy-{}-dryrun.service", slug),
        format!("ploy-strategy-{}.service", slug),
        // Compatibility fallback (older units may have kept underscores).
        format!("ploy-strategy-{}-dryrun.service", name),
        format!("ploy-strategy-{}.service", name),
    ];
    candidates.dedup();

    for unit in candidates {
        let out = Command::new("systemctl")
            .arg("is-active")
            .arg(&unit)
            .output()
            .ok()?;

        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match state.as_str() {
            "active" | "activating" | "reloading" | "deactivating" => {
                // MainPID can be 0 for some unit types; treat as running anyway.
                let pid_out = Command::new("systemctl")
                    .arg("show")
                    .arg(&unit)
                    .arg("--property=MainPID")
                    .arg("--value")
                    .output()
                    .ok();

                let pid = pid_out
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);

                return Some(StrategyStatus::Running(pid));
            }
            "failed" => {
                return Some(StrategyStatus::Error(format!(
                    "systemd unit failed: {}",
                    unit
                )));
            }
            _ => {}
        }
    }

    None
}

fn create_default_config(name: &str, path: &PathBuf) -> Result<()> {
    let config = match name {
        "momentum" => include_str!("../../config/strategies/momentum_default.toml"),
        "split_arb" => include_str!("../../config/strategies/split_arb_default.toml"),
        "pattern_memory" => include_str!("../../config/strategies/pattern_memory_default.toml"),
        _ => return Ok(()), // No default for unknown strategies
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, config)?;
    Ok(())
}


async fn backtest_directional_signals_pm_settlement(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use crate::adapters::PostgresStore;
    use crate::strategy::fee_model::FeeModel;
    use chrono::{DateTime, Utc};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::coordinator::bootstrap::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;
    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Directional Signal Backtest (Settlement)                     ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} account_id={} agent_id={} live_only={} limit={} refresh={}",
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh
    );

    let rows = sqlx::query(
        r#"
        SELECT
            recorded_at,
            account_id,
            agent_id,
            strategy_id,
            token_id,
            symbol,
            side,
            confidence,
            market_price,
            edge,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = 'directional_entry'
          AND ($2::text IS NULL OR account_id = $2)
          AND ($3::text IS NULL OR agent_id = $3)
          AND ($4::bool = FALSE OR COALESCE((context->>'dry_run')::bool, false) = false)
        ORDER BY recorded_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query signal_history")?;

    if rows.is_empty() {
        println!("\n  No directional signals found in this window.\n");
        return Ok(());
    }

    #[derive(Debug, Clone)]
    struct SignalRow {
        recorded_at: DateTime<Utc>,
        token_id: String,
        symbol: Option<String>,
        entry_price: Decimal,
    }

    let mut signals: Vec<SignalRow> = Vec::with_capacity(rows.len());
    let mut token_ids: Vec<String> = Vec::with_capacity(rows.len());

    for row in rows {
        let recorded_at: DateTime<Utc> = row.get("recorded_at");
        let token_id: Option<String> = row.get("token_id");
        let Some(token_id) = token_id else { continue };
        let entry_price: Option<Decimal> = row.get("market_price");
        let Some(entry_price) = entry_price else {
            continue;
        };

        let symbol: Option<String> = row.get("symbol");
        token_ids.push(token_id.clone());
        signals.push(SignalRow {
            recorded_at,
            token_id,
            symbol,
            entry_price,
        });
    }

    if signals.is_empty() {
        println!("\n  No usable signals found (missing token_id/market_price).\n");
        return Ok(());
    }

    token_ids.sort();
    token_ids.dedup();

    if !no_refresh {
        let existing = sqlx::query(
            r#"
            SELECT token_id, resolved
            FROM pm_token_settlements
            WHERE token_id = ANY($1)
            "#,
        )
        .bind(&token_ids)
        .fetch_all(store.pool())
        .await
        .context("Failed to query pm_token_settlements")?;

        let mut resolved_map: HashMap<String, bool> = HashMap::new();
        for row in existing {
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            resolved_map.insert(token_id, resolved);
        }

        let mut to_refresh: Vec<String> = token_ids
            .iter()
            .filter(|t| !resolved_map.get(*t).copied().unwrap_or(false))
            .cloned()
            .collect();

        const MAX_REFRESH: usize = 500;
        if to_refresh.len() > MAX_REFRESH {
            to_refresh.truncate(MAX_REFRESH);
        }

        if !to_refresh.is_empty() {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                to_refresh.len()
            );
        }

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed_markets = 0usize;
        let mut refreshed_tokens = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(&token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "failed to fetch gamma market for token");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
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
                tracing::debug!(
                    token_id = %token_id,
                    market_id = %market.id,
                    "gamma market missing clob_token_ids or outcome_prices; skipping"
                );
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            // Treat as "officially settled" only once the market is closed and prices are 1/0.
            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                if let Err(e) = sqlx::query(
                    r#"
                    INSERT INTO pm_token_settlements (
                        token_id,
                        condition_id,
                        market_id,
                        market_slug,
                        outcome,
                        settled_price,
                        resolved,
                        resolved_at,
                        fetched_at,
                        raw_market
                    )
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                    ON CONFLICT (token_id) DO UPDATE SET
                        condition_id = EXCLUDED.condition_id,
                        market_id = EXCLUDED.market_id,
                        market_slug = EXCLUDED.market_slug,
                        outcome = EXCLUDED.outcome,
                        settled_price = EXCLUDED.settled_price,
                        resolved = EXCLUDED.resolved,
                        resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                        fetched_at = NOW(),
                        raw_market = EXCLUDED.raw_market
                    "#,
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
                .execute(store.pool())
                .await
                {
                    warn!(token_id = %token_id, error = %e, "failed to upsert pm_token_settlements row");
                    continue;
                }

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows\n",
                refreshed_markets, refreshed_tokens
            );
        }
    }

    let settlement_rows = sqlx::query(
        r#"
        SELECT token_id, resolved, settled_price, resolved_at
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(&token_ids)
    .fetch_all(store.pool())
    .await
    .context("Failed to query pm_token_settlements for signal tokens")?;

    #[derive(Debug, Clone)]
    struct SettlementRow {
        resolved: bool,
        settled_price: Option<Decimal>,
    }

    let mut settlements: HashMap<String, SettlementRow> = HashMap::new();
    for row in settlement_rows {
        let token_id: String = row.get("token_id");
        settlements.insert(
            token_id,
            SettlementRow {
                resolved: row.get("resolved"),
                settled_price: row.get("settled_price"),
            },
        );
    }

    let fee_model = FeeModel::crypto();
    let spread_cost = dec!(0.01);

    let mut total = 0usize;
    let mut resolved = 0usize;
    let mut wins = 0usize;
    let mut sum_pnl = 0.0f64;
    let mut equity = 0.0f64;
    let mut peak = 0.0f64;
    let mut max_dd = 0.0f64;

    let mut by_symbol: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new(); // (n, wins, pnl_sum)

    // Process oldest→newest for drawdown.
    signals.sort_by_key(|s| s.recorded_at);
    for s in &signals {
        total += 1;

        let entry_price_f64 = s.entry_price.to_f64().unwrap_or(0.0);
        let fee_rate = fee_model.effective_rate(s.entry_price);
        let fee_per_share = (s.entry_price * fee_rate).to_f64().unwrap_or(0.0);
        let costs = fee_per_share + spread_cost.to_f64().unwrap_or(0.01);

        let Some(settlement) = settlements.get(&s.token_id) else {
            continue;
        };
        if !settlement.resolved {
            continue;
        }
        let Some(settled_price) = settlement.settled_price else {
            continue;
        };

        resolved += 1;
        let payout = settled_price.to_f64().unwrap_or(0.0);
        let win = payout >= 0.99;
        if win {
            wins += 1;
        }

        let pnl = payout - entry_price_f64 - costs;
        sum_pnl += pnl;
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }

        let sym = s.symbol.clone().unwrap_or_else(|| "UNKNOWN".to_string());
        let entry = by_symbol.entry(sym).or_insert((0, 0, 0.0));
        entry.0 += 1;
        if win {
            entry.1 += 1;
        }
        entry.2 += pnl;
    }

    if resolved == 0 {
        println!("\n  Signals: {} (0 resolved yet). Wait for settlements, or run with longer lookback.\n", total);
        return Ok(());
    }

    let win_rate = wins as f64 / resolved as f64;
    let avg_pnl = sum_pnl / resolved as f64;

    println!(
        "\n  Signals: {} (resolved: {}) | Win rate: {:.1}% | Avg PnL/share: {:+.4} | Total PnL: {:+.4} | Max DD: {:.4}\n",
        total,
        resolved,
        win_rate * 100.0,
        avg_pnl,
        sum_pnl,
        max_dd
    );

    println!("  By symbol (resolved only):");
    for (sym, (n, w, pnl_sum)) in by_symbol {
        if n == 0 {
            continue;
        }
        println!(
            "    {:<8} n={:<5} win={:>5.1}% pnl_sum={:+.4} avg={:+.4}",
            sym,
            n,
            (w as f64 / n as f64) * 100.0,
            pnl_sum,
            pnl_sum / n as f64
        );
    }

    Ok(())
}


fn is_market_resolved(prices: &[rust_decimal::Decimal]) -> bool {
    if prices.is_empty() {
        return false;
    }
    let winners = prices
        .iter()
        .filter(|p| **p >= rust_decimal_macros::dec!(0.99))
        .count();
    let losers = prices
        .iter()
        .filter(|p| **p <= rust_decimal_macros::dec!(0.01))
        .count();
    winners == 1 && losers == prices.len().saturating_sub(1)
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

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(pool)
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


