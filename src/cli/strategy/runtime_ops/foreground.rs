use super::*;
use crate::adapters::PolymarketWebSocket;
use crate::strategy::{DataFeed, DataFeedManager, StrategyAction};
#[path = "foreground_submit.rs"]
mod foreground_submit;

pub(super) async fn run_strategy_foreground(
    name: &str,
    config_path: &PathBuf,
    dry_run: bool,
) -> Result<()> {
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
    let action_handle = tokio::spawn(handle_strategy_actions(
        action_rx,
        dry_run,
        executor,
        order_store,
    ));

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

pub(super) async fn init_order_store() -> Option<Arc<PostgresStore>> {
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

pub(super) async fn handle_strategy_actions(
    mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    dry_run: bool,
    executor: Option<Arc<OrderExecutor>>,
    store: Option<Arc<PostgresStore>>,
) {
    let submitter = foreground_submit::ForegroundIntentSubmitter::new(dry_run, executor.clone());

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitIntent { intent } => {
                foreground_submit::handle_submit_intent(
                    &strategy_id,
                    intent,
                    &submitter,
                    store.as_ref(),
                )
                .await;
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
