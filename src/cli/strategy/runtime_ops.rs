use super::*;

pub(super) async fn list_strategies() -> Result<()> {
    let strategies_dir = config_dir().join("strategies");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Available Strategies                                         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

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

pub(super) async fn start_strategy(
    name: &str,
    config: Option<PathBuf>,
    dry_run: bool,
    foreground: bool,
) -> Result<()> {
    info!("Starting strategy: {}", name);

    if !dry_run {
        let result = crate::safety::direct_live::enforce_live_gate("ploy strategy start");
        if let Err(ref e) = result {
            warn!("{e}");
            println!("\x1b[31m✗ {e}\x1b[0m");
        }
        result?;
    }

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

    let config_path = config.unwrap_or_else(|| {
        config_dir()
            .join("strategies")
            .join(format!("{}.toml", name))
    });

    if !config_path.exists() {
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
        run_strategy_foreground(name, &config_path, dry_run).await
    } else {
        run_strategy_daemon(name, &config_path, dry_run).await
    }
}

async fn run_strategy_foreground(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    use crate::adapters::PolymarketWebSocket;
    use crate::strategy::{DataFeed, DataFeedManager};

    let config_content = fs::read_to_string(config_path)
        .context(format!("Failed to read config: {}", config_path.display()))?;

    println!(
        "\x1b[32m▶ Running {} in foreground (Ctrl+C to stop)\x1b[0m\n",
        name
    );

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

    let executor = if dry_run {
        println!("  \x1b[33m⚠ DRY RUN MODE - Orders will be simulated\x1b[0m");
        let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
        Some(Arc::new(OrderExecutor::new(
            client,
            ExecutionConfig::default(),
        )))
    } else {
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
                        true,
                    )
                    .await
                } else {
                    PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, true)
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

    let manager = Arc::new(StrategyManager::new(1000));
    let action_rx = manager
        .take_action_receiver()
        .await
        .expect("Action receiver should be available");

    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only = true;

    for feed in &required_feeds {
        match feed {
            DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            DataFeed::BinanceKlines {
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
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit
        );
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    let has_polymarket_feed = required_feeds.iter().any(|feed| {
        matches!(
            feed,
            DataFeed::PolymarketEvents { .. } | DataFeed::PolymarketQuotes { .. }
        )
    });

    if has_polymarket_feed {
        println!("  \x1b[36mConfiguring Polymarket feed\x1b[0m");
        let pm_client = PolymarketClient::new("https://clob.polymarket.com", dry_run)?;
        let pm_ws =
            PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client);
    }

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

    println!("  \x1b[36mStarting data feeds...\x1b[0m");
    feed_manager.start().await?;

    let tokens = feed_manager.start_for_feeds(required_feeds).await?;
    if !tokens.is_empty() {
        println!("  \x1b[36mSubscribed to {} tokens\x1b[0m", tokens.len());
    }

    println!("\x1b[32m✓ Data feeds started\x1b[0m\n");

    let order_store = init_order_store().await;
    let action_handle = tokio::spawn(handle_strategy_actions(action_rx, executor, order_store));

    println!("Press Ctrl+C to stop...\n");
    tokio::signal::ctrl_c().await?;

    println!("\n\x1b[33m⚠ Shutdown signal received\x1b[0m");
    println!("Stopping strategy gracefully...");
    manager
        .stop_strategy(&strategy_id, true)
        .await
        .context("Failed to stop strategy")?;

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

async fn handle_strategy_actions(
    mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    executor: Option<Arc<OrderExecutor>>,
    store: Option<Arc<PostgresStore>>,
) {
    use crate::strategy::StrategyAction;

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitIntent { intent } => {
                let client_order_id = intent.client_order_id.clone();
                let mut order = crate::strategy::order_request_from_intent(&intent);
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
                println!(
                    "  \x1b[36m║\x1b[0m  Side: {:?}, Shares: {}, Price: {:.2}¢{:<20}\x1b[36m║\x1b[0m",
                    order.market_side, order.shares, price_cents, ""
                );
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

async fn run_strategy_daemon(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    let run_dir = run_dir();
    fs::create_dir_all(&run_dir)?;

    let pid_file = run_dir.join(format!("{}.pid", name));
    let log_file = log_dir().join(format!("{}.log", name));

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

    fs::create_dir_all(log_dir())?;
    let log = fs::File::create(&log_file)?;
    let log_err = log.try_clone()?;

    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    let child = cmd.spawn().context("Failed to spawn strategy process")?;
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

pub(super) async fn stop_strategy(name: &str, force: bool) -> Result<()> {
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
                let _ = fs::remove_file(&pid_file);
                println!("\x1b[32m✓ Strategy '{}' stopped\x1b[0m", name);
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to stop: {}\x1b[0m", e);
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

pub(super) async fn show_status(name: Option<&str>) -> Result<()> {
    println!("\n{}", "=".repeat(60));
    println!("  STRATEGY STATUS");
    println!("{}\n", "=".repeat(60));

    let strategies = if let Some(name) = name {
        vec![name.to_string()]
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

    for strategy_name in strategies {
        let status = get_strategy_status(&strategy_name);
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
                    strategy_name, "● running", pid_str, uptime
                );
            }
            StrategyStatus::Stopped => {
                println!(
                    "  {:<15} \x1b[90m{:<12}\x1b[0m {:<10} {}",
                    strategy_name, "○ stopped", "-", "-"
                );
            }
            StrategyStatus::Error(error) => {
                println!(
                    "  {:<15} \x1b[31m{:<12}\x1b[0m {:<10} {}",
                    strategy_name, "✗ error", "-", error
                );
            }
        }
    }

    println!("\n{}", "=".repeat(60));
    Ok(())
}

pub(super) async fn show_logs(name: &str, tail: usize, follow: bool) -> Result<()> {
    let log_file = log_dir().join(format!("{}.log", name));

    if !log_file.exists() {
        println!("\x1b[33m⚠ No log file found for '{}'\x1b[0m", name);
        println!("  Expected: {}", log_file.display());
        return Ok(());
    }

    if follow {
        let mut child = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .spawn()
            .context("Failed to run tail")?;

        child.wait()?;
    } else {
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

pub(super) async fn reload_strategy(name: &str) -> Result<()> {
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

fn config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

fn run_dir() -> PathBuf {
    std::env::var("PLOY_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/run"))
}

fn log_dir() -> PathBuf {
    std::env::var("PLOY_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/logs"))
}

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
                    let _ = fs::remove_file(&pid_file);
                }
                Err(_) => {
                    let _ = fs::remove_file(&pid_file);
                }
            },
            Err(e) => return StrategyStatus::Error(e.to_string()),
        }
    }

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
        true
    }
}

fn get_process_uptime(_pid: u32) -> Option<String> {
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
                let pid_out = Command::new("systemctl")
                    .arg("show")
                    .arg(&unit)
                    .arg("--property=MainPID")
                    .arg("--value")
                    .output()
                    .ok();

                let pid = pid_out
                    .as_ref()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .and_then(|value| value.parse::<u32>().ok())
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
        "momentum" => include_str!("../../../config/strategies/momentum_default.toml"),
        "split_arb" => include_str!("../../../config/strategies/split_arb_default.toml"),
        "pattern_memory" => include_str!("../../../config/strategies/pattern_memory_default.toml"),
        _ => return Ok(()),
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, config)?;
    Ok(())
}
