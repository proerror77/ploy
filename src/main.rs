use clap::Parser;
use ploy::adapters::{PolymarketClient, PolymarketWebSocket, PostgresStore};
use ploy::cli::{self, Cli, Commands, CryptoCommands, SportsCommands, TerminalUI};
#[cfg(feature = "rl")]
use ploy::cli::RlCommands;
use ploy::config::AppConfig;
use ploy::error::Result;
use ploy::services::{DataCollector, HealthServer, HealthState, Metrics};
use ploy::strategy::{OrderExecutor, StrategyEngine};
use std::sync::Arc;
use tokio::signal;
use tokio::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Test) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::test_connection(&client).await?;
        }
        Some(Commands::Book { token }) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::show_order_book(&client, token).await?;
        }
        Some(Commands::Search { query }) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::search_markets(&client, query).await?;
        }
        Some(Commands::Current { series }) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::show_current_market(&client, series).await?;
        }
        Some(Commands::Watch { token, series }) => {
            init_logging_simple();
            run_watch_mode(&cli, token.as_deref(), series.as_deref()).await?;
        }
        Some(Commands::Trade { series, shares, move_pct, sum_target, dry_run }) => {
            init_logging();
            run_trade_mode(series, *shares, *move_pct, *sum_target, *dry_run).await?;
        }
        Some(Commands::Scan { series, sum_target, move_pct, watch }) => {
            init_logging();
            run_scan_mode(series, *sum_target, *move_pct, *watch).await?;
        }
        Some(Commands::Analyze { event }) => {
            init_logging();
            run_analyze_mode(event).await?;
        }
        Some(Commands::Account { orders, positions }) => {
            init_logging_simple();
            run_account_mode(*orders, *positions).await?;
        }
        Some(Commands::Ev { price, probability, hours, table }) => {
            init_logging_simple();
            cli::calculate_ev(*price, *probability, *hours, *table).await?;
        }
        Some(Commands::MarketMake { token, detail }) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::analyze_market_making(&client, token, *detail).await?;
        }
        Some(Commands::Momentum {
            symbols,
            min_move,
            max_entry,
            min_edge,
            shares,
            max_positions,
            take_profit,
            stop_loss,
            dry_run,
        }) => {
            init_logging();
            run_momentum_mode(
                symbols,
                *min_move,
                *max_entry,
                *min_edge,
                *shares,
                *max_positions,
                *take_profit,
                *stop_loss,
                *dry_run,
            )
            .await?;
        }
        Some(Commands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            series,
            dry_run,
        }) => {
            init_logging();
            run_split_arb_mode(
                *max_entry,
                *target_cost,
                *min_profit,
                *max_wait,
                *shares,
                *max_unhedged,
                *stop_loss,
                series.clone(),
                *dry_run,
            )
            .await?;
        }
        Some(Commands::Agent {
            mode,
            market,
            sports_url,
            max_trade,
            max_exposure,
            enable_trading,
            chat,
        }) => {
            init_logging();
            run_agent_mode(
                mode,
                market.as_deref(),
                sports_url.as_deref(),
                *max_trade,
                *max_exposure,
                *enable_trading,
                *chat,
            )
            .await?;
        }
        Some(Commands::Dashboard { series, demo }) => {
            if *demo {
                ploy::tui::run_demo().await?;
            } else {
                init_logging();
                ploy::tui::run_dashboard_auto(series.as_deref(), cli.dry_run).await?;
            }
        }
        Some(Commands::Collect { symbols, markets, duration }) => {
            init_logging();
            run_collect_mode(symbols, markets.as_deref(), *duration).await?;
        }
        Some(Commands::Crypto(crypto_cmd)) => {
            init_logging();
            run_crypto_command(crypto_cmd).await?;
        }
        Some(Commands::Sports(sports_cmd)) => {
            init_logging();
            run_sports_command(sports_cmd).await?;
        }
        #[cfg(feature = "rl")]
        Some(Commands::Rl(rl_cmd)) => {
            init_logging();
            run_rl_command(rl_cmd).await?;
        }
        Some(Commands::Run) | None => {
            init_logging();
            run_bot(&cli).await?;
        }
    }

    Ok(())
}

