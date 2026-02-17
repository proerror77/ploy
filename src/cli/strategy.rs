//! Strategy management commands
//!
//! ploy strategy list              - List all strategies and their status
//! ploy strategy start <name>      - Start a strategy
//! ploy strategy stop <name>       - Stop a strategy
//! ploy strategy status [name]     - Show strategy status
//! ploy strategy logs <name>       - View strategy logs
//! ploy strategy reload <name>     - Reload strategy config

use anyhow::{Context, Result};
use clap::Subcommand;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::PolymarketClient;
use crate::config::ExecutionConfig;
use crate::signing::Wallet;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::{StrategyFactory, StrategyManager};

/// Strategy-related commands
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

    /// Run the NBA Q3→Q4 comeback trading agent standalone
    NbaComeback {
        /// Config file path
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Run in dry-run mode
        #[arg(long)]
        dry_run: bool,
    },

    /// Report prediction accuracy using Polymarket official settlement (token pays 1/0)
    Accuracy {
        /// Lookback window in hours (scopes which entry intents are scored)
        #[arg(long, default_value = "12")]
        lookback_hours: u64,

        /// Filter by domain: crypto|sports|politics
        #[arg(long)]
        domain: Option<String>,

        /// Filter by account_id (defaults to all)
        #[arg(long)]
        account_id: Option<String>,

        /// Filter by agent_id (defaults to all)
        #[arg(long)]
        agent_id: Option<String>,

        /// Only include live orders (exclude dry-run)
        #[arg(long)]
        live_only: bool,

        /// Max number of intents to print (latest first)
        #[arg(long, default_value = "200")]
        limit: usize,

        /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
        #[arg(long)]
        no_refresh: bool,

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
            Self::Accuracy {
                lookback_hours,
                domain,
                account_id,
                agent_id,
                live_only,
                limit,
                no_refresh,
                database_url,
            } => {
                report_accuracy_pm_settlement(
                    lookback_hours,
                    domain,
                    account_id,
                    agent_id,
                    live_only,
                    limit,
                    no_refresh,
                    database_url,
                )
                .await
            }
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

/// List all available strategies
async fn list_strategies() -> Result<()> {
    let strategies_dir = config_dir().join("strategies");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Available Strategies                                         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Get strategies from factory
    let available = StrategyFactory::available_strategies();

    println!("  {:<15} {:<12} {}", "NAME", "STATUS", "DESCRIPTION");
    println!("  {}", "-".repeat(55));

    for strategy_info in &available {
        let status = get_strategy_status(&strategy_info.name);
        let status_str = match status {
            StrategyStatus::Running(_) => "\x1b[32m● running\x1b[0m",
            StrategyStatus::Stopped => "\x1b[90m○ stopped\x1b[0m",
            StrategyStatus::Error(_) => "\x1b[31m✗ error\x1b[0m",
        };
        println!(
            "  {:<15} {:<20} {}",
            strategy_info.name, status_str, strategy_info.description
        );
    }

    // Check for custom strategy configs
    if strategies_dir.exists() {
        println!("\n  Custom Configs:");
        println!("  {}", "-".repeat(55));

        if let Ok(entries) = fs::read_dir(&strategies_dir) {
            let mut found = false;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "toml").unwrap_or(false) {
                    if let Some(stem) = path.file_stem() {
                        let name = stem.to_string_lossy();
                        // Skip default configs
                        if !name.ends_with("_default") {
                            println!("  {:<15} (config: {})", name, path.display());
                            found = true;
                        }
                    }
                }
            }
            if !found {
                println!("  \x1b[90m(no custom configs found)\x1b[0m");
            }
        }
    }

    println!("\n  Commands:");
    println!("  {}", "-".repeat(55));
    println!("  ploy strategy start <name>     Start a strategy");
    println!("  ploy strategy stop <name>      Stop a running strategy");
    println!("  ploy strategy status           Show all strategy status");
    println!("  ploy strategy logs <name>      View strategy logs\n");

    Ok(())
}

/// Start a strategy
async fn start_strategy(
    name: &str,
    config: Option<PathBuf>,
    dry_run: bool,
    foreground: bool,
) -> Result<()> {
    info!("Starting strategy: {}", name);

    // Check if already running.
    //
    // NOTE: when invoked as a systemd service (ExecStart), the unit can appear "active"
    // while we are starting. In that case, `get_strategy_status()` would detect the unit
    // and we'd exit immediately, causing a restart loop. Skip the check under systemd.
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some()
        || std::env::var_os("SYSTEMD_EXEC_PID").is_some()
        || std::env::var_os("JOURNAL_STREAM").is_some();

    if !under_systemd {
        if let StrategyStatus::Running(pid) = get_strategy_status(name) {
            println!(
                "\x1b[33m⚠ Strategy '{}' is already running (PID: {})\x1b[0m",
                name, pid
            );
            println!("  Use 'ploy strategy stop {}' first", name);
            return Ok(());
        }
    }

    // Find config file
    let config_path = config.unwrap_or_else(|| {
        config_dir()
            .join("strategies")
            .join(format!("{}.toml", name))
    });

    if !config_path.exists() {
        // Try to use default config
        let default_config = config_dir()
            .join("strategies")
            .join(format!("{}_default.toml", name));
        if !default_config.exists() {
            println!("\x1b[33m⚠ No config found for '{}'.\x1b[0m", name);
            println!("  Creating default config at: {}", config_path.display());
            create_default_config(name, &config_path)?;
        }
    }

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Starting Strategy: {:<40}║\x1b[0m", name);
    println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "\x1b[36m║\x1b[0m  Config: {:<51}\x1b[36m║\x1b[0m",
        config_path.display()
    );
    println!(
        "\x1b[36m║\x1b[0m  Dry Run: {:<50}\x1b[36m║\x1b[0m",
        if dry_run { "YES" } else { "NO" }
    );
    println!(
        "\x1b[36m║\x1b[0m  Mode: {:<53}\x1b[36m║\x1b[0m",
        if foreground { "foreground" } else { "daemon" }
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    if foreground {
        // Run in foreground - exec directly
        run_strategy_foreground(name, &config_path, dry_run).await
    } else {
        // Run as daemon
        run_strategy_daemon(name, &config_path, dry_run).await
    }
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
                match PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    false, // neg_risk: use standard risk settings
                )
                .await
                {
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

    // Start data feeds
    println!("  \x1b[36mStarting data feeds...\x1b[0m");
    feed_manager.start().await?;

    // Discover and subscribe to events based on strategy feeds
    let tokens = feed_manager.start_for_feeds(required_feeds).await?;
    if !tokens.is_empty() {
        println!("  \x1b[36mSubscribed to {} tokens\x1b[0m", tokens.len());
    }

    println!("\x1b[32m✓ Data feeds started\x1b[0m\n");

    // Spawn action handler task with executor
    let action_handle = tokio::spawn(handle_strategy_actions(action_rx, executor));

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

/// Handle actions emitted by strategies
async fn handle_strategy_actions(
    mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    executor: Option<Arc<OrderExecutor>>,
) {
    use crate::strategy::StrategyAction;

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitOrder {
                client_order_id,
                order,
                priority: _,
            } => {
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
                    client_order_id
                );
                println!(
                    "  \x1b[36m║\x1b[0m  Token: {:<50}\x1b[36m║\x1b[0m",
                    &order.token_id[..order.token_id.len().min(50)]
                );
                println!("  \x1b[36m║\x1b[0m  Side: {:?}, Shares: {}, Price: {:.2}¢{:<20}\x1b[36m║\x1b[0m",
                    order.market_side, order.shares, price_cents, "");
                println!("  \x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");

                // Execute order if executor is available
                if let Some(ref exec) = executor {
                    info!("Executing order: {} @ {:.2}¢", client_order_id, price_cents);
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
                        }
                        Err(e) => {
                            println!("  \x1b[31m✗ Order failed: {}\x1b[0m\n", e);
                            error!("Order execution failed: {}", e);
                        }
                    }
                } else {
                    println!("  \x1b[33m⚠ No executor - order logged but not submitted\x1b[0m\n");
                    warn!(
                        "Order {} not executed - no executor configured",
                        client_order_id
                    );
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
            StrategyAction::UpdateRisk { level, reason } => {
                println!(
                    "  \x1b[35m[{}]\x1b[0m Risk: {:?} - {}",
                    strategy_id, level, reason
                );
            }
            StrategyAction::SubscribeFeed { feed } => {
                println!("  \x1b[90m[{}]\x1b[0m Subscribe: {:?}", strategy_id, feed);
            }
            StrategyAction::UnsubscribeFeed { feed } => {
                println!("  \x1b[90m[{}]\x1b[0m Unsubscribe: {:?}", strategy_id, feed);
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

/// Stop a running strategy
async fn stop_strategy(name: &str, force: bool) -> Result<()> {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if !pid_file.exists() {
        println!("\x1b[33m⚠ Strategy '{}' is not running\x1b[0m", name);
        return Ok(());
    }

    let pid: u32 = fs::read_to_string(&pid_file)?
        .trim()
        .parse()
        .context("Invalid PID file")?;

    let signal = if force { "SIGKILL" } else { "SIGTERM" };

    println!(
        "Stopping strategy '{}' (PID: {}) with {}...",
        name, pid, signal
    );

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let sig = if force {
            Signal::SIGKILL
        } else {
            Signal::SIGTERM
        };
        match kill(Pid::from_raw(pid as i32), sig) {
            Ok(_) => {
                // Remove PID file
                let _ = fs::remove_file(&pid_file);
                println!("\x1b[32m✓ Strategy '{}' stopped\x1b[0m", name);
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to stop: {}\x1b[0m", e);
                // Clean up stale PID file
                let _ = fs::remove_file(&pid_file);
            }
        }
    }

    #[cfg(not(unix))]
    {
        println!("\x1b[33m⚠ Signal handling not supported on this platform\x1b[0m");
        println!("  Manually kill process with PID: {}", pid);
    }

    Ok(())
}

/// Show strategy status
async fn show_status(name: Option<&str>) -> Result<()> {
    println!("\n{}", "=".repeat(60));
    println!("  STRATEGY STATUS");
    println!("{}\n", "=".repeat(60));

    let strategies = if let Some(n) = name {
        vec![n.to_string()]
    } else {
        vec![
            "momentum".into(),
            "split_arb".into(),
            "pattern_memory".into(),
            "sports".into(),
            "politics".into(),
        ]
    };

    println!(
        "  {:<15} {:<12} {:<10} {}",
        "NAME", "STATUS", "PID", "UPTIME"
    );
    println!("  {}", "-".repeat(55));

    for strat_name in strategies {
        let status = get_strategy_status(&strat_name);
        match status {
            StrategyStatus::Running(pid) => {
                let pid_str = if pid == 0 {
                    "-".to_string()
                } else {
                    pid.to_string()
                };
                let uptime = if pid == 0 {
                    "unknown".into()
                } else {
                    get_process_uptime(pid).unwrap_or_else(|| "unknown".into())
                };
                println!(
                    "  {:<15} \x1b[32m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "● running", pid_str, uptime
                );
            }
            StrategyStatus::Stopped => {
                println!(
                    "  {:<15} \x1b[90m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "○ stopped", "-", "-"
                );
            }
            StrategyStatus::Error(e) => {
                println!(
                    "  {:<15} \x1b[31m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "✗ error", "-", e
                );
            }
        }
    }

    println!("\n{}", "=".repeat(60));

    Ok(())
}

/// Show strategy logs
async fn show_logs(name: &str, tail: usize, follow: bool) -> Result<()> {
    let log_file = log_dir().join(format!("{}.log", name));

    if !log_file.exists() {
        println!("\x1b[33m⚠ No log file found for '{}'\x1b[0m", name);
        println!("  Expected: {}", log_file.display());
        return Ok(());
    }

    if follow {
        // Use tail -f
        let mut child = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .spawn()
            .context("Failed to run tail")?;

        child.wait()?;
    } else {
        // Just show last N lines
        let output = Command::new("tail")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .output()
            .context("Failed to run tail")?;

        print!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

/// Reload strategy configuration
async fn reload_strategy(name: &str) -> Result<()> {
    let pid_file = run_dir().join(format!("{}.pid", name));

    if !pid_file.exists() {
        println!("\x1b[33m⚠ Strategy '{}' is not running\x1b[0m", name);
        return Ok(());
    }

    let pid: u32 = fs::read_to_string(&pid_file)?
        .trim()
        .parse()
        .context("Invalid PID file")?;

    println!("Reloading config for strategy '{}' (PID: {})...", name, pid);

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        match kill(Pid::from_raw(pid as i32), Signal::SIGHUP) {
            Ok(_) => {
                println!("\x1b[32m✓ Reload signal sent\x1b[0m");
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to send reload signal: {}\x1b[0m", e);
            }
        }
    }

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
        // Back-compat (older units may have kept underscores).
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

/// Seed NBA team comeback stats into the database
async fn seed_nba_stats(season: &str, database_url: Option<String>) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::nba_data_collector::TeamStats;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!(
        "\x1b[36m║  NBA Team Stats Seeder (season: {:<27})║\x1b[0m",
        season
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    // Pre-computed comeback rates for all 30 NBA teams (2025-26 season estimates)
    // Format: (name, abbrev, comeback_5pt, comeback_10pt, comeback_15pt, q4_avg_pts, elo)
    let teams: &[(&str, &str, f64, f64, f64, f64, f64)] = &[
        ("Atlanta Hawks", "ATL", 0.38, 0.18, 0.06, 28.5, 1490.0),
        ("Boston Celtics", "BOS", 0.52, 0.30, 0.14, 30.2, 1620.0),
        ("Brooklyn Nets", "BKN", 0.32, 0.14, 0.04, 27.0, 1430.0),
        ("Charlotte Hornets", "CHA", 0.30, 0.12, 0.03, 26.8, 1420.0),
        ("Chicago Bulls", "CHI", 0.35, 0.16, 0.05, 27.5, 1470.0),
        ("Cleveland Cavaliers", "CLE", 0.48, 0.27, 0.12, 29.8, 1590.0),
        ("Dallas Mavericks", "DAL", 0.44, 0.24, 0.10, 29.2, 1560.0),
        ("Denver Nuggets", "DEN", 0.46, 0.26, 0.11, 29.5, 1580.0),
        ("Detroit Pistons", "DET", 0.28, 0.10, 0.03, 26.2, 1400.0),
        (
            "Golden State Warriors",
            "GSW",
            0.42,
            0.22,
            0.09,
            28.8,
            1540.0,
        ),
        ("Houston Rockets", "HOU", 0.40, 0.20, 0.08, 28.2, 1510.0),
        ("Indiana Pacers", "IND", 0.43, 0.23, 0.10, 29.5, 1530.0),
        ("LA Clippers", "LAC", 0.39, 0.19, 0.07, 28.0, 1500.0),
        ("Los Angeles Lakers", "LAL", 0.41, 0.21, 0.08, 28.5, 1520.0),
        ("Memphis Grizzlies", "MEM", 0.42, 0.22, 0.09, 28.8, 1530.0),
        ("Miami Heat", "MIA", 0.40, 0.20, 0.08, 28.0, 1510.0),
        ("Milwaukee Bucks", "MIL", 0.45, 0.25, 0.11, 29.5, 1570.0),
        (
            "Minnesota Timberwolves",
            "MIN",
            0.46,
            0.26,
            0.11,
            29.2,
            1580.0,
        ),
        (
            "New Orleans Pelicans",
            "NOP",
            0.36,
            0.17,
            0.06,
            27.8,
            1480.0,
        ),
        ("New York Knicks", "NYK", 0.44, 0.24, 0.10, 29.0, 1560.0),
        (
            "Oklahoma City Thunder",
            "OKC",
            0.50,
            0.29,
            0.13,
            30.0,
            1610.0,
        ),
        ("Orlando Magic", "ORL", 0.37, 0.18, 0.07, 27.5, 1490.0),
        ("Philadelphia 76ers", "PHI", 0.41, 0.21, 0.08, 28.5, 1520.0),
        ("Phoenix Suns", "PHX", 0.43, 0.23, 0.09, 29.0, 1540.0),
        (
            "Portland Trail Blazers",
            "POR",
            0.29,
            0.11,
            0.03,
            26.5,
            1410.0,
        ),
        ("Sacramento Kings", "SAC", 0.39, 0.19, 0.07, 28.2, 1500.0),
        ("San Antonio Spurs", "SAS", 0.31, 0.13, 0.04, 26.8, 1430.0),
        ("Toronto Raptors", "TOR", 0.33, 0.15, 0.05, 27.2, 1450.0),
        ("Utah Jazz", "UTA", 0.30, 0.12, 0.03, 26.5, 1420.0),
        ("Washington Wizards", "WAS", 0.27, 0.09, 0.02, 25.8, 1390.0),
    ];

    let mut count = 0;
    for &(name, abbrev, cr5, cr10, cr15, q4_avg, elo) in teams {
        let stats = TeamStats {
            team_name: name.to_string(),
            season: season.to_string(),
            wins: 0,
            losses: 0,
            win_rate: 0.0,
            avg_points: 0.0,
            q1_avg_points: 0.0,
            q2_avg_points: 0.0,
            q3_avg_points: 0.0,
            q4_avg_points: q4_avg,
            comeback_rate_5pt: cr5,
            comeback_rate_10pt: cr10,
            comeback_rate_15pt: cr15,
            elo_rating: Some(elo),
            offensive_rating: None,
            defensive_rating: None,
        };

        store
            .upsert_nba_team_stats(name, abbrev, season, &stats)
            .await
            .context(format!("Failed to upsert {}", abbrev))?;

        println!(
            "  \x1b[32m✓\x1b[0m {} ({}) — 5pt:{:.0}% 10pt:{:.0}% 15pt:{:.0}%",
            name,
            abbrev,
            cr5 * 100.0,
            cr10 * 100.0,
            cr15 * 100.0
        );
        count += 1;
    }

    println!(
        "\n\x1b[32m✓ Seeded {} teams for season {}\x1b[0m\n",
        count, season
    );
    Ok(())
}

async fn report_accuracy_pm_settlement(
    lookback_hours: u64,
    domain: Option<String>,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use anyhow::bail;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let account_id = account_id.or_else(|| std::env::var("PLOY_ACCOUNT__ID").ok());

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let domain_norm = domain
        .as_deref()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty());
    if let Some(ref d) = domain_norm {
        if !matches!(d.as_str(), "crypto" | "sports" | "politics") {
            bail!("invalid --domain: {d} (expected crypto|sports|politics)");
        }
    }

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Accuracy Report (Polymarket Settlement)                      ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} domain={} account_id={} agent_id={} live_only={} limit={} refresh={}",
        lookback_hours,
        domain_norm.as_deref().unwrap_or("all"),
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh
    );

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    // Pull latest entry intents within lookback window.
    //
    // NOTE:
    // - Some strategies express "DOWN" exposure via sells (short) rather than buys,
    //   so we can't filter to `is_buy = TRUE`.
    // - Prefer the explicit signal_type suffix when present.
    let rows = sqlx::query(
        r#"
        SELECT
            executed_at,
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            market_side,
            is_buy,
            limit_price,
            dry_run,
            filled_shares,
            metadata
        FROM agent_order_executions
        WHERE executed_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND filled_shares > 0
          AND (
                (metadata ? 'signal_type' AND RIGHT(metadata->>'signal_type', 6) = '_entry')
             OR (NOT (metadata ? 'signal_type') AND is_buy = TRUE)
          )
          AND ($2::text IS NULL OR LOWER(domain) = $2)
          AND ($3::text IS NULL OR account_id = $3)
          AND ($4::text IS NULL OR agent_id = $4)
          AND ($5::bool = FALSE OR dry_run = FALSE)
        ORDER BY executed_at DESC
        LIMIT $6
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(domain_norm.as_deref())
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query agent_order_executions")?;

    if rows.is_empty() {
        println!("\n  No filled entry intents found in this window.\n");
        return Ok(());
    }

    let mut token_ids: Vec<String> = Vec::with_capacity(rows.len());
    for row in &rows {
        let token_id: String = row.get("token_id");
        token_ids.push(token_id);
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

        // Avoid hammering Gamma on large windows; refresh a bounded set.
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
                if !seen_conditions.insert(cond.clone()) {
                    continue;
                }
            }

            let clob_ids = market
                .clob_token_ids
                .as_deref()
                .and_then(|s| parse_json_array_strings(s).ok())
                .unwrap_or_default();
            let outcomes = market
                .outcomes
                .as_deref()
                .and_then(|s| parse_json_array_strings(s).ok())
                .unwrap_or_default();
            let price_strs = market
                .outcome_prices
                .as_deref()
                .and_then(|s| parse_json_array_strings(s).ok())
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
            let condition_id = market.condition_id.clone();

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                sqlx::query(
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
                .context("Failed to upsert pm_token_settlements row")?;

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows",
                refreshed_markets, refreshed_tokens
            );
        }
    }

    // Final join for scoring.
    let scored_rows = sqlx::query(
        r#"
        SELECT
            e.executed_at,
            e.intent_id,
            e.agent_id,
            e.domain,
            e.market_slug,
            e.token_id,
            e.market_side,
            e.is_buy,
            e.limit_price,
            e.dry_run,
            e.metadata,
            s.resolved as pm_resolved,
            s.settled_price as pm_settled_price,
            s.outcome as pm_outcome
        FROM agent_order_executions e
        LEFT JOIN pm_token_settlements s
          ON s.token_id = e.token_id
        WHERE e.executed_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND e.filled_shares > 0
          AND (
                (e.metadata ? 'signal_type' AND RIGHT(e.metadata->>'signal_type', 6) = '_entry')
             OR (NOT (e.metadata ? 'signal_type') AND e.is_buy = TRUE)
          )
          AND ($2::text IS NULL OR LOWER(e.domain) = $2)
          AND ($3::text IS NULL OR e.account_id = $3)
          AND ($4::text IS NULL OR e.agent_id = $4)
          AND ($5::bool = FALSE OR e.dry_run = FALSE)
        ORDER BY e.executed_at DESC
        LIMIT $6
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(domain_norm.as_deref())
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query joined accuracy rows")?;

    let mut total = 0usize;
    let mut scored = 0usize;
    let mut wins = 0usize;
    let mut pending = 0usize;
    let mut by_agent: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // scored, wins

    for row in &scored_rows {
        total += 1;
        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let is_resolved = resolved.unwrap_or(false) && settled_price.is_some();

        if !is_resolved {
            pending += 1;
            continue;
        }

        scored += 1;
        let is_buy: bool = row.get("is_buy");
        let sp = settled_price.unwrap_or(Decimal::ZERO);
        let won = if is_buy {
            sp > dec!(0.5)
        } else {
            sp < dec!(0.5)
        };
        if won {
            wins += 1;
        }

        let agent: String = row.get("agent_id");
        let entry = by_agent.entry(agent).or_insert((0, 0));
        entry.0 += 1;
        if won {
            entry.1 += 1;
        }
    }

    let losses = scored.saturating_sub(wins);
    let acc = if scored > 0 {
        100.0 * (wins as f64) / (scored as f64)
    } else {
        0.0
    };

    println!("\n  Summary:");
    println!("  - intents_total:    {}", total);
    println!("  - intents_scored:   {}", scored);
    println!("  - wins:             {}", wins);
    println!("  - losses:           {}", losses);
    println!("  - pending:          {}", pending);
    println!("  - accuracy:         {:.2}%", acc);

    if !by_agent.is_empty() {
        println!("\n  By agent (scored, wins, accuracy):");
        for (agent, (a_scored, a_wins)) in by_agent.iter() {
            let a_acc = if *a_scored > 0 {
                100.0 * (*a_wins as f64) / (*a_scored as f64)
            } else {
                0.0
            };
            println!(
                "  - {:<20} scored={:<5} wins={:<5} acc={:.2}%",
                agent, a_scored, a_wins, a_acc
            );
        }
    }

    println!("\n  Latest intents:");
    println!("  Time (UTC)          Agent              Side  Dir   Entry  Settled Outcome        Result  Intent");
    println!("  ------------------  ------------------  ----  ----  -----  ------ -------------  ------  ------------------------------------");

    for row in &scored_rows {
        let executed_at: DateTime<Utc> = row.get("executed_at");
        let agent: String = row.get("agent_id");
        let side: String = row.get("market_side");
        let is_buy: bool = row.get("is_buy");
        let entry_price: Decimal = row.get("limit_price");
        let intent_id: uuid::Uuid = row.get("intent_id");

        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let outcome: Option<String> = row.try_get("pm_outcome").ok();

        let (settled_str, outcome_str, result_str) =
            if resolved.unwrap_or(false) && settled_price.is_some() {
                let sp = settled_price.unwrap_or(Decimal::ZERO);
                let won = if is_buy {
                    sp > dec!(0.5)
                } else {
                    sp < dec!(0.5)
                };
                (
                    format!("{:.3}", sp),
                    outcome.unwrap_or_else(|| "-".to_string()),
                    if won { "WIN" } else { "LOSE" }.to_string(),
                )
            } else {
                ("-".to_string(), "-".to_string(), "PENDING".to_string())
            };

        println!(
            "  {}  {:<18}  {:<4}  {:<4}  {:>5.1}¢  {:>6} {:<13}  {:<6}  {}",
            executed_at.format("%Y-%m-%d %H:%M"),
            agent,
            side,
            if is_buy { "BUY" } else { "SELL" },
            entry_price * dec!(100),
            settled_str,
            outcome_str,
            result_str,
            intent_id
        );
    }

    println!();
    Ok(())
}

fn parse_json_array_strings(input: &str) -> std::result::Result<Vec<String>, serde_json::Error> {
    let s = input.trim();
    if s.is_empty() || s == "null" {
        return Ok(Vec::new());
    }

    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Ok(v);
    }
    let vals = serde_json::from_str::<Vec<serde_json::Value>>(s)?;
    Ok(vals
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect())
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

/// Run the NBA comeback agent standalone
async fn run_nba_comeback(_config: Option<PathBuf>, _dry_run: bool) -> Result<()> {
    use crate::platform::DomainAgent;
    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  NBA Q3→Q4 Comeback Trading Agent                            ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Load config
    let app_config = crate::config::AppConfig::load().context("Failed to load config")?;

    let nba_cfg = app_config.nba_comeback.unwrap_or_else(|| {
        info!("No [nba_comeback] in config, using defaults");
        crate::config::NbaComebackConfig {
            enabled: true,
            min_edge: rust_decimal::Decimal::new(5, 2),
            max_entry_price: rust_decimal::Decimal::new(75, 2),
            shares: 50,
            cooldown_secs: 300,
            max_daily_spend_usd: rust_decimal::Decimal::new(100, 0),
            min_deficit: 1,
            max_deficit: 15,
            target_quarter: 3,
            espn_poll_interval_secs: 30,
            min_comeback_rate: 0.15,
            season: "2025-26".to_string(),
            grok_enabled: false,
            grok_interval_secs: 300,
            grok_min_edge: rust_decimal::Decimal::new(8, 2),
            grok_min_confidence: 0.6,
            grok_decision_cooldown_secs: 60,
            grok_fallback_enabled: true,
        }
    });

    println!("  Season: {}", nba_cfg.season);
    println!("  Min edge: {}", nba_cfg.min_edge);
    println!("  Max entry: {}", nba_cfg.max_entry_price);
    println!("  Shares: {}", nba_cfg.shares);
    println!("  Target quarter: Q{}", nba_cfg.target_quarter);
    println!("  ESPN poll interval: {}s", nba_cfg.espn_poll_interval_secs);
    println!(
        "  Min comeback rate: {:.0}%",
        nba_cfg.min_comeback_rate * 100.0
    );
    println!();

    // Connect to DB and load stats
    let db_url = app_config.database.url;
    let store = crate::adapters::PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    let mut stats_provider = crate::strategy::nba_comeback::ComebackStatsProvider::new(
        store.pool().clone(),
        nba_cfg.season.clone(),
    );
    stats_provider
        .load_all()
        .await
        .context("Failed to load team stats — run 'ploy strategy nba-seed-stats' first")?;

    println!(
        "  \x1b[32m✓\x1b[0m Loaded {} team profiles",
        stats_provider.team_count()
    );

    // Create core + agent
    let espn = crate::strategy::nba_comeback::EspnClient::new();
    let core =
        crate::strategy::nba_comeback::NbaComebackCore::new(espn, stats_provider, nba_cfg.clone());

    let mut agent = crate::platform::NbaComebackAgent::new(core);
    agent.start().await?;

    println!(
        "  \x1b[32m✓\x1b[0m Agent running — scanning every {}s",
        nba_cfg.espn_poll_interval_secs
    );
    println!("\nPress Ctrl+C to stop...\n");

    // Main loop: tick the agent
    let interval = std::time::Duration::from_secs(nba_cfg.espn_poll_interval_secs);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n\x1b[33m⚠ Shutdown signal received\x1b[0m");
                agent.stop().await?;
                println!("\x1b[32m✓ Agent stopped\x1b[0m");
                break;
            }
            _ = tokio::time::sleep(interval) => {
                use crate::platform::DomainAgent;
                let intents = agent.on_event(crate::platform::DomainEvent::Tick(chrono::Utc::now())).await?;
                if !intents.is_empty() {
                    for intent in &intents {
                        println!("  \x1b[36m📤 ORDER: {} {} shares @ {} (edge: {})\x1b[0m",
                            intent.metadata.get("trailing_team").unwrap_or(&"?".to_string()),
                            intent.shares,
                            intent.limit_price,
                            intent.metadata.get("edge").unwrap_or(&"?".to_string()),
                        );
                    }
                }
            }
        }
    }

    Ok(())
}