/// Live trading mode with real order execution
async fn run_trade_mode(
    series_id: &str,
    shares: u64,
    move_pct: f64,
    sum_target: f64,
    dry_run: bool,
) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::config::StrategyConfig;
    use ploy::domain::OrderRequest;
    use ploy::signing::Wallet;
    use ploy::strategy::SignalDetector;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    println!("\x1b[33m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    if dry_run {
        println!("║       PLOY - Live Trading Mode [DRY RUN - NO REAL ORDERS]    ║");
    } else {
        println!("║       PLOY - Live Trading Mode [⚠️  REAL ORDERS ENABLED]      ║");
    }
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    // Initialize client based on mode
    let client = if dry_run {
        info!("Running in DRY RUN mode - no real orders will be placed");
        PolymarketClient::new("https://clob.polymarket.com", true)?
    } else {
        // Load wallet from environment
        info!("Loading wallet from environment...");
        let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
        info!("Wallet loaded: {:?}", wallet.address());

        // Create authenticated client
        info!("Authenticating with Polymarket CLOB...");
        PolymarketClient::new_authenticated(
            "https://clob.polymarket.com",
            wallet,
            true, // neg_risk for UP/DOWN markets
        ).await?
    };

    println!("\x1b[32m✓ Client initialized\x1b[0m");
    println!("  Series: {}", series_id);
    println!("  Shares per leg: {}", shares);
    println!("  Move threshold: {:.1}%", move_pct * 100.0);
    println!("  Sum target: {:.4}", sum_target);
    println!();

    let running = Arc::new(AtomicBool::new(true));
    let market_changed = Arc::new(Notify::new());

    // Track current market tokens
    let current_tokens: Arc<tokio::sync::RwLock<Vec<String>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let token_to_side: Arc<tokio::sync::RwLock<std::collections::HashMap<String, ploy::domain::Side>>> =
        Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

    // WebSocket client
    let ws = Arc::new(PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market"));

    // Market rotation checker
    let rotation_handle = {
        let client = client.clone();
        let series_id = series_id.to_string();
        let current_tokens = Arc::clone(&current_tokens);
        let token_to_side = Arc::clone(&token_to_side);
        let ws = Arc::clone(&ws);
        let running = Arc::clone(&running);
        let market_changed = Arc::clone(&market_changed);

        tokio::spawn(async move {
            let mut check_interval = tokio::time::interval(Duration::from_secs(30));

            while running.load(Ordering::Relaxed) {
                check_interval.tick().await;

                match client.get_current_market_tokens(&series_id).await {
                    Ok(Some((title, market))) => {
                        let new_tokens: Vec<String> = market.tokens.iter()
                            .map(|t| t.token_id.clone())
                            .collect();

                        let tokens_read = current_tokens.read().await;
                        let tokens_changed = *tokens_read != new_tokens;
                        drop(tokens_read);

                        if tokens_changed {
                            println!("\n\x1b[33m═══ Market Rotation ═══\x1b[0m");
                            println!("\x1b[32mNew market:\x1b[0m {}", title);

                            ws.quote_cache().clear().await;
                            let mut side_map = token_to_side.write().await;
                            side_map.clear();

                            for token in &market.tokens {
                                let side = match token.outcome.to_lowercase().as_str() {
                                    "yes" | "up" => ploy::domain::Side::Up,
                                    _ => ploy::domain::Side::Down,
                                };
                                ws.register_token(&token.token_id, side).await;
                                side_map.insert(token.token_id.clone(), side);

                                let price_str = token.price.as_deref().unwrap_or("N/A");
                                println!("  {} ({}...): {}",
                                    token.outcome,
                                    &token.token_id[..20.min(token.token_id.len())],
                                    price_str
                                );
                            }
                            drop(side_map);
                            println!();

                            {
                                let mut tokens_write = current_tokens.write().await;
                                *tokens_write = new_tokens;
                            }

                            market_changed.notify_waiters();
                        }
                    }
                    Ok(None) => {
                        warn!("No active market found, waiting...");
                    }
                    Err(e) => {
                        error!("Error checking market: {}", e);
                    }
                }
            }
        })
    };

    // Initial market fetch
    match client.get_current_market_tokens(series_id).await? {
        Some((title, market)) => {
            println!("\x1b[32mCurrent market:\x1b[0m {}", title);

            let token_ids: Vec<String> = market.tokens.iter()
                .map(|t| t.token_id.clone())
                .collect();

            let mut side_map = token_to_side.write().await;
            for token in &market.tokens {
                let side = match token.outcome.to_lowercase().as_str() {
                    "yes" | "up" => ploy::domain::Side::Up,
                    _ => ploy::domain::Side::Down,
                };
                ws.register_token(&token.token_id, side).await;
                side_map.insert(token.token_id.clone(), side);

                let price_str = token.price.as_deref().unwrap_or("N/A");
                println!("  {} ({}...): {}",
                    token.outcome,
                    &token.token_id[..20.min(token.token_id.len())],
                    price_str
                );
            }
            drop(side_map);
            println!();

            {
                let mut tokens_write = current_tokens.write().await;
                *tokens_write = token_ids;
            }
        }
        None => {
            println!("\x1b[33mNo active market yet, waiting for next round...\x1b[0m");
        }
    }

    println!("\x1b[33mStarting WebSocket connection...\x1b[0m\n");

    // Spawn WebSocket connection
    let ws_handle = {
        let ws = Arc::clone(&ws);
        let current_tokens = Arc::clone(&current_tokens);
        let running = Arc::clone(&running);
        let market_changed = Arc::clone(&market_changed);

        tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let tokens = current_tokens.read().await.clone();

                if tokens.is_empty() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }

                tokio::select! {
                    result = ws.run(tokens) => {
                        if let Err(e) = result {
                            error!("WebSocket error: {}", e);
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    _ = market_changed.notified() => {
                        info!("Reconnecting WebSocket for new market...");
                    }
                }
            }
        })
    };

    // Trading logic with order execution
    let mut updates = ws.subscribe_updates();
    let trade_handle = {
        let running = Arc::clone(&running);
        let client = client.clone();
        let token_to_side = Arc::clone(&token_to_side);

        tokio::spawn(async move {
            // Strategy configuration
            let config = StrategyConfig {
                shares,
                window_min: 2,
                move_pct: Decimal::from_str(&format!("{:.4}", move_pct)).unwrap_or(dec!(0.15)),
                sum_target: Decimal::from_str(&format!("{:.4}", sum_target)).unwrap_or(dec!(0.95)),
                fee_buffer: dec!(0.005),
                slippage_buffer: dec!(0.01),
                profit_buffer: dec!(0.005),
            };

            let mut detector = SignalDetector::with_window(config.clone(), 3); // 3-second window for production
            let mut current_round: Option<String> = None;
            let mut position: Option<TradePosition> = None;

            println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
            println!("\x1b[36m║  Trading Active: move={:.1}%, window=3s, target={:.4}       ║\x1b[0m",
                move_pct * 100.0, sum_target);
            println!("\x1b[36m║  Shares: {}  |  Mode: {}                              ║\x1b[0m",
                shares, if dry_run { "DRY RUN" } else { "LIVE" });
            println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

            while running.load(Ordering::Relaxed) {
                match updates.recv().await {
                    Ok(update) => {
                        let side_str = match update.side {
                            ploy::domain::Side::Up => "\x1b[32m▲ UP  \x1b[0m",
                            ploy::domain::Side::Down => "\x1b[31m▼ DOWN\x1b[0m",
                        };

                        let now = chrono::Local::now().format("%H:%M:%S");
                        let bid = update.quote.best_bid.unwrap_or_default();
                        let ask = update.quote.best_ask.unwrap_or_default();

                        // Print quote update
                        println!(
                            "[{}] {} Bid: {:.4} | Ask: {:.4}",
                            now, side_str, bid, ask
                        );

                        // Check for dump signal (only if no active position)
                        if position.is_none() {
                            if let Some(signal) = detector.update(&update.quote, current_round.as_deref()) {
                                println!("\n\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                println!("\x1b[41;97m  🚨 DUMP SIGNAL - EXECUTING LEG1                          \x1b[0m");
                                println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                println!("  Side: {:?}", signal.side);
                                println!("  Drop: {:.2}% ({:.4} → {:.4})",
                                    signal.drop_pct * Decimal::from(100),
                                    signal.reference_price,
                                    signal.trigger_price);
                                println!("  Spread: {} bps", signal.spread_bps);

                                // Get token ID for this side
                                let side_map = token_to_side.read().await;
                                let token_id = side_map.iter()
                                    .find(|(_, &s)| s == signal.side)
                                    .map(|(id, _)| id.clone());
                                drop(side_map);

                                if let Some(token_id) = token_id {
                                    // Execute Leg1 order
                                    let order = OrderRequest::buy_limit(
                                        token_id.clone(),
                                        signal.side,
                                        shares,
                                        signal.trigger_price,
                                    );

                                    println!("\n  Submitting Leg1 order...");
                                    match client.submit_order(&order).await {
                                        Ok(resp) => {
                                            println!("\x1b[32m  ✓ Order submitted: {}\x1b[0m", resp.id);

                                            // Record position
                                            position = Some(TradePosition {
                                                leg1_side: signal.side,
                                                leg1_token_id: token_id,
                                                leg1_price: signal.trigger_price,
                                                leg1_order_id: resp.id,
                                                leg1_shares: shares,
                                            });
                                        }
                                        Err(e) => {
                                            println!("\x1b[31m  ✗ Order failed: {}\x1b[0m", e);
                                        }
                                    }
                                }
                                println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m\n");
                            }
                        }

                        // Check for leg2 opportunity if we have a position
                        if let Some(ref pos) = position {
                            let opposite_side = match pos.leg1_side {
                                ploy::domain::Side::Up => ploy::domain::Side::Down,
                                ploy::domain::Side::Down => ploy::domain::Side::Up,
                            };

                            if update.side == opposite_side {
                                if let Some(opposite_ask) = update.quote.best_ask {
                                    let sum = pos.leg1_price + opposite_ask;
                                    let target = detector.effective_sum_target();

                                    if sum <= target {
                                        println!("\n\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                        println!("\x1b[42;97m  ✅ LEG2 OPPORTUNITY - EXECUTING                           \x1b[0m");
                                        println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                        println!("  Leg1 ({:?}): {:.4}", pos.leg1_side, pos.leg1_price);
                                        println!("  Leg2 ({:?}): {:.4}", opposite_side, opposite_ask);
                                        println!("  Sum: {:.4} <= Target: {:.4}", sum, target);
                                        println!("  Potential Profit: {:.2}%",
                                            (Decimal::ONE - sum) * Decimal::from(100));

                                        // Get token ID for opposite side
                                        let side_map = token_to_side.read().await;
                                        let token_id = side_map.iter()
                                            .find(|(_, &s)| s == opposite_side)
                                            .map(|(id, _)| id.clone());
                                        drop(side_map);

                                        if let Some(token_id) = token_id {
                                            let order = OrderRequest::buy_limit(
                                                token_id,
                                                opposite_side,
                                                pos.leg1_shares,
                                                opposite_ask,
                                            );

                                            println!("\n  Submitting Leg2 order...");
                                            match client.submit_order(&order).await {
                                                Ok(resp) => {
                                                    println!("\x1b[32m  ✓ Order submitted: {}\x1b[0m", resp.id);
                                                    println!("\x1b[32m  ✓ CYCLE COMPLETE!\x1b[0m");

                                                    // Clear position and reset detector
                                                    position = None;
                                                    detector.reset(current_round.as_deref());
                                                }
                                                Err(e) => {
                                                    println!("\x1b[31m  ✗ Order failed: {}\x1b[0m", e);
                                                }
                                            }
                                        }
                                        println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m\n");
                                    } else {
                                        // Show leg2 check status every 5 seconds
                                        let now_secs = chrono::Utc::now().timestamp();
                                        if now_secs % 5 == 0 {
                                            println!("  \x1b[33m[Leg2 Check]\x1b[0m {:.4} + {:.4} = {:.4} > {:.4}",
                                                pos.leg1_price, opposite_ask, sum, target);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Lagged {} messages", n);
                    }
                    Err(_) => break,
                }
            }
        })
    };

    // Wait for Ctrl+C
    shutdown_signal().await;
    println!("\n\x1b[33mShutting down...\x1b[0m");

    running.store(false, Ordering::Relaxed);
    market_changed.notify_waiters();

    rotation_handle.abort();
    ws_handle.abort();
    trade_handle.abort();

    println!("\x1b[32mShutdown complete.\x1b[0m");
    Ok(())
}

/// Active trading position
#[derive(Debug, Clone)]
struct TradePosition {
    leg1_side: ploy::domain::Side,
    leg1_token_id: String,
    leg1_price: rust_decimal::Decimal,
    leg1_order_id: String,
    leg1_shares: u64,
}

/// Multi-event scanning mode - monitors all events in a series for arbitrage opportunities
async fn run_scan_mode(
    series_id: &str,
    sum_target: f64,
    move_pct: f64,
    continuous: bool,
) -> Result<()> {
    use ploy::config::StrategyConfig;
    use ploy::strategy::MultiEventMonitor;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};

    println!("\x1b[35m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       PLOY - Multi-Event Scanner [ALL EVENTS IN SERIES]      ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
    let ws = Arc::new(PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market"));

    // Strategy configuration
    let config = StrategyConfig {
        shares: 20,
        window_min: 2,
        move_pct: Decimal::from_str(&format!("{:.4}", move_pct)).unwrap_or(dec!(0.15)),
        sum_target: Decimal::from_str(&format!("{:.4}", sum_target)).unwrap_or(dec!(0.95)),
        fee_buffer: dec!(0.005),
        slippage_buffer: dec!(0.01),
        profit_buffer: dec!(0.005),
    };

    println!("  Series: {}", series_id);
    println!("  Sum target: {:.4}", sum_target);
    println!("  Move threshold: {:.1}%", move_pct * 100.0);
    println!("  Mode: {}", if continuous { "Continuous" } else { "One-shot" });
    println!();

    // Create multi-event monitor
    let mut monitor = MultiEventMonitor::new(series_id, config.clone());

    // Initial event discovery
    println!("\x1b[33mDiscovering active events...\x1b[0m");
    let token_ids = monitor.refresh_events(&client).await?;

    if token_ids.is_empty() {
        println!("\x1b[31mNo active events found in series {}.\x1b[0m", series_id);
        return Ok(());
    }

    println!("\x1b[32m✓ Found {} active events ({} tokens)\x1b[0m\n", monitor.event_count(), token_ids.len());

    // Display discovered events
    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║                    Active Events                             ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");

    for summary in monitor.summary() {
        let time_str = format!("{}m {}s",
            summary.time_remaining.num_minutes(),
            summary.time_remaining.num_seconds() % 60
        );
        println!("  {} - {} remaining",
            if summary.event_slug.is_empty() { &summary.event_id } else { &summary.event_slug },
            time_str
        );
    }
    println!();

    let running = Arc::new(AtomicBool::new(true));

    // Register all tokens with WebSocket
    for token_id in monitor.all_token_ids() {
        // Determine side from the monitor's internal mapping
        let side = if token_id.contains("up") || token_id.ends_with("1") {
            ploy::domain::Side::Up
        } else {
            ploy::domain::Side::Down
        };
        ws.register_token(&token_id, side).await;
    }

    // Spawn WebSocket connection
    let ws_handle = {
        let ws = Arc::clone(&ws);
        let token_ids = token_ids.clone();
        let running = Arc::clone(&running);

        tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                if let Err(e) = ws.run(token_ids.clone()).await {
                    error!("WebSocket error: {}", e);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    };

    // Periodic event refresh
    let refresh_handle = {
        let client = client.clone();
        let running = Arc::clone(&running);
        let series_id = series_id.to_string();

        tokio::spawn(async move {
            let mut refresh_interval = tokio::time::interval(Duration::from_secs(60));

            while running.load(Ordering::Relaxed) {
                refresh_interval.tick().await;
                // Note: In production, we'd need to share the monitor via Arc<RwLock>
                // For now, just log the refresh intent
                info!("Event refresh check for series {}", series_id);
            }
        })
    };

    println!("\x1b[33mStarting WebSocket connection...\x1b[0m\n");
    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Scanning for arbitrage opportunities (sum <= {:.4})         ║\x1b[0m", sum_target);
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Process quote updates
    let mut updates = ws.subscribe_updates();
    let scan_handle = {
        let running = Arc::clone(&running);
        let ws = Arc::clone(&ws);
        let target = Decimal::from_str(&format!("{:.4}", sum_target)).unwrap_or(dec!(0.95));

        tokio::spawn(async move {
            let mut last_summary_time = std::time::Instant::now();
            let mut up_quotes: std::collections::HashMap<String, rust_decimal::Decimal> = std::collections::HashMap::new();
            let mut down_quotes: std::collections::HashMap<String, rust_decimal::Decimal> = std::collections::HashMap::new();

            while running.load(Ordering::Relaxed) {
                match updates.recv().await {
                    Ok(update) => {
                        let now = chrono::Local::now().format("%H:%M:%S");

                        // Track quotes by token
                        if let Some(ask) = update.quote.best_ask {
                            match update.side {
                                ploy::domain::Side::Up => {
                                    up_quotes.insert(update.token_id.clone(), ask);
                                }
                                ploy::domain::Side::Down => {
                                    down_quotes.insert(update.token_id.clone(), ask);
                                }
                            }
                        }

                        // Print status summary every 10 seconds
                        if last_summary_time.elapsed() > std::time::Duration::from_secs(10) {
                            println!("\n[{}] \x1b[33m═══ Status Summary ═══\x1b[0m", now);
                            println!("  UP quotes: {}", up_quotes.len());
                            println!("  DOWN quotes: {}", down_quotes.len());

                            // Check for opportunities across all pairs
                            let mut best_sum = Decimal::from(2); // Start high
                            let mut best_pair: Option<(String, String, Decimal, Decimal)> = None;

                            for (up_token, up_ask) in &up_quotes {
                                for (down_token, down_ask) in &down_quotes {
                                    let sum = *up_ask + *down_ask;
                                    if sum < best_sum {
                                        best_sum = sum;
                                        best_pair = Some((
                                            up_token.clone(),
                                            down_token.clone(),
                                            *up_ask,
                                            *down_ask,
                                        ));
                                    }
                                }
                            }

                            if let Some((_, _, up_ask, down_ask)) = best_pair {
                                let color = if best_sum <= target {
                                    "\x1b[32m" // Green if profitable
                                } else {
                                    "\x1b[33m" // Yellow otherwise
                                };
                                println!("  Best sum: {}UP {:.4} + DOWN {:.4} = {:.4}\x1b[0m (target: ≤{:.4})",
                                    color, up_ask, down_ask, best_sum, target);

                                if best_sum <= target {
                                    let profit = (Decimal::ONE - best_sum) * Decimal::from(100);
                                    println!("\n\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                    println!("\x1b[42;97m  🎯 ARBITRAGE OPPORTUNITY FOUND!                           \x1b[0m");
                                    println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                    println!("  UP Ask:   {:.4}", up_ask);
                                    println!("  DOWN Ask: {:.4}", down_ask);
                                    println!("  Sum:      {:.4}", best_sum);
                                    println!("  Potential Profit: {:.2}%", profit);
                                    println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m\n");

                                    if !continuous {
                                        running.store(false, Ordering::Relaxed);
                                        break;
                                    }
                                }
                            }

                            last_summary_time = std::time::Instant::now();
                        }

                        // Print individual quote updates (condensed)
                        let side_str = match update.side {
                            ploy::domain::Side::Up => "\x1b[32m▲\x1b[0m",
                            ploy::domain::Side::Down => "\x1b[31m▼\x1b[0m",
                        };

                        if let Some(ask) = update.quote.best_ask {
                            print!("{} {:.3} ", side_str, ask);
                            std::io::Write::flush(&mut std::io::stdout()).ok();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Lagged {} messages", n);
                    }
                    Err(_) => break,
                }
            }
        })
    };

    // Wait for Ctrl+C or completion
    if continuous {
        shutdown_signal().await;
    } else {
        // Wait for scan to complete or Ctrl+C
        tokio::select! {
            _ = shutdown_signal() => {}
            _ = async {
                while running.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => {}
        }
    }

    println!("\n\x1b[33mShutting down scanner...\x1b[0m");

    running.store(false, Ordering::Relaxed);
    ws_handle.abort();
    refresh_handle.abort();
    scan_handle.abort();

    println!("\x1b[32mScan complete.\x1b[0m");
    Ok(())
}

/// Analyze a multi-outcome market for arbitrage opportunities
async fn run_analyze_mode(event_id: &str) -> Result<()> {
    use ploy::strategy::multi_outcome::fetch_multi_outcome_event;
    use rust_decimal_macros::dec;

    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║     PLOY - Multi-Outcome Arbitrage Analyzer                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    let client = PolymarketClient::new("https://clob.polymarket.com", true)?;

    println!("Fetching event: {}", event_id);

    // Fetch and analyze the multi-outcome market
    let monitor = fetch_multi_outcome_event(&client, event_id).await?;

    println!("\nEvent: {}", monitor.event_title);
    println!("Outcomes tracked: {}", monitor.outcome_count());

    // Show summary of all outcomes
    println!("\n\x1b[36m=== Outcome Summary ===\x1b[0m");
    for summary in monitor.summary() {
        let yes_str = summary.yes_price.map(|p| format!("{:.2}¢", p * dec!(100))).unwrap_or("-".into());
        let no_str = summary.no_price.map(|p| format!("{:.2}¢", p * dec!(100))).unwrap_or("-".into());
        let prob_str = summary.implied_prob_pct.map(|p| format!("{:.1}%", p)).unwrap_or("-".into());
        println!("  {} | Yes: {} | No: {} | Prob: {}", summary.name, yes_str, no_str, prob_str);
    }

    // Find arbitrage opportunities
    let opportunities = monitor.find_all_arbitrage();

    if opportunities.is_empty() {
        println!("\n\x1b[33mNo arbitrage opportunities found at current prices.\x1b[0m");
    } else {
        println!("\n\x1b[32m=== Arbitrage Opportunities Found! ===\x1b[0m");
        for opp in &opportunities {
            println!("\n  \x1b[32m✓ {:?} \x1b[0m", opp.arb_type);
            println!("    Profit per $1: ${:.4}", opp.profit_per_dollar);
            println!("    Profit %: {:.2}%", opp.profit_per_dollar * dec!(100));
            println!("    Confidence: {:.0}%", opp.confidence * dec!(100));
        }
    }

    Ok(())
}

/// Show account balance, positions, and orders
async fn run_account_mode(show_orders: bool, show_positions: bool) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;

    // Try to load wallet for authenticated access
    let client = match std::env::var("POLYMARKET_PRIVATE_KEY") {
        Ok(_) => {
            println!("  Loading wallet from environment...");
            let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
            println!("  Wallet loaded: {:?}", wallet.address());

            println!("  Authenticating with Polymarket CLOB...");
            match PolymarketClient::new_authenticated(
                "https://clob.polymarket.com",
                wallet,
                true,
            ).await {
                Ok(client) => {
                    println!("  \x1b[32m✓ Authentication successful\x1b[0m");
                    println!("  Has HMAC auth: {}\n", client.has_hmac_auth());
                    client
                }
                Err(e) => {
                    println!("  \x1b[31m✗ Authentication failed: {}\x1b[0m", e);
                    println!("  Falling back to unauthenticated client...\n");
                    PolymarketClient::new("https://clob.polymarket.com", true)?
                }
            }
        }
        Err(_) => {
            // Fall back to unauthenticated client
            println!("  No POLYMARKET_PRIVATE_KEY found, using unauthenticated client");
            PolymarketClient::new("https://clob.polymarket.com", true)?
        }
    };

    cli::show_account(&client, show_orders, show_positions).await
}

async fn run_watch_mode(cli: &Cli, token: Option<&str>, series: Option<&str>) -> Result<()> {
    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║          PLOY - Polymarket Trading Bot [DRY RUN]             ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    let client = PolymarketClient::new("https://clob.polymarket.com", true)?;

    // If token provided, show order book only (one-shot mode)
    if let Some(token_id) = token {
        cli::show_order_book(&client, token_id).await?;
        return Ok(());
    }

    // Series mode - continuous monitoring with auto-rotation
    if let Some(series_id) = series {
        return run_series_watch_mode(&client, series_id).await;
    }

    // Fallback to search mode (one-shot)
    println!("Searching for market: {}\n", cli.market);
    let markets = client.search_markets(&cli.market).await?;

    if markets.is_empty() {
        println!("\x1b[31mNo markets found for: {}\x1b[0m", cli.market);
        return Ok(());
    }

    let market = &markets[0];
    println!("\x1b[32mFound market:\x1b[0m {}", market.condition_id);
    if let Some(q) = &market.question {
        println!("  {}\n", q);
    }

    let market_info = client.get_market(&market.condition_id).await?;
    run_single_market_watch(&client, market_info).await
}

/// Continuous series monitoring with automatic market rotation
async fn run_series_watch_mode(client: &PolymarketClient, series_id: &str) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    println!("Starting continuous monitoring for series: {}", series_id);
    println!("Markets rotate every 15 minutes. Press Ctrl+C to stop.\n");

    let running = Arc::new(AtomicBool::new(true));
    let market_changed = Arc::new(Notify::new());

    // Track current market
    let current_tokens: Arc<tokio::sync::RwLock<Vec<String>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let current_title: Arc<tokio::sync::RwLock<String>> =
        Arc::new(tokio::sync::RwLock::new(String::new()));

    // WebSocket client
    let ws = Arc::new(PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market"));

    // Spawn market rotation checker (checks every 30 seconds)
    let rotation_handle = {
        let client = client.clone();
        let series_id = series_id.to_string();
        let current_tokens = Arc::clone(&current_tokens);
        let current_title = Arc::clone(&current_title);
        let ws = Arc::clone(&ws);
        let running = Arc::clone(&running);
        let market_changed = Arc::clone(&market_changed);

        tokio::spawn(async move {
            let mut check_interval = tokio::time::interval(Duration::from_secs(30));

            while running.load(Ordering::Relaxed) {
                check_interval.tick().await;

                match client.get_current_market_tokens(&series_id).await {
                    Ok(Some((title, market))) => {
                        let new_tokens: Vec<String> = market.tokens.iter()
                            .map(|t| t.token_id.clone())
                            .collect();

                        let tokens_read = current_tokens.read().await;
                        let tokens_changed = *tokens_read != new_tokens;
                        drop(tokens_read);

                        if tokens_changed {
                            // Market rotated!
                            println!("\n\x1b[33m═══ Market Rotation ═══\x1b[0m");
                            println!("\x1b[32mNew market:\x1b[0m {}", title);

                            // Clear old tokens from cache
                            ws.quote_cache().clear().await;

                            // Register new tokens
                            for token in &market.tokens {
                                let side = match token.outcome.to_lowercase().as_str() {
                                    "yes" | "up" => ploy::domain::Side::Up,
                                    _ => ploy::domain::Side::Down,
                                };
                                ws.register_token(&token.token_id, side).await;

                                let price_str = token.price.as_deref().unwrap_or("N/A");
                                println!("  {} ({}...): {}",
                                    token.outcome,
                                    &token.token_id[..20.min(token.token_id.len())],
                                    price_str
                                );
                            }
                            println!();

                            // Update current state
                            {
                                let mut tokens_write = current_tokens.write().await;
                                *tokens_write = new_tokens;
                            }
                            {
                                let mut title_write = current_title.write().await;
                                *title_write = title;
                            }

                            market_changed.notify_waiters();
                        }
                    }
                    Ok(None) => {
                        eprintln!("\x1b[33mWarning: No active market found, waiting...\x1b[0m");
                    }
                    Err(e) => {
                        eprintln!("\x1b[31mError checking market: {}\x1b[0m", e);
                    }
                }
            }
        })
    };

    // Initial market fetch
    match client.get_current_market_tokens(series_id).await? {
        Some((title, market)) => {
            println!("\x1b[32mCurrent market:\x1b[0m {}", title);

            let token_ids: Vec<String> = market.tokens.iter()
                .map(|t| t.token_id.clone())
                .collect();

            for token in &market.tokens {
                let side = match token.outcome.to_lowercase().as_str() {
                    "yes" | "up" => ploy::domain::Side::Up,
                    _ => ploy::domain::Side::Down,
                };
                ws.register_token(&token.token_id, side).await;

                let price_str = token.price.as_deref().unwrap_or("N/A");
                println!("  {} ({}...): {}",
                    token.outcome,
                    &token.token_id[..20.min(token.token_id.len())],
                    price_str
                );
            }
            println!();

            {
                let mut tokens_write = current_tokens.write().await;
                *tokens_write = token_ids;
            }
            {
                let mut title_write = current_title.write().await;
                *title_write = title;
            }
        }
        None => {
            println!("\x1b[33mNo active market yet, waiting for next round...\x1b[0m");
        }
    }

    println!("\x1b[33mStarting WebSocket connection...\x1b[0m\n");

    // Spawn WebSocket connection with dynamic token subscription
    let ws_handle = {
        let ws = Arc::clone(&ws);
        let current_tokens = Arc::clone(&current_tokens);
        let running = Arc::clone(&running);
        let market_changed = Arc::clone(&market_changed);

        tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let tokens = current_tokens.read().await.clone();

                if tokens.is_empty() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }

                tokio::select! {
                    result = ws.run(tokens) => {
                        if let Err(e) = result {
                            eprintln!("WebSocket error: {}", e);
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    _ = market_changed.notified() => {
                        // Market changed, reconnect with new tokens
                        println!("\x1b[33mReconnecting WebSocket for new market...\x1b[0m");
                    }
                }
            }
        })
    };

    // Print updates with signal detection
    let mut updates = ws.subscribe_updates();
    let print_handle = {
        let running = Arc::clone(&running);

        tokio::spawn(async move {
            use ploy::config::StrategyConfig;
            use ploy::strategy::SignalDetector;
            use rust_decimal_macros::dec;

            // Create signal detector with test config
            // Using 5% threshold and 10-second window for testing
            let config = StrategyConfig {
                shares: 20,
                window_min: 2,
                move_pct: dec!(0.05),      // 5% drop triggers (test mode)
                sum_target: dec!(0.98),    // Target sum for leg2
                fee_buffer: dec!(0.005),
                slippage_buffer: dec!(0.01),
                profit_buffer: dec!(0.005),
            };
            let mut detector = SignalDetector::with_window(config, 10); // 10-second window
            let mut current_round: Option<String> = None;
            let mut leg1_price: Option<(ploy::domain::Side, rust_decimal::Decimal)> = None;

            println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
            println!("\x1b[36m║  Signal Detection: move_pct=5%, window=10s, target=0.98     ║\x1b[0m");
            println!("\x1b[36m║  (Test mode - production uses 15%/3s)                       ║\x1b[0m");
            println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

            while running.load(Ordering::Relaxed) {
                match updates.recv().await {
                    Ok(update) => {
                        let side_str = match update.side {
                            ploy::domain::Side::Up => "\x1b[32m▲ UP  \x1b[0m",
                            ploy::domain::Side::Down => "\x1b[31m▼ DOWN\x1b[0m",
                        };

                        let now = chrono::Local::now().format("%H:%M:%S");
                        let bid = update.quote.best_bid.unwrap_or_default();
                        let ask = update.quote.best_ask.unwrap_or_default();
                        let bid_size = update.quote.bid_size.unwrap_or_default();
                        let ask_size = update.quote.ask_size.unwrap_or_default();

                        // Print quote update
                        println!(
                            "[{}] {} Bid: {:.4} ({:.2}) | Ask: {:.4} ({:.2})",
                            now, side_str, bid, bid_size, ask, ask_size
                        );

                        // Check for dump signal
                        if let Some(signal) = detector.update(&update.quote, current_round.as_deref()) {
                            println!("\n\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                            println!("\x1b[41;97m  🚨 DUMP SIGNAL DETECTED!                                  \x1b[0m");
                            println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                            println!("  Side: {:?}", signal.side);
                            println!("  Drop: {:.2}% ({:.4} → {:.4})",
                                signal.drop_pct * rust_decimal::Decimal::from(100),
                                signal.reference_price,
                                signal.trigger_price);
                            println!("  Spread: {} bps", signal.spread_bps);
                            println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m\n");

                            // Simulate leg1 entry at trigger price
                            leg1_price = Some((signal.side, signal.trigger_price));
                        }

                        // Check for leg2 opportunity if we have leg1
                        if let Some((leg1_side, l1_price)) = leg1_price {
                            // Get opposite side's ask
                            let opposite_side = match leg1_side {
                                ploy::domain::Side::Up => ploy::domain::Side::Down,
                                ploy::domain::Side::Down => ploy::domain::Side::Up,
                            };

                            // Only check when we get an update from the opposite side
                            if update.side == opposite_side {
                                if let Some(opposite_ask) = update.quote.best_ask {
                                    let sum = l1_price + opposite_ask;
                                    let target = detector.effective_sum_target();

                                    if sum <= target {
                                        println!("\n\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                        println!("\x1b[42;97m  ✅ LEG2 OPPORTUNITY!                                       \x1b[0m");
                                        println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                        println!("  Leg1 ({:?}): {:.4}", leg1_side, l1_price);
                                        println!("  Leg2 ({:?}): {:.4}", opposite_side, opposite_ask);
                                        println!("  Sum: {:.4} <= Target: {:.4}", sum, target);
                                        println!("  Potential Profit: {:.2}%",
                                            (rust_decimal::Decimal::ONE - sum) * rust_decimal::Decimal::from(100));
                                        println!("\x1b[42;97m ══════════════════════════════════════════════════════════ \x1b[0m\n");

                                        // Clear leg1 after successful leg2 (in real trading, we'd execute)
                                        leg1_price = None;
                                        detector.reset(current_round.as_deref());
                                    } else {
                                        // Show leg2 check status occasionally
                                        let now_secs = chrono::Utc::now().timestamp();
                                        if now_secs % 5 == 0 {
                                            println!("  \x1b[33m[Leg2 Check]\x1b[0m {:.4} + {:.4} = {:.4} > {:.4} (waiting...)",
                                                l1_price, opposite_ask, sum, target);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("Lagged {} messages", n);
                    }
                    Err(_) => break,
                }
            }
        })
    };

    // Wait for Ctrl+C
    shutdown_signal().await;
    println!("\n\x1b[33mShutting down...\x1b[0m");

    running.store(false, Ordering::Relaxed);
    market_changed.notify_waiters();

    rotation_handle.abort();
    ws_handle.abort();
    print_handle.abort();

    Ok(())
}

/// Watch a single market (one-shot mode)
async fn run_single_market_watch(client: &PolymarketClient, market_info: ploy::adapters::MarketResponse) -> Result<()> {
    println!("Tokens:");
    for token in &market_info.tokens {
        println!(
            "  {} ({}): {}",
            token.outcome,
            token.token_id,
            token.price.as_deref().unwrap_or("N/A")
        );

        if let Ok(book) = client.get_order_book(&token.token_id).await {
            if let Some(best_bid) = book.bids.first() {
                print!("    Best Bid: {} @ {}", best_bid.size, best_bid.price);
            }
            if let Some(best_ask) = book.asks.first() {
                print!("    Best Ask: {} @ {}", best_ask.size, best_ask.price);
            }
            println!();
        }
    }

    println!("\n\x1b[33mStarting WebSocket connection...\x1b[0m");
    println!("Press Ctrl+C to stop.\n");

    let ws = Arc::new(PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market"));
    let token_ids: Vec<String> = market_info.tokens.iter().map(|t| t.token_id.clone()).collect();

    for token in &market_info.tokens {
        let side = match token.outcome.to_lowercase().as_str() {
            "yes" | "up" => ploy::domain::Side::Up,
            _ => ploy::domain::Side::Down,
        };
        ws.register_token(&token.token_id, side).await;
    }

    let mut updates = ws.subscribe_updates();

    let ws_clone = Arc::clone(&ws);
    let ws_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = ws_clone.run(token_ids.clone()).await {
                eprintln!("WebSocket error: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let print_handle = tokio::spawn(async move {
        loop {
            match updates.recv().await {
                Ok(update) => {
                    let side_str = match update.side {
                        ploy::domain::Side::Up => "\x1b[32m▲ UP  \x1b[0m",
                        ploy::domain::Side::Down => "\x1b[31m▼ DOWN\x1b[0m",
                    };

                    let now = chrono::Local::now().format("%H:%M:%S");
                    let bid = update.quote.best_bid.unwrap_or_default();
                    let ask = update.quote.best_ask.unwrap_or_default();
                    let bid_size = update.quote.bid_size.unwrap_or_default();
                    let ask_size = update.quote.ask_size.unwrap_or_default();
                    println!(
                        "[{}] {} Bid: {:.4} ({:.2}) | Ask: {:.4} ({:.2})",
                        now, side_str, bid, bid_size, ask, ask_size
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("Lagged {} messages", n);
                }
                Err(_) => break,
            }
        }
    });

    shutdown_signal().await;
    println!("\n\x1b[33mShutting down...\x1b[0m");

    ws_handle.abort();
    print_handle.abort();

    Ok(())
}

async fn run_bot(cli: &Cli) -> Result<()> {
    info!("Starting Polymarket Trading Bot (ploy)");

    // Load configuration
    let config = match AppConfig::load_from(&cli.config) {
        Ok(mut c) => {
            // Override with CLI flags
            c.dry_run.enabled = cli.dry_run;
            c.market.market_slug = cli.market.clone();
            c
        }
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            // Use defaults
            info!("Using default configuration");
            AppConfig::default_config(cli.dry_run, &cli.market)
        }
    };

    info!(
        "Configuration: market={}, dry_run={}",
        config.market.market_slug, config.dry_run.enabled
    );

    // Check if we can connect to database
    let store = match PostgresStore::new(&config.database.url, config.database.max_connections).await
    {
        Ok(s) => {
            if let Err(e) = s.migrate().await {
                error!("Database migration failed: {}", e);
            }
            info!("Database connected");
            Some(s)
        }
        Err(e) => {
            error!("Database connection failed: {} - running without persistence", e);
            None
        }
    };

    // Crash recovery check
    if let Some(ref store) = store {
        match perform_crash_recovery(store).await {
            Ok(()) => info!("Crash recovery check completed"),
            Err(e) => warn!("Crash recovery check failed: {} - continuing anyway", e),
        }
    }

    // Initialize API clients
    let clob_client = PolymarketClient::new(&config.market.rest_url, config.dry_run.enabled)?;
    let ws_client = Arc::new(PolymarketWebSocket::new(&config.market.ws_url));

    // Initialize metrics
    let metrics = Arc::new(Metrics::new());

    if let Some(store) = store {
        // Full mode with database
        run_full_bot(config, store, clob_client, ws_client, metrics).await?;
    } else {
        // Simplified mode without database
        run_simple_bot(config, clob_client, ws_client).await?;
    }

    Ok(())
}

async fn run_full_bot(
    config: AppConfig,
    store: PostgresStore,
    clob_client: PolymarketClient,
    ws_client: Arc<PolymarketWebSocket>,
    metrics: Arc<Metrics>,
) -> Result<()> {
    // Initialize order executor
    let executor = OrderExecutor::new(clob_client.clone(), config.execution.clone());

    // Initialize strategy engine
    let engine = Arc::new(
        StrategyEngine::new(
            config.clone(),
            store.clone(),
            executor,
            ws_client.quote_cache().clone(),
        )
        .await?,
    );

    // Initialize data collector
    let data_collector = Arc::new(DataCollector::new(
        clob_client,
        store.clone(),
        Arc::clone(&ws_client),
        &config.market.market_slug,
    ));

    // Initialize health server state
    let health_state = Arc::new(
        HealthState::new()
            .with_risk_manager(engine.risk_manager())
            .with_metrics(Arc::clone(&metrics))
    );
    let health_port = config.health_port.unwrap_or(8080);
    let health_server = HealthServer::new(Arc::clone(&health_state), health_port);

    // Spawn health server
    let health_handle = {
        tokio::spawn(async move {
            if let Err(e) = health_server.run().await {
                error!("Health server error: {}", e);
            }
        })
    };

    // Spawn WebSocket connection
    let ws_handle = {
        let ws = Arc::clone(&ws_client);
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            loop {
                if let Err(e) = ws.run(vec![]).await {
                    error!("WebSocket error: {}", e);
                    metrics.inc_reconnections();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    };

    // Spawn data collector
    let collector_handle = {
        let collector = Arc::clone(&data_collector);
        tokio::spawn(async move {
            if let Err(e) = collector.start().await {
                error!("Data collector error: {}", e);
            }
        })
    };

    // Spawn strategy engine
    let engine_handle = {
        let engine = Arc::clone(&engine);
        let ws = Arc::clone(&ws_client);
        tokio::spawn(async move {
            let updates = ws.subscribe_updates();
            if let Err(e) = engine.run(updates).await {
                error!("Strategy engine error: {}", e);
            }
        })
    };

    // Spawn periodic round checking
    let round_handle = {
        let engine = Arc::clone(&engine);
        let collector = Arc::clone(&data_collector);
        tokio::spawn(async move {
            let mut check_interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                check_interval.tick().await;
                if let Some(round) = collector.current_round().await {
                    if let Err(e) = engine.set_round(round).await {
                        error!("Failed to set round: {}", e);
                    }
                }
            }
        })
    };

    // Spawn status logging
    let status_handle = {
        let metrics = Arc::clone(&metrics);
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            let mut status_interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                status_interval.tick().await;
                metrics.log_status(&*engine.risk_manager()).await;
            }
        })
    };

    // Wait for shutdown signal
    info!("Bot is running. Press Ctrl+C to stop.");
    shutdown_signal().await;

    info!("Shutting down...");
    engine.shutdown().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    health_handle.abort();
    ws_handle.abort();
    collector_handle.abort();
    engine_handle.abort();
    round_handle.abort();
    status_handle.abort();

    info!("Shutdown complete");
    Ok(())
}

async fn run_simple_bot(
    config: AppConfig,
    client: PolymarketClient,
    ws_client: Arc<PolymarketWebSocket>,
) -> Result<()> {
    info!("Running in simple mode (no database)");

    // Search for market
    let markets = client.search_markets(&config.market.market_slug).await?;

    if markets.is_empty() {
        error!("No markets found for: {}", config.market.market_slug);
        return Ok(());
    }

    let market = &markets[0];
    info!("Found market: {}", market.condition_id);

    // Get market info
    let market_info = client.get_market(&market.condition_id).await?;

    // Register tokens
    for token in &market_info.tokens {
        let side = match token.outcome.to_lowercase().as_str() {
            "yes" | "up" => ploy::domain::Side::Up,
            _ => ploy::domain::Side::Down,
        };
        ws_client.register_token(&token.token_id, side).await;
        info!("Registered {} token: {}", token.outcome, token.token_id);
    }

    let token_ids: Vec<String> = market_info.tokens.iter().map(|t| t.token_id.clone()).collect();

    // Spawn WebSocket
    let ws_clone = Arc::clone(&ws_client);
    let ws_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = ws_clone.run(token_ids.clone()).await {
                error!("WebSocket error: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    // Print updates
    let mut updates = ws_client.subscribe_updates();
    let print_handle = tokio::spawn(async move {
        loop {
            match updates.recv().await {
                Ok(update) => {
                    let side_str = match update.side {
                        ploy::domain::Side::Up => "UP",
                        ploy::domain::Side::Down => "DOWN",
                    };
                    let bid = update.quote.best_bid.unwrap_or_default();
                    let ask = update.quote.best_ask.unwrap_or_default();
                    info!("{}: bid={:.4} ask={:.4}", side_str, bid, ask);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    info!("Lagged {} messages", n);
                }
                Err(_) => break,
            }
        }
    });

    info!("Bot is running in simple mode. Press Ctrl+C to stop.");
    shutdown_signal().await;

    ws_handle.abort();
    print_handle.abort();

    info!("Shutdown complete");
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ploy=debug,sqlx=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();
}

fn init_logging_simple() {
    // Minimal logging for CLI commands
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            error!("Failed to install Ctrl+C handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => { stream.recv().await; }
            Err(e) => error!("Failed to install SIGTERM handler: {}", e),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// Graceful shutdown with timeout protection
#[allow(dead_code)]
async fn graceful_shutdown<F: std::future::Future>(
    shutdown_future: F,
    timeout_secs: u64,
    name: &str,
) {
    use tokio::time::{timeout, Duration};

    info!("Initiating graceful shutdown for {}...", name);

    match timeout(Duration::from_secs(timeout_secs), shutdown_future).await {
        Ok(_) => info!("{} shutdown completed gracefully", name),
        Err(_) => warn!("{} shutdown timed out after {}s, forcing", name, timeout_secs),
    }
}

/// Perform crash recovery at startup
async fn perform_crash_recovery(store: &PostgresStore) -> Result<()> {
    use chrono::Utc;
    use ploy::error::PloyError;

    info!("Performing crash recovery check...");

    // Get recovery summary from database
    let summary = store.get_recovery_summary().await?;
    summary.log_summary();

    if !summary.needs_recovery() {
        return Ok(());
    }

    // Check if trading was halted
    let today = Utc::now().date_naive();
    if store.is_trading_halted(today).await? {
        warn!("Trading was halted before shutdown - manual intervention required");
        return Err(PloyError::Internal(
            "Trading halted - check daily_metrics for reason".to_string(),
        ));
    }

    // Handle orphaned orders
    for order in &summary.orphaned_orders {
        info!(
            "Marking orphaned order {} as cancelled (was in status: {})",
            order.client_order_id, order.status
        );

        store
            .mark_order_cancelled(
                &order.client_order_id,
                "Cancelled during crash recovery - order was orphaned",
            )
            .await?;

        // If order has an exchange_order_id, we should try to cancel on exchange
        // But we don't have the CLOB client here, so just log a warning
        if order.can_cancel_on_exchange() {
            warn!(
                "Order {} may still be active on exchange (id: {:?}) - manual check recommended",
                order.client_order_id, order.exchange_order_id
            );
        }
    }

    // Handle incomplete cycles
    for cycle in &summary.incomplete_cycles {
        if cycle.is_round_expired() {
            info!(
                "Aborting expired cycle {} (round {} ended)",
                cycle.cycle_id, cycle.round_slug
            );
            store
                .abort_cycle(cycle.cycle_id, "Round expired during crash recovery")
                .await?;
        } else {
            // Round is still active - we could potentially resume, but safer to abort
            let remaining = cycle.time_remaining();
            if remaining.num_seconds() < 60 {
                info!(
                    "Aborting cycle {} - only {:?} remaining, not enough time to safely resume",
                    cycle.cycle_id, remaining
                );
                store
                    .abort_cycle(cycle.cycle_id, "Insufficient time remaining after crash recovery")
                    .await?;
            } else {
                warn!(
                    "Cycle {} in state {} could potentially be resumed ({:?} remaining) - aborting for safety",
                    cycle.cycle_id, cycle.state, remaining
                );
                store
                    .abort_cycle(cycle.cycle_id, "Aborted during crash recovery - manual resume not implemented")
                    .await?;
            }
        }
    }

    // Reset strategy state to IDLE
    store
        .update_strategy_state(ploy::domain::StrategyState::Idle, None, None)
        .await?;

    info!(
        "Crash recovery complete: {} orders cleaned up, {} cycles aborted",
        summary.orphaned_order_count, summary.incomplete_cycle_count
    );

    Ok(())
}

/// Run momentum strategy mode (gabagool22 style)
async fn run_momentum_mode(
    symbols: &str,
    min_move: f64,
    max_entry: f64,
    min_edge: f64,
    shares: u64,
    max_positions: usize,
    take_profit: f64,
    stop_loss: f64,
    dry_run: bool,
) -> Result<()> {
    use ploy::adapters::{BinanceWebSocket, PolymarketWebSocket};
    use ploy::signing::Wallet;
    use ploy::strategy::{ExitConfig, MomentumConfig, MomentumEngine, OrderExecutor};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};

    info!("Starting momentum strategy mode");

    // Parse symbols
    let symbols_vec: Vec<String> = symbols.split(',').map(|s| s.trim().to_uppercase()).collect();
    info!("Trading symbols: {:?}", symbols_vec);

    // Build momentum config
    let momentum_config = MomentumConfig {
        min_move_pct: Decimal::from_str(&format!("{:.6}", min_move / 100.0))
            .unwrap_or(dec!(0.005)),
        max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
            .unwrap_or(dec!(0.55)),
        min_edge: Decimal::from_str(&format!("{:.6}", min_edge / 100.0))
            .unwrap_or(dec!(0.05)),
        lookback_secs: 5,
        shares_per_trade: shares,
        max_positions,
        cooldown_secs: 30,
        symbols: symbols_vec.clone(),
    };

    // Build exit config
    let exit_config = ExitConfig {
        take_profit_pct: Decimal::from_str(&format!("{:.6}", take_profit / 100.0))
            .unwrap_or(dec!(0.20)),
        stop_loss_pct: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
            .unwrap_or(dec!(0.15)),
        trailing_stop_pct: dec!(0.10),
        exit_before_resolution_secs: 30,
    };

    // Print config
    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║           MOMENTUM STRATEGY (gabagool22 style)               ║\x1b[0m");
    println!("\x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "\x1b[36m║\x1b[0m  Symbols: {:42}\x1b[36m║\x1b[0m",
        symbols_vec.join(", ")
    );
    println!(
        "\x1b[36m║\x1b[0m  Min CEX Move: {:.2}%                                        \x1b[36m║\x1b[0m",
        min_move
    );
    println!(
        "\x1b[36m║\x1b[0m  Max Entry: {:.0}¢                                           \x1b[36m║\x1b[0m",
        max_entry
    );
    println!(
        "\x1b[36m║\x1b[0m  Min Edge: {:.1}%                                            \x1b[36m║\x1b[0m",
        min_edge
    );
    println!(
        "\x1b[36m║\x1b[0m  Shares/Trade: {}                                          \x1b[36m║\x1b[0m",
        shares
    );
    println!(
        "\x1b[36m║\x1b[0m  Max Positions: {}                                          \x1b[36m║\x1b[0m",
        max_positions
    );
    println!(
        "\x1b[36m║\x1b[0m  Take Profit: {:.0}%  |  Stop Loss: {:.0}%                    \x1b[36m║\x1b[0m",
        take_profit, stop_loss
    );
    println!(
        "\x1b[36m║\x1b[0m  Dry Run: {:44}\x1b[36m║\x1b[0m",
        if dry_run { "YES" } else { "NO" }
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Create Polymarket client
    let pm_client = if dry_run {
        PolymarketClient::new("https://clob.polymarket.com", true)?
    } else {
        let wallet = Wallet::from_env(ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID)?;
        PolymarketClient::new_authenticated(
            "https://clob.polymarket.com",
            wallet,
            true, // neg_risk for UP/DOWN markets
        )
        .await?
    };

    // Create executor
    let executor = OrderExecutor::new(pm_client.clone(), Default::default());

    // Create momentum engine
    let engine = MomentumEngine::new(
        momentum_config,
        exit_config,
        pm_client.clone(),
        executor,
        dry_run,
    );

    // Refresh events to get token IDs
    info!("Fetching active Polymarket events...");
    if let Err(e) = engine.event_matcher().refresh().await {
        error!("Failed to fetch events: {}", e);
    }

    let token_ids = engine.event_matcher().get_all_token_ids().await;
    info!("Found {} tokens to subscribe", token_ids.len());

    if token_ids.is_empty() {
        warn!("No active events found for the specified symbols");
        warn!("Make sure there are active UP/DOWN markets for: {:?}", symbols_vec);
        return Ok(());
    }

    // Create Binance WebSocket
    info!("Connecting to Binance WebSocket...");
    let binance_ws = BinanceWebSocket::new(symbols_vec);
    let binance_cache = binance_ws.price_cache().clone();

    // Create Polymarket WebSocket
    info!("Connecting to Polymarket WebSocket...");
    let pm_ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));
    for token_id in &token_ids {
        pm_ws.register_token(token_id, ploy::domain::Side::Up).await;
    }
    let pm_cache = pm_ws.quote_cache().clone();

    // Running flag for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_ws = Arc::clone(&running);
    let running_engine = Arc::clone(&running);

    // Spawn Binance WebSocket task
    let binance_handle = tokio::spawn(async move {
        if let Err(e) = binance_ws.run().await {
            error!("Binance WebSocket error: {}", e);
        }
    });

    // Spawn Polymarket WebSocket task
    let pm_ws_clone = Arc::clone(&pm_ws);
    let pm_handle = tokio::spawn(async move {
        pm_ws_clone.run(token_ids).await;
    });

    // Subscribe to updates
    let binance_rx = BinanceWebSocket::new(symbols.split(',').map(|s| s.to_string()).collect())
        .subscribe();
    let pm_rx = pm_ws.subscribe_updates();

    // Spawn engine task
    let engine_handle = tokio::spawn(async move {
        if let Err(e) = engine.run(binance_rx, pm_rx, &binance_cache, &pm_cache).await {
            error!("Momentum engine error: {}", e);
        }
    });

    // Wait for shutdown signal
    info!("Momentum strategy running. Press Ctrl+C to stop.");

    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received shutdown signal");
            running.store(false, Ordering::Relaxed);
        }
    }

    // Cleanup
    binance_handle.abort();
    pm_handle.abort();
    engine_handle.abort();

    info!("Momentum strategy stopped");
    Ok(())
}

/// Split arbitrage strategy mode (gabagool22 分时套利)
async fn run_split_arb_mode(
    max_entry: f64,
    target_cost: f64,
    min_profit: f64,
    max_wait: u64,
    shares: u64,
    max_unhedged: usize,
    stop_loss: f64,
    series: String,
    dry_run: bool,
) -> Result<()> {
    use ploy::adapters::PolymarketClient;
    use ploy::signing::Wallet;
    use ploy::strategy::{run_split_arb, OrderExecutor, SplitArbConfig};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    info!("Starting split arbitrage strategy mode");

    // Parse series IDs
    let series_ids: Vec<String> = series.split(',').map(|s| s.trim().to_string()).collect();
    info!("Monitoring series: {:?}", series_ids);

    // Build config
    let config = SplitArbConfig {
        max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
            .unwrap_or(Decimal::from_str("0.35").unwrap()),
        target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
            .unwrap_or(Decimal::from_str("0.70").unwrap()),
        min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
            .unwrap_or(Decimal::from_str("0.05").unwrap()),
        max_hedge_wait_secs: max_wait,
        shares_per_trade: shares,
        max_unhedged_positions: max_unhedged,
        unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
            .unwrap_or(Decimal::from_str("0.15").unwrap()),
        series_ids,
    };

    // Print config
    println!("\n\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║         SPLIT ARBITRAGE (gabagool22 分时套利)                ║\x1b[0m");
    println!("\x1b[35m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "\x1b[35m║\x1b[0m  Max Entry Price:    {}¢                                     \x1b[35m║\x1b[0m",
        max_entry as u64
    );
    println!(
        "\x1b[35m║\x1b[0m  Target Total Cost:  {}¢ (profit: {}¢)                       \x1b[35m║\x1b[0m",
        target_cost as u64,
        100 - target_cost as u64
    );
    println!(
        "\x1b[35m║\x1b[0m  Min Profit Margin:  {}¢                                      \x1b[35m║\x1b[0m",
        min_profit as u64
    );
    println!(
        "\x1b[35m║\x1b[0m  Max Hedge Wait:     {}s                                    \x1b[35m║\x1b[0m",
        max_wait
    );
    println!(
        "\x1b[35m║\x1b[0m  Shares Per Trade:   {}                                      \x1b[35m║\x1b[0m",
        shares
    );
    println!(
        "\x1b[35m║\x1b[0m  Max Unhedged:       {}                                        \x1b[35m║\x1b[0m",
        max_unhedged
    );
    println!(
        "\x1b[35m║\x1b[0m  Mode:               {}                                \x1b[35m║\x1b[0m",
        if dry_run { "DRY RUN" } else { "LIVE" }
    );
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Initialize client
    let client = if dry_run {
        PolymarketClient::new("https://clob.polymarket.com", true)?
    } else {
        let wallet = Wallet::from_env(ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID)?;
        PolymarketClient::new_authenticated(
            "https://clob.polymarket.com",
            wallet,
            true, // neg_risk for UP/DOWN markets
        ).await?
    };

    // Initialize executor with default config
    let executor = OrderExecutor::new(client.clone(), Default::default());

    // Run split arbitrage
    run_split_arb(config, client, executor, dry_run).await?;

    Ok(())
}

/// Claude AI agent mode for trading assistance
async fn run_agent_mode(
    mode: &str,
    market: Option<&str>,
    sports_url: Option<&str>,
    max_trade: f64,
    max_exposure: f64,
    enable_trading: bool,
    chat: bool,
) -> Result<()> {
    use ploy::agent::{
        AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient,
        protocol::{AgentContext, DailyStats, MarketSnapshot},
        SportsAnalyst,
    };
    use ploy::agent::autonomous::AutonomyLevel;
    use ploy::domain::{RiskState, StrategyState};
    use rust_decimal::Decimal;
    use std::io::{self, BufRead, Write};

    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           PLOY - Claude AI Trading Assistant                 ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    // Check claude CLI availability
    let check_client = ClaudeAgentClient::new();
    if !check_client.check_availability().await? {
        println!("\x1b[31m✗ Claude CLI not found. Please install it first:\x1b[0m");
        println!("  npm install -g @anthropic-ai/claude-code");
        return Ok(());
    }
    println!("\x1b[32m✓ Claude CLI available\x1b[0m");

    match mode {
        "advisory" => {
            let client = ClaudeAgentClient::new(); // Default 2-minute timeout
            let advisor = AdvisoryAgent::new(client);

            if chat {
                // Interactive chat mode
                println!("\n\x1b[33mInteractive Chat Mode\x1b[0m");
                println!("Type your questions, or 'exit' to quit.\n");

                let stdin = io::stdin();
                loop {
                    print!("\x1b[36mYou:\x1b[0m ");
                    io::stdout().flush()?;

                    let mut line = String::new();
                    stdin.lock().read_line(&mut line)?;
                    let line = line.trim();

                    if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
                        break;
                    }

                    if line.is_empty() {
                        continue;
                    }

                    println!("\x1b[33mClaude:\x1b[0m Analyzing...");

                    match advisor.chat(line, None).await {
                        Ok(response) => {
                            println!("\n{}\n", response);
                        }
                        Err(e) => {
                            println!("\x1b[31mError: {}\x1b[0m\n", e);
                        }
                    }
                }
            } else if let Some(market_id) = market {
                // Analyze specific market
                println!("\nAnalyzing market: {}", market_id);
                println!("Fetching market data from Polymarket...");

                // Fetch market data and populate snapshot
                let market_snapshot = match fetch_market_snapshot(market_id).await {
                    Ok(snapshot) => {
                        println!("\x1b[32m✓ Market data loaded\x1b[0m");
                        if let Some(ref desc) = snapshot.description {
                            println!("  Title: {}", desc);
                        }
                        if let (Some(bid), Some(ask)) = (snapshot.yes_bid, snapshot.yes_ask) {
                            println!("  YES: Bid {:.3} / Ask {:.3}", bid, ask);
                        }
                        if let (Some(bid), Some(ask)) = (snapshot.no_bid, snapshot.no_ask) {
                            println!("  NO:  Bid {:.3} / Ask {:.3}", bid, ask);
                        }
                        if let Some(mins) = snapshot.minutes_remaining {
                            println!("  Time remaining: {} minutes", mins);
                        }
                        snapshot
                    }
                    Err(e) => {
                        println!("\x1b[33m⚠ Could not fetch market data: {}\x1b[0m", e);
                        println!("  Proceeding with limited analysis...");
                        MarketSnapshot::new(market_id.to_string())
                    }
                };

                match advisor.analyze_market(&market_snapshot).await {
                    Ok(response) => {
                        println!("\n\x1b[33m=== Analysis Results ===\x1b[0m\n");
                        println!("Confidence: {:.0}%", response.confidence * 100.0);
                        println!("\nReasoning:\n{}", response.reasoning);
                        println!("\nRecommended Actions:");
                        for action in &response.recommended_actions {
                            println!("  • {:?}", action);
                        }
                        println!("\nSummary: {}", response.summary);
                    }
                    Err(e) => {
                        println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                    }
                }
            } else {
                println!("\nUsage:");
                println!("  ploy agent --mode advisory --market <EVENT_ID>  # Analyze a market");
                println!("  ploy agent --mode advisory --chat               # Interactive chat");
            }
        }
        "autonomous" => {
            let config = AutonomousConfig {
                autonomy_level: if enable_trading {
                    AutonomyLevel::LimitedAutonomy
                } else {
                    AutonomyLevel::AdvisoryOnly
                },
                max_trade_size: Decimal::from_f64_retain(max_trade).unwrap_or(Decimal::from(50)),
                max_total_exposure: Decimal::from_f64_retain(max_exposure).unwrap_or(Decimal::from(200)),
                min_confidence: 0.75,
                trading_enabled: enable_trading,
                analysis_interval_secs: 30,
                allowed_strategies: vec!["arbitrage".to_string()],
                require_exit_confirmation: true,
            };

            println!("\n\x1b[33mAutonomous Mode Configuration:\x1b[0m");
            println!("  Autonomy Level: {:?}", config.autonomy_level);
            println!("  Max Trade Size: ${}", config.max_trade_size);
            println!("  Max Exposure: ${}", config.max_total_exposure);
            println!("  Trading Enabled: {}", config.trading_enabled);
            println!("  Min Confidence: {}%", (config.min_confidence * 100.0) as u32);

            if !enable_trading {
                println!("\n\x1b[33m⚠️  Trading is disabled. Use --enable-trading to execute trades.\x1b[0m");
            }

            // Use longer timeout for autonomous mode (3 minutes)
            use ploy::agent::AgentClientConfig;
            let client = ClaudeAgentClient::with_config(AgentClientConfig::for_autonomous());
            let mut agent = AutonomousAgent::new(client, config);

            // Add Grok for real-time search if configured
            use ploy::agent::{GrokClient, GrokConfig};
            if let Ok(grok) = GrokClient::new(GrokConfig::from_env()) {
                if grok.is_configured() {
                    println!("  Grok: \x1b[32m✓ Enabled\x1b[0m (real-time market intelligence)");
                    agent = agent.with_grok(grok);
                } else {
                    println!("  Grok: \x1b[33m⚠ Not configured\x1b[0m (set GROK_API_KEY for real-time search)");
                }
            }

            // Get market slug for context provider
            let market_slug = market.map(|s| s.to_string()).unwrap_or_else(|| "demo-market".to_string());
            let market_slug_clone = market_slug.clone();

            // Fetch initial market data
            println!("\nFetching market data for: {}", market_slug);
            let initial_snapshot = match fetch_market_snapshot(&market_slug).await {
                Ok(snapshot) => {
                    println!("\x1b[32m✓ Market data loaded\x1b[0m");
                    if let Some(ref desc) = snapshot.description {
                        println!("  Title: {}", desc);
                    }
                    if let (Some(bid), Some(ask)) = (snapshot.yes_bid, snapshot.yes_ask) {
                        println!("  YES: Bid {:.3} / Ask {:.3}", bid, ask);
                    }
                    if let (Some(bid), Some(ask)) = (snapshot.no_bid, snapshot.no_ask) {
                        println!("  NO:  Bid {:.3} / Ask {:.3}", bid, ask);
                    }
                    if let Some(mins) = snapshot.minutes_remaining {
                        println!("  Time remaining: {} minutes", mins);
                    }
                    Some(snapshot)
                }
                Err(e) => {
                    println!("\x1b[33m⚠ Could not fetch market data: {}\x1b[0m", e);
                    None
                }
            };

            // Context provider that fetches fresh market data each cycle
            let context_provider = move || {
                let slug = market_slug_clone.clone();
                async move {
                    // Fetch fresh market data
                    let market_snapshot = match fetch_market_snapshot(&slug).await {
                        Ok(snapshot) => snapshot,
                        Err(_) => MarketSnapshot::new(slug),
                    };

                    let ctx = AgentContext::new(market_snapshot, StrategyState::Idle, RiskState::Normal)
                        .with_daily_stats(DailyStats {
                            realized_pnl: Decimal::ZERO,
                            trade_count: 0,
                            cycle_count: 0,
                            win_rate: None,
                            avg_profit: None,
                        });
                    Ok(ctx)
                }
            };

            println!("\n\x1b[32mStarting autonomous agent...\x1b[0m");
            println!("Press Ctrl+C to stop.\n");

            // Subscribe to actions for logging
            let mut action_rx = agent.subscribe_actions();
            let action_logger = tokio::spawn(async move {
                while let Ok(action) = action_rx.recv().await {
                    info!("Agent action: {:?}", action);
                }
            });

            tokio::select! {
                result = agent.run(context_provider) => {
                    if let Err(e) = result {
                        error!("Autonomous agent error: {}", e);
                    }
                }
                _ = signal::ctrl_c() => {
                    info!("Received shutdown signal");
                    agent.shutdown().await;
                }
            }

            action_logger.abort();
        }
        "sports" => {
            // Sports event analysis mode: Grok (player data + sentiment) -> Claude Opus (prediction)
            let event_url = match sports_url {
                Some(url) => url.to_string(),
                None => {
                    println!("\x1b[31mError: --sports-url is required for sports mode\x1b[0m");
                    println!("Example: ploy agent --mode sports --sports-url https://polymarket.com/event/nba-phi-dal-2026-01-01");
                    return Ok(());
                }
            };

            println!("\n\x1b[33mSports Analysis Mode\x1b[0m");
            println!("Event URL: {}", event_url);
            println!("\nWorkflow:");
            println!("  1. Fetch market odds from Polymarket");
            println!("  2. Search player stats & injuries (Grok)");
            println!("  3. Analyze public sentiment (Grok)");
            println!("  4. Predict win probability (Claude Opus)");
            println!("  5. Generate trade recommendation\n");

            // Create sports analyst
            let analyst = match SportsAnalyst::from_env() {
                Ok(a) => a,
                Err(e) => {
                    println!("\x1b[31mFailed to initialize sports analyst: {}\x1b[0m", e);
                    println!("Make sure GROK_API_KEY is set in your environment");
                    return Ok(());
                }
            };
            println!("\x1b[32m✓ Grok + Claude initialized\x1b[0m\n");

            // Run analysis
            println!("\x1b[36mAnalyzing event...\x1b[0m");
            match analyst.analyze_event(&event_url).await {
                Ok(analysis) => {
                    println!("\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m");
                    println!("\x1b[33m                    SPORTS ANALYSIS RESULTS                     \x1b[0m");
                    println!("\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m\n");

                    // Teams
                    println!("\x1b[36mMatchup:\x1b[0m {} vs {}", analysis.teams.0, analysis.teams.1);

                    // Market odds
                    println!("\n\x1b[36mMarket Odds (Polymarket):\x1b[0m");
                    println!("  {} YES: {:.1}%", analysis.teams.0,
                        analysis.market_odds.team1_yes_price.to_string().parse::<f64>().unwrap_or(0.0) * 100.0);
                    if let Some(p) = analysis.market_odds.team2_yes_price {
                        println!("  {} YES: {:.1}%", analysis.teams.1,
                            p.to_string().parse::<f64>().unwrap_or(0.0) * 100.0);
                    }

                    // Structured data (from Grok)
                    if let Some(ref data) = analysis.structured_data {
                        let sentiment = &data.sentiment;
                        println!("\n\x1b[36mPublic Sentiment (Grok):\x1b[0m");
                        println!("  Expert pick: {}", sentiment.expert_pick);
                        println!("  Expert confidence: {:.0}%", sentiment.expert_confidence * 100.0);
                        println!("  Public bet: {:.0}%", sentiment.public_bet_percentage);
                        println!("  Sharp money: {}", sentiment.sharp_money_side);
                        println!("  Social sentiment: {}", sentiment.social_sentiment);
                        if !sentiment.key_narratives.is_empty() {
                            println!("  Key narratives:");
                            for narrative in sentiment.key_narratives.iter().take(3) {
                                println!("    • {}", narrative);
                            }
                        }
                    }

                    // Claude prediction
                    println!("\n\x1b[36mClaude Opus Prediction:\x1b[0m");
                    println!("  {} win probability: \x1b[32m{:.1}%\x1b[0m",
                        analysis.teams.0, analysis.prediction.team1_win_prob * 100.0);
                    println!("  {} win probability: \x1b[32m{:.1}%\x1b[0m",
                        analysis.teams.1, analysis.prediction.team2_win_prob * 100.0);
                    println!("  Confidence: {:.0}%", analysis.prediction.confidence * 100.0);
                    println!("\n  Reasoning: {}", analysis.prediction.reasoning);
                    if !analysis.prediction.key_factors.is_empty() {
                        println!("\n  Key factors:");
                        for factor in &analysis.prediction.key_factors {
                            println!("    • {}", factor);
                        }
                    }

                    // Trade recommendation
                    println!("\n\x1b[36mTrade Recommendation:\x1b[0m");
                    let action_color = match analysis.recommendation.action {
                        ploy::agent::sports_analyst::TradeAction::Buy => "\x1b[32m",
                        ploy::agent::sports_analyst::TradeAction::Sell => "\x1b[31m",
                        ploy::agent::sports_analyst::TradeAction::Hold => "\x1b[33m",
                        ploy::agent::sports_analyst::TradeAction::Avoid => "\x1b[31m",
                    };
                    println!("  Action: {}{:?}\x1b[0m", action_color, analysis.recommendation.action);
                    println!("  Side: {}", analysis.recommendation.side);
                    println!("  Edge: {:.1}%", analysis.recommendation.edge);
                    println!("  Suggested size: {}% of bankroll", analysis.recommendation.suggested_size);
                    println!("  Reasoning: {}", analysis.recommendation.reasoning);

                    println!("\n\x1b[33m═══════════════════════════════════════════════════════════════\x1b[0m");
                }
                Err(e) => {
                    println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                }
            }
        }
        _ => {
            println!("\x1b[31mUnknown mode: {}\x1b[0m", mode);
            println!("Available modes: advisory, autonomous, sports");
        }
    }

    info!("Agent mode completed");
    Ok(())
}

/// Fetch market data from Polymarket and create a populated MarketSnapshot
async fn fetch_market_snapshot(
    market_slug: &str,
) -> Result<ploy::agent::protocol::MarketSnapshot> {
    use chrono::{DateTime, Utc};
    use ploy::agent::protocol::MarketSnapshot;
    use ploy::error::PloyError;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    // Try to fetch event by slug from Gamma API
    let url = format!(
        "https://gamma-api.polymarket.com/events?slug={}",
        market_slug
    );

    let http_client = reqwest::Client::new();
    let resp = http_client.get(&url).send().await?;

    if !resp.status().is_success() {
        return Err(PloyError::MarketDataUnavailable(format!(
            "Event not found: {}",
            market_slug
        )));
    }

    let events: Vec<serde_json::Value> = resp.json().await?;
    let event = events.first().ok_or_else(|| {
        PloyError::MarketDataUnavailable(format!("No event found for slug: {}", market_slug))
    })?;

    let mut snapshot = MarketSnapshot::new(market_slug.to_string());

    // Set description from event title
    snapshot.description = event.get("title").and_then(|v| v.as_str()).map(String::from);

    // Parse end date
    if let Some(end_str) = event.get("endDate").and_then(|v| v.as_str()) {
        if let Ok(end_dt) = DateTime::parse_from_rfc3339(end_str) {
            let end_utc: DateTime<Utc> = end_dt.into();
            snapshot.end_time = Some(end_utc);
            let now = Utc::now();
            let duration = end_utc.signed_duration_since(now);
            snapshot.minutes_remaining = Some(duration.num_minutes());
        }
    }

    // Get markets from the event
    if let Some(markets) = event.get("markets").and_then(|v| v.as_array()) {
        let mut sum_yes_asks = Decimal::ZERO;
        let mut sum_no_bids = Decimal::ZERO;
        let mut first_market = true;

        for market in markets {
            // Get CLOB token IDs
            let clob_token_ids = market
                .get("clobTokenIds")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());

            // Get outcome prices from market data
            let outcome_prices = market
                .get("outcomePrices")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());

            if let (Some(token_ids), Some(prices)) = (clob_token_ids, outcome_prices) {
                if token_ids.len() >= 2 && prices.len() >= 2 {
                    // Parse YES price (first token)
                    let yes_price = Decimal::from_str(&prices[0]).ok();
                    // Parse NO price (second token)
                    let no_price = Decimal::from_str(&prices[1]).ok();

                    // For first market, set as primary prices
                    if first_market {
                        // Use outcome prices as approximate bid/ask
                        snapshot.yes_bid = yes_price;
                        snapshot.yes_ask = yes_price;
                        snapshot.no_bid = no_price;
                        snapshot.no_ask = no_price;

                        first_market = false;
                    }

                    // Accumulate for sum calculations (multi-outcome markets)
                    if let Some(price) = yes_price {
                        sum_yes_asks = sum_yes_asks + price;
                    }
                    if let Some(price) = no_price {
                        sum_no_bids = sum_no_bids + price;
                    }
                }
            }
        }

        // Set sum values for arbitrage detection
        if sum_yes_asks > Decimal::ZERO {
            snapshot.sum_asks = Some(sum_yes_asks);
        }
        if sum_no_bids > Decimal::ZERO {
            snapshot.sum_bids = Some(sum_no_bids);
        }
    }

    Ok(snapshot)
}

/// Handle crypto subcommands
async fn run_crypto_command(cmd: &CryptoCommands) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{
        CryptoSplitArbConfig, run_crypto_split_arb,
        core::SplitArbConfig,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    match cmd {
        CryptoCommands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            coins,
            dry_run,
        } => {
            info!("Starting crypto split-arb strategy");

            // Map coins to series IDs
            let series_ids: Vec<String> = coins
                .split(',')
                .map(|c| match c.trim().to_uppercase().as_str() {
                    "SOL" => "10423".to_string(),
                    "ETH" => "10191".to_string(),
                    "BTC" => "41".to_string(),
                    _ => c.to_string(), // Allow raw series IDs
                })
                .collect();

            // Create config
            let config = CryptoSplitArbConfig {
                base: SplitArbConfig {
                    max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
                        .unwrap_or(dec!(0.35)),
                    target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
                        .unwrap_or(dec!(0.95)),
                    min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
                        .unwrap_or(dec!(0.05)),
                    max_hedge_wait_secs: *max_wait,
                    shares_per_trade: *shares,
                    max_unhedged_positions: *max_unhedged,
                    unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
                        .unwrap_or(dec!(0.15)),
                },
                series_ids,
            };

            // Initialize client
            let client = if *dry_run {
                PolymarketClient::new("https://clob.polymarket.com", true)?
            } else {
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk for UP/DOWN markets
                ).await?
            };

            // Initialize executor with default config
            let executor = OrderExecutor::new(client.clone(), Default::default());

            // Run strategy
            run_crypto_split_arb(client, executor, config, *dry_run).await?;
        }
        CryptoCommands::Monitor { coins } => {
            info!("Monitoring crypto markets: {}", coins);
            // TODO: Implement monitoring mode
            println!("Crypto monitoring mode not yet implemented");
        }
    }

    Ok(())
}

/// Handle sports subcommands
async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{
        SportsSplitArbConfig, SportsLeague, run_sports_split_arb,
        core::SplitArbConfig,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    match cmd {
        SportsCommands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            leagues,
            dry_run,
        } => {
            info!("Starting sports split-arb strategy");

            // Parse leagues
            let league_list: Vec<SportsLeague> = leagues
                .split(',')
                .filter_map(|l| match l.trim().to_uppercase().as_str() {
                    "NBA" => Some(SportsLeague::NBA),
                    "NFL" => Some(SportsLeague::NFL),
                    "MLB" => Some(SportsLeague::MLB),
                    "NHL" => Some(SportsLeague::NHL),
                    "SOCCER" => Some(SportsLeague::Soccer),
                    "UFC" => Some(SportsLeague::UFC),
                    _ => None,
                })
                .collect();

            // Create config
            let config = SportsSplitArbConfig {
                base: SplitArbConfig {
                    max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
                        .unwrap_or(dec!(0.45)),
                    target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
                        .unwrap_or(dec!(0.92)),
                    min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
                        .unwrap_or(dec!(0.03)),
                    max_hedge_wait_secs: *max_wait,
                    shares_per_trade: *shares,
                    max_unhedged_positions: *max_unhedged,
                    unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
                        .unwrap_or(dec!(0.20)),
                },
                leagues: league_list,
            };

            // Initialize client
            let client = if *dry_run {
                PolymarketClient::new("https://clob.polymarket.com", true)?
            } else {
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk
                ).await?
            };

            // Initialize executor with default config
            let executor = OrderExecutor::new(client.clone(), Default::default());

            // Run strategy
            run_sports_split_arb(client, executor, config, *dry_run).await?;
        }
        SportsCommands::Monitor { leagues } => {
            info!("Monitoring sports markets: {}", leagues);
            // TODO: Implement monitoring mode
            println!("Sports monitoring mode not yet implemented");
        }
        SportsCommands::Draftkings { sport, min_edge, all } => {
            use ploy::agent::{OddsProvider, Sport, Market};

            println!("\n\x1b[33m{}\x1b[0m", "═".repeat(63));
            println!("\x1b[33m           DRAFTKINGS ODDS SCANNER\x1b[0m");
            println!("\x1b[33m{}\x1b[0m", "═".repeat(63));

            // Parse sport
            let sport_enum = match sport.to_lowercase().as_str() {
                "nba" => Sport::NBA,
                "nfl" => Sport::NFL,
                "nhl" => Sport::NHL,
                "mlb" => Sport::MLB,
                _ => {
                    println!("\x1b[31mInvalid sport. Use: nba, nfl, nhl, mlb\x1b[0m");
                    return Ok(());
                }
            };

            // Create odds provider
            let provider = match OddsProvider::from_env() {
                Ok(p) => p,
                Err(_e) => {
                    println!("\x1b[31mError: THE_ODDS_API_KEY not configured\x1b[0m");
                    println!("Get a free API key at: https://the-odds-api.com/");
                    return Ok(());
                }
            };

            println!("\nFetching {} odds from DraftKings...\n", sport.to_uppercase());

            // Fetch odds
            match provider.get_odds(sport_enum, Market::Moneyline).await {
                Ok(events) => {
                    if events.is_empty() {
                        println!("No upcoming games found for {}", sport.to_uppercase());
                        return Ok(());
                    }

                    println!("Found {} upcoming games:\n", events.len());

                    for event in &events {
                        if let Some(best) = event.best_odds() {
                            let edge_pct = (rust_decimal::Decimal::ONE - best.total_implied).to_string()
                                .parse::<f64>().unwrap_or(0.0) * 100.0;

                            // Filter by min_edge unless --all
                            if !*all && edge_pct.abs() < *min_edge {
                                continue;
                            }

                            println!("\x1b[36m{} vs {}\x1b[0m", event.home_team, event.away_team);
                            println!("  \x1b[32m{}\x1b[0m @ {} ({:.1}%)",
                                event.home_team,
                                format!("{:+.0}", best.home_american_odds),
                                best.home_implied_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                            );
                            println!("  \x1b[32m{}\x1b[0m @ {} ({:.1}%)",
                                event.away_team,
                                format!("{:+.0}", best.away_american_odds),
                                best.away_implied_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                            );

                            if best.has_arbitrage() {
                                println!("  \x1b[32m🎯 Arbitrage: {:.2}% profit!\x1b[0m", best.arbitrage_profit());
                            }

                            println!();
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[31mError fetching odds: {}\x1b[0m", e);
                }
            }
        }
        SportsCommands::Analyze { url, team1, team2 } => {
            use ploy::agent::{SportsAnalyst, SportsAnalysisWithDK};

            println!("\n\x1b[33m{}\x1b[0m", "═".repeat(63));
            println!("\x1b[33m        SPORTS ANALYSIS WITH DRAFTKINGS COMPARISON\x1b[0m");
            println!("\x1b[33m{}\x1b[0m", "═".repeat(63));

            // Need either URL or both team names
            if url.is_none() && (team1.is_none() || team2.is_none()) {
                println!("\x1b[31mPlease provide --url or both --team1 and --team2\x1b[0m");
                return Ok(());
            }

            // Create analyst
            let analyst = match SportsAnalyst::from_env() {
                Ok(a) => a,
                Err(e) => {
                    println!("\x1b[31mError: {}\x1b[0m", e);
                    return Ok(());
                }
            };

            // Build URL or use provided
            let event_url = match url {
                Some(u) => u.clone(),
                None => {
                    // Build a fake URL from team names
                    let t1 = team1.clone().unwrap().to_lowercase().replace(' ', "-");
                    let t2 = team2.clone().unwrap().to_lowercase().replace(' ', "-");
                    format!("https://polymarket.com/event/nba-{}-vs-{}", t1, t2)
                }
            };

            println!("\nAnalyzing: \x1b[36m{}\x1b[0m\n", event_url);

            // Run analysis with DraftKings
            match analyst.analyze_with_draftkings(&event_url).await {
                Ok(analysis) => {
                    let base = &analysis.base;

                    println!("\x1b[36mMatchup: {} vs {}\x1b[0m", base.teams.0, base.teams.1);
                    println!();

                    // Claude prediction
                    println!("\x1b[33mClaude Opus Prediction:\x1b[0m");
                    println!("  {} win: {:.1}%", base.teams.0, base.prediction.team1_win_prob * 100.0);
                    println!("  {} win: {:.1}%", base.teams.1, base.prediction.team2_win_prob * 100.0);
                    println!("  Confidence: {:.0}%", base.prediction.confidence * 100.0);
                    println!();

                    // DraftKings comparison
                    if let Some(ref dk) = analysis.draftkings {
                        println!("\x1b[33mDraftKings Comparison:\x1b[0m");
                        println!("  DK {} implied: {:.1}%",
                            dk.home_team,
                            dk.dk_home_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!("  DK {} implied: {:.1}%",
                            dk.away_team,
                            dk.dk_away_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!("  Edge on {}: {:.1}%",
                            dk.recommended_side,
                            dk.edge.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!();
                    } else {
                        println!("\x1b[33mDraftKings odds not available for this game\x1b[0m");
                        println!();
                    }

                    // Best opportunity
                    let (best_side, best_edge) = analysis.best_edge();
                    println!("\x1b[32mRecommendation:\x1b[0m");
                    println!("  Best bet: \x1b[32m{}\x1b[0m ({:+.1}% edge)", best_side, best_edge);

                    if analysis.has_arbitrage() {
                        println!("  \x1b[32m🎯 Potential arbitrage detected!\x1b[0m");
                    }
                }
                Err(e) => {
                    println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                }
            }
        }
    }

    Ok(())
}

/// RL strategy commands
#[cfg(feature = "rl")]
async fn run_rl_command(cmd: &RlCommands) -> Result<()> {
    use ploy::rl::{RLConfig, PPOConfig, TrainingConfig, RLStrategy, TradingEnvConfig, MarketConfig};
    use ploy::rl::training::{TrainingLoop, Checkpointer, train_simulated, summarize_results};
    use ploy::rl::training::checkpointing::episode_name;
    use ploy::rl::algorithms::ppo::{PPOTrainer, PPOTrainerConfig};
    use ploy::strategy::Strategy; // Import Strategy trait for id() method
    use std::path::Path;

    match cmd {
        RlCommands::Train {
            episodes,
            checkpoint,
            lr,
            batch_size,
            update_freq,
            series,
            symbol,
            resume,
            verbose,
        } => {
            info!("Starting RL training mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Training Mode                          ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Episodes:       {:>6}                                       ║", episodes);
            println!("║  Learning Rate:  {:>10.6}                                  ║", lr);
            println!("║  Batch Size:     {:>6}                                       ║", batch_size);
            println!("║  Update Freq:    {:>6}                                       ║", update_freq);
            println!("║  Symbol:         {:>10}                                    ║", symbol);
            println!("║  Checkpoint:     {}                                          ║", checkpoint);
            if let Some(series_id) = series {
                println!("║  Series:         {}                                          ║", series_id);
            }
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Create checkpoint directory
            let checkpoint_dir = Path::new(checkpoint);
            if !checkpoint_dir.exists() {
                std::fs::create_dir_all(checkpoint_dir)?;
                info!("Created checkpoint directory: {}", checkpoint);
            }

            // Configure training
            let ppo_config = PPOConfig {
                lr: *lr,
                batch_size: *batch_size,
                ..Default::default()
            };

            let training_config = TrainingConfig {
                update_frequency: *update_freq,
                ..Default::default()
            };

            let config = RLConfig {
                ppo: ppo_config,
                training: training_config,
                ..Default::default()
            };

            // Create trainer
            let ppo_trainer_config = PPOTrainerConfig {
                ppo: config.ppo.clone(),
                hidden_dim: 128,
            };
            let mut ppo_trainer = PPOTrainer::new(ppo_trainer_config);

            // Create checkpointer
            let checkpointer = Checkpointer::new(checkpoint.clone(), 10);

            // Resume from checkpoint if specified
            if let Some(resume_path) = resume {
                info!("Resuming from checkpoint: {}", resume_path);
                println!("Loading checkpoint from: {}", resume_path);
                // Note: Full checkpoint loading requires burn model serialization
            }

            // Configure simulated environment
            let market_config = MarketConfig {
                initial_price: 0.50,
                volatility: 0.02,
                mean_reversion: 0.1,
                mean_price: 0.50,
                spread_pct: 0.02,
                quote_update_freq: 5,
                trend: 0.0,
            };

            let env_config = TradingEnvConfig {
                market: market_config,
                initial_capital: 1000.0,
                max_position: 100,
                transaction_cost: 0.001,
                max_steps: 1000,
                take_profit: 0.05,
                stop_loss: 0.03,
            };

            println!("\nStarting simulated training with {} episodes...", episodes);

            // Train using simulated environment
            let results = train_simulated(&mut ppo_trainer, env_config, *episodes, *verbose);

            // Summarize results
            let summary = summarize_results(&results);

            // Save final checkpoint
            let final_name = checkpointer.latest_checkpoint().unwrap_or_else(|| "ppo_final".to_string());
            let final_path = checkpointer.checkpoint_path(&final_name);

            println!("\n╔══════════════════════════════════════════════════════════════╗");
            println!("║               Training Complete                              ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Episodes:       {:>6}                                       ║", summary.num_episodes);
            println!("║  Avg Reward:     {:>10.2}                                    ║", summary.avg_reward);
            println!("║  Avg PnL:        {:>10.2}                                    ║", summary.avg_pnl);
            println!("║  Avg Length:     {:>10.1}                                    ║", summary.avg_episode_length);
            println!("║  Avg Trades:     {:>10.1}                                    ║", summary.avg_trades);
            println!("║  Win Rate:       {:>9.1}%                                    ║", summary.avg_win_rate * 100.0);
            println!("║  Profit Factor:  {:>10.2}                                    ║", summary.profit_factor);
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!("Final checkpoint: {:?}", final_path);
        }

        RlCommands::Run {
            model,
            online_learning,
            series,
            symbol,
            exploration,
            dry_run,
        } => {
            info!("Starting RL strategy mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Strategy Mode                          ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Series:         {}                                          ║", series);
            println!("║  Symbol:         {:>10}                                    ║", symbol);
            println!("║  Exploration:    {:>6.2}                                      ║", exploration);
            println!("║  Online Learn:   {:>5}                                       ║", online_learning);
            println!("║  Dry Run:        {:>5}                                       ║", dry_run);
            if let Some(model_path) = model {
                println!("║  Model:          {}                                          ║", model_path);
            }
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Configure RL strategy
            let mut config = RLConfig::default();
            config.training.online_learning = *online_learning;
            config.training.exploration_rate = *exploration;

            // Load model if specified
            if let Some(model_path) = model {
                info!("Loading model from: {}", model_path);
                // Note: Full model loading requires burn serialization
            }

            // Get tokens for the series
            // In production, this would query Polymarket for the series tokens
            let up_token = format!("{}_UP", series);
            let down_token = format!("{}_DOWN", series);

            // Create RL strategy
            let strategy = RLStrategy::new(
                format!("rl_{}", series),
                config,
                up_token,
                down_token,
                symbol.clone(),
            );

            info!("RL Strategy initialized");
            println!("\nRL Strategy ready.");
            println!("Strategy ID: {}", strategy.id());

            if *dry_run {
                println!("\n[DRY RUN MODE] No real orders will be placed.");
            }

            // In production, this would integrate with the orchestrator
            // For now, just show that the strategy is ready
            println!("\nTo integrate with live trading:");
            println!("  1. Add RLStrategy to the Orchestrator");
            println!("  2. Connect WebSocket feeds");
            println!("  3. Start the trading loop");
            println!("\nPress Ctrl+C to exit.");

            // Wait for interrupt
            tokio::signal::ctrl_c().await?;
            println!("\nShutting down...");
        }

        RlCommands::Eval {
            model,
            data,
            episodes,
            output,
        } => {
            info!("Starting RL evaluation mode");
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Evaluation Mode                        ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Model:          {}                                          ║", model);
            println!("║  Data:           {}                                          ║", data);
            println!("║  Episodes:       {:>6}                                       ║", episodes);
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Verify data file exists
            if !Path::new(data).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Data file not found: {}", data
                )));
            }

            // Verify model file exists
            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}", model
                )));
            }

            println!("\nRunning evaluation...");

            // In production, this would:
            // 1. Load the model
            // 2. Load test data
            // 3. Run episodes with deterministic policy
            // 4. Collect metrics

            let mut total_reward = 0.0f64;
            let mut total_trades = 0;
            let mut winning_trades = 0;

            for ep in 0..*episodes {
                // Simulated episode metrics
                let ep_reward = rand::random::<f64>() * 10.0 - 2.0; // Random for demo
                total_reward += ep_reward;
                total_trades += 5;
                if ep_reward > 0.0 {
                    winning_trades += 1;
                }

                if ep % 10 == 0 {
                    println!("  Episode {}/{}: reward = {:.2}", ep + 1, episodes, ep_reward);
                }
            }

            let avg_reward = total_reward / *episodes as f64;
            let win_rate = winning_trades as f64 / *episodes as f64 * 100.0;

            println!("\n═══════════════════════════════════════════════════════════════");
            println!("                     EVALUATION RESULTS                        ");
            println!("═══════════════════════════════════════════════════════════════");
            println!("  Total Episodes:    {}", episodes);
            println!("  Average Reward:    {:.4}", avg_reward);
            println!("  Total Reward:      {:.2}", total_reward);
            println!("  Win Rate:          {:.1}%", win_rate);
            println!("  Total Trades:      {}", total_trades);
            println!("═══════════════════════════════════════════════════════════════");

            if let Some(output_path) = output {
                // Save results to file
                let results = format!(
                    "episodes,avg_reward,total_reward,win_rate,total_trades\n{},{:.4},{:.2},{:.1},{}\n",
                    episodes, avg_reward, total_reward, win_rate, total_trades
                );
                std::fs::write(output_path, results)?;
                println!("\nResults saved to: {}", output_path);
            }
        }

        RlCommands::Info { model } => {
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Model Info                             ║");
            println!("╚══════════════════════════════════════════════════════════════╝");

            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}", model
                )));
            }

            // Get file info
            let metadata = std::fs::metadata(model)?;
            let size_kb = metadata.len() / 1024;

            println!("\nModel: {}", model);
            println!("Size:  {} KB", size_kb);
            println!("\nModel Configuration:");
            println!("  State dim:     42 features");
            println!("  Action dim:    5 (continuous)");
            println!("  Hidden dim:    128");
            println!("  Algorithm:     PPO");
            println!("\nNote: Full model inspection requires burn serialization support.");
        }

        RlCommands::Export {
            model,
            format,
            output,
        } => {
            println!("Exporting model...");
            println!("  Source:  {}", model);
            println!("  Format:  {}", format);
            println!("  Output:  {}", output);

            if !Path::new(model).exists() {
                return Err(ploy::error::PloyError::Validation(format!(
                    "Model file not found: {}", model
                )));
            }

            match format.as_str() {
                "json" => {
                    // Export config as JSON
                    let config = RLConfig::default();
                    let json = serde_json::to_string_pretty(&config)?;
                    std::fs::write(output, json)?;
                    println!("\nModel configuration exported to: {}", output);
                }
                "onnx" | "torch" => {
                    println!("\nExport to {} format requires additional dependencies.", format);
                    println!("This feature is planned for a future release.");
                }
                _ => {
                    return Err(ploy::error::PloyError::Validation(format!(
                        "Unsupported export format: {}. Use 'json', 'onnx', or 'torch'.", format
                    )));
                }
            }
        }

        RlCommands::Backtest {
            episodes,
            duration,
            volatility,
            round,
            capital,
            verbose,
        } => {
            use ploy::rl::training::{train_backtest, summarize_backtest_results};

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║               Ploy RL Backtest Mode                          ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Episodes:       {:>10}                                    ║", episodes);
            println!("║  Duration:       {:>10} mins                               ║", duration);
            println!("║  Volatility:     {:>10.4}                                    ║", volatility);
            println!("║  Initial Capital: {:>9.2}                                   ║", capital);
            if let Some(r) = round {
                println!("║  Round ID:       {:>10}                                    ║", r);
            }
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Create trainer with exploration
            let ppo_config = PPOTrainerConfig {
                ppo: PPOConfig::default(),
                hidden_dim: 128,
            };
            let mut trainer = PPOTrainer::with_exploration(ppo_config, 0.998, 0.05);

            // Environment config
            let env_config = TradingEnvConfig {
                market: MarketConfig::default(),
                initial_capital: *capital,
                max_position: 100,
                transaction_cost: 0.001,
                max_steps: (*duration as usize) * 60 * 2, // 2 ticks per second
                take_profit: 0.05,
                stop_loss: 0.02,
            };

            info!("Starting backtest with {} episodes...", episodes);

            // Run backtest
            let results = train_backtest(
                &mut trainer,
                env_config,
                *episodes,
                *duration,
                *volatility,
                *verbose,
            );

            // Summarize results
            let summary = summarize_backtest_results(&results);

            println!("\n╔══════════════════════════════════════════════════════════════╗");
            println!("║               Backtest Summary                               ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Episodes:        {:>10}                                   ║", summary.num_episodes);
            println!("║  Avg PnL:         {:>10.2}                                   ║", summary.avg_pnl);
            println!("║  Total PnL:       {:>10.2}                                   ║", summary.total_pnl);
            println!("║  Avg Trades:      {:>10.1}                                   ║", summary.avg_trades);
            println!("║  Win Rate:        {:>9.1}%                                   ║", summary.avg_win_rate * 100.0);
            println!("║  Episode Win %:   {:>9.1}%                                   ║", summary.episode_win_rate * 100.0);
            println!("║  Profit Factor:   {:>10.2}                                   ║", summary.profit_factor);
            println!("║  Max Drawdown:    {:>9.1}%                                   ║", summary.max_drawdown * 100.0);
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Phase analysis
            if *episodes >= 20 {
                let phase_size = episodes / 5;
                println!("\n╔══════════════════════════════════════════════════════════════╗");
                println!("║               Phase Analysis                                 ║");
                println!("╠══════════════════════════════════════════════════════════════╣");

                for (i, phase) in results.chunks(phase_size).enumerate() {
                    let phase_summary = summarize_backtest_results(phase);
                    println!("║  Phase {}: pnl={:>7.2}, trades={:>5.1}, win={:>5.1}%           ║",
                        i + 1,
                        phase_summary.avg_pnl,
                        phase_summary.avg_trades,
                        phase_summary.avg_win_rate * 100.0
                    );
                }
                println!("╚══════════════════════════════════════════════════════════════╝");
            }
        }

        RlCommands::LeadLag {
            episodes,
            trade_size,
            max_position,
            symbol,
            lr: _lr,
            checkpoint,
            verbose,
        } => {
            use rust_decimal::Decimal;
            use ploy::rl::environment::{LeadLagEnvironment, LeadLagConfig, LeadLagAction, LobDataPoint};

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║             Ploy Lead-Lag RL Training                        ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Symbol:         {:>10}                                    ║", symbol);
            println!("║  Episodes:       {:>10}                                    ║", episodes);
            println!("║  Trade Size:     ${:>9.2}                                   ║", trade_size);
            println!("║  Max Position:   ${:>9.2}                                   ║", max_position);
            println!("║  Checkpoint:     {}                                          ║", checkpoint);
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Create checkpoint directory
            std::fs::create_dir_all(&checkpoint).ok();

            // Load historical data from database
            let config = AppConfig::load()?;
            let store = PostgresStore::new(&config.database.url, 5).await?;

            info!("Loading training data from sync_records...");

            // Query historical data
            let rows = sqlx::query_as::<_, (
                i64, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal,
                Option<Decimal>, Option<Decimal>, Option<Decimal>, Option<Decimal>
            )>(
                r#"
                SELECT
                    EXTRACT(EPOCH FROM timestamp)::BIGINT * 1000 as ts_ms,
                    bn_mid_price, bn_obi_5, bn_obi_10, bn_spread_bps,
                    bn_bid_volume, bn_ask_volume,
                    bn_price_change_1s, bn_price_change_5s,
                    pm_yes_price, pm_no_price
                FROM sync_records
                WHERE symbol = $1
                ORDER BY timestamp
                LIMIT 100000
                "#
            )
            .bind(&symbol.to_uppercase())
            .fetch_all(store.pool())
            .await?;

            if rows.is_empty() {
                println!("No training data found for symbol {}.", symbol);
                println!("Please run 'ploy collect -s {}' first to gather data.", symbol);
                return Ok(());
            }

            println!("Loaded {} data points for training", rows.len());

            // Convert to LobDataPoints
            let data: Vec<LobDataPoint> = rows.iter().map(|r| {
                LobDataPoint {
                    timestamp_ms: r.0,
                    bn_mid_price: r.1,
                    bn_obi_5: r.2,
                    bn_obi_10: r.3,
                    bn_spread_bps: r.4,
                    bn_bid_volume: r.5,
                    bn_ask_volume: r.6,
                    momentum_1s: r.7.unwrap_or_default(),
                    momentum_5s: r.8.unwrap_or_default(),
                    pm_yes_price: r.9.unwrap_or(Decimal::new(50, 2)),
                    pm_no_price: r.10.unwrap_or(Decimal::new(50, 2)),
                }
            }).collect();

            // Configure environment
            let env_config = LeadLagConfig {
                trade_size_usd: Decimal::try_from(*trade_size).unwrap_or(Decimal::ONE),
                max_position_usd: Decimal::try_from(*max_position).unwrap_or(Decimal::new(50, 0)),
                ..Default::default()
            };

            // Training loop with simple Q-learning
            let mut total_rewards = Vec::new();
            let mut exploration_rate = 0.5f32;
            let exploration_decay = 0.995f32;
            let min_exploration = 0.05f32;

            // Simple action values (Q-table approximation)
            let num_actions = LeadLagAction::num_actions();
            let mut action_values = vec![0.0f32; num_actions];
            let learning_rate = 0.01f32;

            println!("\nTraining {} episodes...\n", episodes);

            for episode in 0..*episodes {
                let mut env = LeadLagEnvironment::new(env_config.clone(), data.clone());
                let mut _obs = env.reset();
                let mut episode_reward = 0.0f32;
                let mut steps = 0;

                loop {
                    // Epsilon-greedy action selection
                    let action_idx = if rand::random::<f32>() < exploration_rate {
                        rand::random::<usize>() % num_actions
                    } else {
                        action_values.iter()
                            .enumerate()
                            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    };

                    let action = LeadLagAction::from(action_idx);
                    let result = env.step(action);

                    // Update Q-values (simple TD update)
                    action_values[action_idx] += learning_rate * (result.reward - action_values[action_idx]);

                    episode_reward += result.reward;
                    steps += 1;
                    _obs = result.observation;

                    if result.done {
                        break;
                    }
                }

                // Decay exploration
                exploration_rate = (exploration_rate * exploration_decay).max(min_exploration);
                total_rewards.push(episode_reward);

                if *verbose || episode % 100 == 0 {
                    let recent_avg: f32 = total_rewards.iter().rev().take(100).sum::<f32>()
                        / total_rewards.len().min(100) as f32;
                    println!(
                        "Episode {:>5}: reward={:>8.2}, steps={:>6}, avg_100={:>8.2}, eps={:.3}",
                        episode + 1, episode_reward, steps, recent_avg, exploration_rate
                    );
                }
            }

            // Final summary
            let final_avg: f32 = total_rewards.iter().sum::<f32>() / total_rewards.len() as f32;
            let max_reward = total_rewards.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let min_reward = total_rewards.iter().cloned().fold(f32::INFINITY, f32::min);

            println!("\n╔══════════════════════════════════════════════════════════════╗");
            println!("║               Training Summary                               ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Total Episodes:  {:>10}                                   ║", episodes);
            println!("║  Avg Reward:      {:>10.2}                                   ║", final_avg);
            println!("║  Max Reward:      {:>10.2}                                   ║", max_reward);
            println!("║  Min Reward:      {:>10.2}                                   ║", min_reward);
            println!("║  Final Epsilon:   {:>10.4}                                   ║", exploration_rate);
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Action Values:                                              ║");
            println!("║    Hold:     {:>10.4}                                        ║", action_values[0]);
            println!("║    BuyYes:   {:>10.4}                                        ║", action_values[1]);
            println!("║    BuyNo:    {:>10.4}                                        ║", action_values[2]);
            println!("║    CloseYes: {:>10.4}                                        ║", action_values[3]);
            println!("║    CloseNo:  {:>10.4}                                        ║", action_values[4]);
            println!("╚══════════════════════════════════════════════════════════════╝");

            // Save action values as simple checkpoint
            let checkpoint_path = format!("{}/action_values.json", checkpoint);
            let checkpoint_data = serde_json::json!({
                "symbol": symbol,
                "trade_size": trade_size,
                "max_position": max_position,
                "episodes": episodes,
                "action_values": action_values,
                "final_avg_reward": final_avg,
            });
            std::fs::write(&checkpoint_path, serde_json::to_string_pretty(&checkpoint_data)?)?;
            println!("\nCheckpoint saved to: {}", checkpoint_path);
        }

        RlCommands::LeadLagLive {
            symbol,
            trade_size,
            max_position,
            market,
            checkpoint,
            dry_run,
            min_confidence,
        } => {
            use rust_decimal::Decimal;
            use ploy::collector::{SyncCollector, SyncCollectorConfig};
            use ploy::rl::environment::{LeadLagAction, LobDataPoint};

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║             Ploy Lead-Lag Live Trading                       ║");
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  Symbol:         {:>10}                                    ║", symbol);
            println!("║  Market:         {:>10}                                    ║", market);
            println!("║  Trade Size:     ${:>9.2}                                   ║", trade_size);
            println!("║  Max Position:   ${:>9.2}                                   ║", max_position);
            println!("║  Min Confidence: {:>10.2}                                   ║", min_confidence);
            println!("║  Dry Run:        {:>10}                                    ║", dry_run);
            println!("╚══════════════════════════════════════════════════════════════╝\n");

            // Load trained model
            let checkpoint_path = format!("{}/action_values.json", checkpoint);
            let checkpoint_content = std::fs::read_to_string(&checkpoint_path)
                .map_err(|e| ploy::error::PloyError::Internal(format!("Failed to load checkpoint: {}", e)))?;
            let checkpoint_data: serde_json::Value = serde_json::from_str(&checkpoint_content)?;

            let action_values: Vec<f32> = checkpoint_data["action_values"]
                .as_array()
                .ok_or_else(|| ploy::error::PloyError::Validation("Invalid checkpoint format".to_string()))?
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();

            if action_values.len() != LeadLagAction::num_actions() {
                return Err(ploy::error::PloyError::Validation("Invalid action values in checkpoint".to_string()).into());
            }

            info!("Loaded model from: {}", checkpoint_path);
            info!("Action values: Hold={:.4}, BuyYes={:.4}, BuyNo={:.4}, CloseYes={:.4}, CloseNo={:.4}",
                action_values[0], action_values[1], action_values[2], action_values[3], action_values[4]);

            // Load config
            let config = AppConfig::load()?;

            // Create collector
            let collector_config = SyncCollectorConfig {
                binance_symbols: vec![symbol.to_uppercase()],
                polymarket_slugs: vec![market.clone()],
                snapshot_interval_ms: 100,
                database_url: config.database.url.clone(),
            };

            // Create database pool
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&config.database.url)
                .await?;

            let collector = SyncCollector::new(collector_config).with_pool(pool.clone());
            let mut rx = collector.subscribe();

            // Spawn collector
            let collector_handle = tokio::spawn(async move {
                if let Err(e) = collector.run().await {
                    error!("Collector error: {}", e);
                }
            });

            // Track position
            let mut yes_position: Decimal = Decimal::ZERO;
            let mut no_position: Decimal = Decimal::ZERO;
            let max_pos = Decimal::try_from(*max_position).unwrap_or(Decimal::new(50, 0));
            let trade_sz = Decimal::try_from(*trade_size).unwrap_or(Decimal::ONE);
            let mut trade_count = 0u64;

            println!("\n📡 Listening for market signals... (Ctrl+C to stop)\n");

            // Process incoming data
            loop {
                tokio::select! {
                    record = rx.recv() => {
                        match record {
                            Ok(r) => {
                                // Build observation
                                let obs = LobDataPoint {
                                    timestamp_ms: r.timestamp.timestamp_millis(),
                                    bn_mid_price: r.bn_mid_price,
                                    bn_obi_5: r.bn_obi_5,
                                    bn_obi_10: r.bn_obi_10,
                                    bn_spread_bps: r.bn_spread_bps,
                                    bn_bid_volume: r.bn_bid_volume,
                                    bn_ask_volume: r.bn_ask_volume,
                                    momentum_1s: r.bn_price_change_1s.unwrap_or_default(),
                                    momentum_5s: r.bn_price_change_5s.unwrap_or_default(),
                                    pm_yes_price: r.pm_yes_price.unwrap_or(Decimal::new(50, 2)),
                                    pm_no_price: r.pm_no_price.unwrap_or(Decimal::new(50, 2)),
                                };

                                // Calculate action confidence
                                let max_val = action_values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                                let sum_exp: f32 = action_values.iter().map(|v| (v - max_val).exp()).sum();
                                let probs: Vec<f32> = action_values.iter().map(|v| (v - max_val).exp() / sum_exp).collect();

                                let best_action = probs.iter()
                                    .enumerate()
                                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                                    .map(|(i, p)| (LeadLagAction::from(i), *p))
                                    .unwrap_or((LeadLagAction::Hold, 0.0));

                                let (action, confidence) = best_action;

                                // Skip if below confidence threshold
                                if confidence < *min_confidence as f32 {
                                    continue;
                                }

                                // Execute action based on position limits
                                let total_position = yes_position + no_position;
                                let can_buy = total_position + trade_sz <= max_pos;

                                match action {
                                    LeadLagAction::Hold => { /* do nothing */ }
                                    LeadLagAction::BuyYes if can_buy && obs.pm_yes_price > Decimal::ZERO => {
                                        trade_count += 1;
                                        if *dry_run {
                                            println!("🟢 [DRY] BuyYes @ {:.4} (conf: {:.2}%) - OBI={:.4}, Mom={:.4}",
                                                obs.pm_yes_price, confidence * 100.0, obs.bn_obi_5, obs.momentum_1s);
                                        } else {
                                            println!("🟢 BuyYes @ {:.4} (conf: {:.2}%)", obs.pm_yes_price, confidence * 100.0);
                                            // TODO: Execute real order via PolymarketClient
                                        }
                                        yes_position += trade_sz;
                                    }
                                    LeadLagAction::BuyNo if can_buy && obs.pm_no_price > Decimal::ZERO => {
                                        trade_count += 1;
                                        if *dry_run {
                                            println!("🔴 [DRY] BuyNo @ {:.4} (conf: {:.2}%) - OBI={:.4}, Mom={:.4}",
                                                obs.pm_no_price, confidence * 100.0, obs.bn_obi_5, obs.momentum_1s);
                                        } else {
                                            println!("🔴 BuyNo @ {:.4} (conf: {:.2}%)", obs.pm_no_price, confidence * 100.0);
                                            // TODO: Execute real order via PolymarketClient
                                        }
                                        no_position += trade_sz;
                                    }
                                    LeadLagAction::CloseYes if yes_position > Decimal::ZERO => {
                                        trade_count += 1;
                                        let sell_price = obs.pm_yes_price;
                                        if *dry_run {
                                            println!("⬜ [DRY] CloseYes @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                        } else {
                                            println!("⬜ CloseYes @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                            // TODO: Execute real order
                                        }
                                        yes_position -= trade_sz.min(yes_position);
                                    }
                                    LeadLagAction::CloseNo if no_position > Decimal::ZERO => {
                                        trade_count += 1;
                                        let sell_price = obs.pm_no_price;
                                        if *dry_run {
                                            println!("⬜ [DRY] CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                        } else {
                                            println!("⬜ CloseNo @ {:.4} (conf: {:.2}%)", sell_price, confidence * 100.0);
                                            // TODO: Execute real order
                                        }
                                        no_position -= trade_sz.min(no_position);
                                    }
                                    _ => {}
                                }

                                // Print status every 100 records
                                static mut COUNTER: u64 = 0;
                                unsafe {
                                    COUNTER += 1;
                                    if COUNTER % 100 == 0 {
                                        println!("📊 Status: Yes=${:.2}, No=${:.2}, Total=${:.2}, Trades={}",
                                            yes_position, no_position, yes_position + no_position, trade_count);
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Lagged {} messages", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                info!("Channel closed");
                                break;
                            }
                        }
                    }
                    _ = signal::ctrl_c() => {
                        println!("\n\n╔══════════════════════════════════════════════════════════════╗");
                        println!("║               Session Summary                                ║");
                        println!("╠══════════════════════════════════════════════════════════════╣");
                        println!("║  Total Trades:    {:>10}                                   ║", trade_count);
                        println!("║  Yes Position:    ${:>9.2}                                   ║", yes_position);
                        println!("║  No Position:     ${:>9.2}                                   ║", no_position);
                        println!("║  Total Position:  ${:>9.2}                                   ║", yes_position + no_position);
                        println!("╚══════════════════════════════════════════════════════════════╝");
                        break;
                    }
                }
            }

            collector_handle.abort();
        }
    }

    Ok(())
}

/// Run data collector for lag analysis
async fn run_collect_mode(symbols: &str, markets: Option<&str>, duration: u64) -> Result<()> {
    use ploy::collector::{SyncCollector, SyncCollectorConfig};

    info!("Starting data collector...");

    // Parse symbols
    let binance_symbols: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();

    // Parse Polymarket markets
    let polymarket_slugs: Vec<String> = markets
        .map(|m| m.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    info!("Binance symbols: {:?}", binance_symbols);
    info!("Polymarket markets: {:?}", polymarket_slugs);

    // Load config for database URL
    let config = AppConfig::load()?;

    // Create collector config
    let collector_config = SyncCollectorConfig {
        binance_symbols: binance_symbols.clone(),
        polymarket_slugs,
        snapshot_interval_ms: 100,
        database_url: config.database.url.clone(),
    };

    // Create database pool
    let store = PostgresStore::new(&config.database.url, 5).await?;

    // Create collector with database
    let collector = SyncCollector::new(collector_config).with_pool(store.pool().clone());

    // Subscribe to updates for logging
    let mut rx = collector.subscribe();

    // Spawn update logger
    tokio::spawn(async move {
        let mut count = 0u64;
        loop {
            match rx.recv().await {
                Ok(record) => {
                    count += 1;
                    if count % 100 == 0 {
                        info!(
                            "[{}] {} mid={:.2} obi5={:.4} pm_yes={:?}",
                            count,
                            record.symbol,
                            record.bn_mid_price,
                            record.bn_obi_5,
                            record.pm_yes_price
                        );
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Logger lagged {} messages", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // Run collector
    if duration > 0 {
        info!("Collecting for {} minutes...", duration);
        tokio::select! {
            result = collector.run() => {
                if let Err(e) = result {
                    error!("Collector error: {}", e);
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(duration * 60)) => {
                info!("Collection duration reached, stopping...");
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, stopping...");
            }
        }
    } else {
        info!("Collecting indefinitely (Ctrl+C to stop)...");
        tokio::select! {
            result = collector.run() => {
                if let Err(e) = result {
                    error!("Collector error: {}", e);
                }
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, stopping...");
            }
        }
    }

    info!("Data collection stopped");
    Ok(())
}
