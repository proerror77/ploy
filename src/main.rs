use chrono::Utc;
use clap::Parser;
#[cfg(feature = "api")]
use ploy::adapters::{start_api_server, start_api_server_background};
use ploy::adapters::{PolymarketClient, PolymarketWebSocket, PostgresStore};
// Use legacy CLI module for backward compatibility
#[cfg(feature = "api")]
use ploy::api::state::StrategyConfigState;
#[cfg(feature = "rl")]
use ploy::cli::legacy::RlCommands;
use ploy::cli::legacy::{
    self as cli, Cli, Commands, CryptoCommands, PoliticsCommands, SportsCommands,
};
use ploy::config::AppConfig;
use ploy::error::{PloyError, Result};
use ploy::safety::legacy_live::legacy_live_allowed;
use ploy::services::{DataCollector, HealthServer, HealthState, Metrics};
use ploy::strategy::{IdempotencyManager, OrderExecutor, StrategyEngine};
use std::sync::Arc;
use tokio::signal;
use tokio::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

mod main_agent_mode;
mod main_commands;
mod main_modes;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Serve { port }) => {
            init_logging_simple();

            #[cfg(feature = "api")]
            {
                use rust_decimal::prelude::ToPrimitive;

                let config = AppConfig::load_from(&cli.config).unwrap_or_else(|e| {
                    warn!("Failed to load config: {}, using defaults", e);
                    AppConfig::default_config(true, "btc-price-series-15m")
                });

                let api_port = (*port)
                    .or_else(|| std::env::var("API_PORT").ok().and_then(|v| v.parse().ok()))
                    .or(config.api_port)
                    .unwrap_or(8081);

                let store =
                    PostgresStore::new(&config.database.url, config.database.max_connections)
                        .await?;

                let api_config = StrategyConfigState {
                    symbols: vec![config.market.market_slug.clone()],
                    min_move: config.strategy.move_pct.to_f64().unwrap_or(0.0),
                    max_entry: config.strategy.sum_target.to_f64().unwrap_or(1.0),
                    shares: i32::try_from(config.strategy.shares).unwrap_or(i32::MAX),
                    predictive: false,
                    exit_edge_floor: None,
                    exit_price_band: None,
                    time_decay_exit_secs: None,
                    liquidity_exit_spread_bps: None,
                };

                start_api_server(Arc::new(store), api_port, api_config).await?;
            }

            #[cfg(not(feature = "api"))]
            {
                return Err(PloyError::Validation(
                    "API feature not enabled. Rebuild with --features api".to_string(),
                ));
            }
        }
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
        Some(Commands::Trade {
            series,
            shares,
            move_pct,
            sum_target,
            dry_run,
        }) => {
            init_logging();
            if !*dry_run {
                enforce_coordinator_only_live("ploy trade")?;
            }
            run_trade_mode(series, *shares, *move_pct, *sum_target, *dry_run).await?;
        }
        Some(Commands::Scan {
            series,
            sum_target,
            move_pct,
            watch,
        }) => {
            init_logging();
            run_scan_mode(series, *sum_target, *move_pct, *watch).await?;
        }
        Some(Commands::Analyze { event }) => {
            init_logging();
            run_analyze_mode(event).await?;
        }
        Some(Commands::EventEdge {
            event,
            title,
            min_edge,
            max_entry,
            shares,
            interval_secs,
            watch,
            trade,
            dry_run,
        }) => {
            init_logging();
            let cfg_for_lockdown = AppConfig::load_from(&cli.config)
                .unwrap_or_else(|_| AppConfig::default_config(true, "btc-price-series-15m"));
            if cfg_for_lockdown.openclaw_runtime_lockdown() {
                return Err(PloyError::Validation(
                    "event-edge runtime is disabled in openclaw lockdown mode".to_string(),
                ));
            }
            if *trade && !*dry_run {
                enforce_coordinator_only_live("ploy event-edge --trade")?;
            }
            run_event_edge_mode(
                event.as_deref(),
                title.as_deref(),
                *min_edge,
                *max_entry,
                *shares,
                *interval_secs,
                *watch,
                *trade,
                *dry_run,
            )
            .await?;
        }
        Some(Commands::Account { orders, positions }) => {
            init_logging_simple();
            run_account_mode(*orders, *positions).await?;
        }
        Some(Commands::Ev {
            price,
            probability,
            hours,
            table,
        }) => {
            init_logging_simple();
            cli::calculate_ev(*price, *probability, *hours, *table).await?;
        }
        Some(Commands::MarketMake { token, detail }) => {
            init_logging_simple();
            let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
            cli::analyze_market_making(&client, token, *detail).await?;
        }
        Some(Commands::Rpc) => {
            init_logging_simple();
            ploy::cli::rpc::run_rpc(&cli.config).await?;
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
            predictive,
            min_time,
            max_time,
            vwap_confirm,
            vwap_lookback,
            vwap_min_dev,
        }) => {
            init_logging();
            if !*dry_run {
                enforce_coordinator_only_live("ploy momentum")?;
            }
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
                *predictive,
                *min_time,
                *max_time,
                *vwap_confirm,
                *vwap_lookback,
                *vwap_min_dev,
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
            if !*dry_run {
                enforce_coordinator_only_live("ploy split-arb")?;
            }
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
            if *enable_trading {
                enforce_coordinator_only_live("ploy agent --enable-trading")?;
            }
            main_agent_mode::run_agent_mode(
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
                ploy::tui::run_dashboard_auto(series.as_deref(), cli.dry_run.unwrap_or(true))
                    .await?;
            }
        }
        Some(Commands::Collect {
            symbols,
            markets,
            duration,
        }) => {
            init_logging();
            run_collect_mode(symbols, markets.as_deref(), *duration).await?;
        }
        Some(Commands::OrderbookHistory {
            asset_ids,
            start_ms,
            end_ms,
            lookback_secs,
            levels,
            sample_ms,
            limit,
            max_pages,
            base_url,
            resume_from_db,
        }) => {
            init_logging();
            run_orderbook_history_mode(
                &cli.config,
                asset_ids,
                *start_ms,
                *end_ms,
                *lookback_secs,
                *levels,
                *sample_ms,
                *limit,
                *max_pages,
                base_url,
                *resume_from_db,
            )
            .await?;
        }
        Some(Commands::Crypto(crypto_cmd)) => {
            init_logging();
            run_crypto_command(crypto_cmd).await?;
        }
        Some(Commands::Sports(sports_cmd)) => {
            init_logging();
            run_sports_command(sports_cmd).await?;
        }
        Some(Commands::Politics(politics_cmd)) => {
            init_logging();
            run_politics_command(politics_cmd).await?;
        }
        Some(Commands::Strategy(strategy_cmd)) => {
            init_logging();
            strategy_cmd.clone().run().await?;
        }
        #[cfg(feature = "rl")]
        Some(Commands::Rl(rl_cmd)) => {
            init_logging();
            run_rl_command(rl_cmd).await?;
        }
        Some(Commands::Claim {
            check_only,
            min_size,
            interval,
        }) => {
            init_logging();
            run_claimer(*check_only, *min_size, *interval).await?;
        }
        Some(Commands::History {
            limit,
            symbol,
            stats_only,
            open_only,
        }) => {
            run_history(*limit, symbol.clone(), *stats_only, *open_only).await?;
        }
        Some(Commands::Paper {
            symbols,
            min_vol_edge,
            min_price_edge,
            log_file,
            stats_interval,
        }) => {
            init_logging();
            run_paper_trading(
                symbols.clone(),
                *min_vol_edge,
                *min_price_edge,
                log_file.clone(),
                *stats_interval,
            )
            .await?;
        }
        Some(Commands::Platform {
            action,
            crypto,
            sports,
            politics,
            dry_run,
            pause,
            resume,
        }) => {
            init_logging();
            run_platform_mode(
                action,
                *crypto,
                *sports,
                *politics,
                *dry_run,
                pause.clone(),
                resume.clone(),
                &cli,
            )
            .await?;
        }
        Some(Commands::Run) | None => {
            init_logging();
            run_bot(&cli).await?;
        }
    }

    Ok(())
}

/// Run the auto-claimer to redeem winning positions
async fn run_claimer(check_only: bool, min_size: f64, interval: u64) -> Result<()> {
    main_modes::run_claimer(check_only, min_size, interval).await
}

/// Run the multi-agent platform (Coordinator + Agents)
async fn run_platform_mode(
    action: &str,
    crypto: bool,
    sports: bool,
    politics: bool,
    dry_run: bool,
    pause: Option<String>,
    resume: Option<String>,
    cli: &Cli,
) -> Result<()> {
    main_modes::run_platform_mode(
        action, crypto, sports, politics, dry_run, pause, resume, cli,
    )
    .await
}

/// Run paper trading with real market data but no execution
async fn run_paper_trading(
    symbols: String,
    min_vol_edge: f64,
    min_price_edge: f64,
    log_file: String,
    stats_interval: u64,
) -> Result<()> {
    main_modes::run_paper_trading(
        symbols,
        min_vol_edge,
        min_price_edge,
        log_file,
        stats_interval,
    )
    .await
}

/// View trading history and statistics
async fn run_history(
    limit: usize,
    symbol: Option<String>,
    stats_only: bool,
    open_only: bool,
) -> Result<()> {
    main_modes::run_history(limit, symbol, stats_only, open_only).await
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
        )
        .await?
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
    let token_to_side: Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, ploy::domain::Side>>,
    > = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

    // WebSocket client
    let ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));

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
                        let new_tokens: Vec<String> =
                            market.tokens.iter().map(|t| t.token_id.clone()).collect();

                        let tokens_read = current_tokens.read().await;
                        let tokens_changed = *tokens_read != new_tokens;
                        drop(tokens_read);

                        if tokens_changed {
                            println!("\n\x1b[33m═══ Market Rotation ═══\x1b[0m");
                            println!("\x1b[32mNew market:\x1b[0m {}", title);

                            ws.quote_cache().clear();
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
                                println!(
                                    "  {} ({}...): {}",
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

            let token_ids: Vec<String> = market.tokens.iter().map(|t| t.token_id.clone()).collect();

            let mut side_map = token_to_side.write().await;
            for token in &market.tokens {
                let side = match token.outcome.to_lowercase().as_str() {
                    "yes" | "up" => ploy::domain::Side::Up,
                    _ => ploy::domain::Side::Down,
                };
                ws.register_token(&token.token_id, side).await;
                side_map.insert(token.token_id.clone(), side);

                let price_str = token.price.as_deref().unwrap_or("N/A");
                println!(
                    "  {} ({}...): {}",
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

            println!(
                "\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m"
            );
            println!(
                "\x1b[36m║  Trading Active: move={:.1}%, window=3s, target={:.4}       ║\x1b[0m",
                move_pct * 100.0,
                sum_target
            );
            println!(
                "\x1b[36m║  Shares: {}  |  Mode: {}                              ║\x1b[0m",
                shares,
                if dry_run { "DRY RUN" } else { "LIVE" }
            );
            println!(
                "\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n"
            );

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
                        println!("[{}] {} Bid: {:.4} | Ask: {:.4}", now, side_str, bid, ask);

                        // Check for dump signal (only if no active position)
                        if position.is_none() {
                            if let Some(signal) =
                                detector.update(&update.quote, current_round.as_deref())
                            {
                                println!("\n\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                println!("\x1b[41;97m  🚨 DUMP SIGNAL - EXECUTING LEG1                          \x1b[0m");
                                println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                                println!("  Side: {:?}", signal.side);
                                println!(
                                    "  Drop: {:.2}% ({:.4} → {:.4})",
                                    signal.drop_pct * Decimal::from(100),
                                    signal.reference_price,
                                    signal.trigger_price
                                );
                                println!("  Spread: {} bps", signal.spread_bps);

                                // Get token ID for this side
                                let side_map = token_to_side.read().await;
                                let token_id = side_map
                                    .iter()
                                    .find(|(_, &s)| s == signal.side)
                                    .map(|(id, _)| id.clone());
                                drop(side_map);

                                if let Some(token_id) = token_id {
                                    // Execute Leg1 order
                                    let _ = token_id;
                                    println!("\n  Submitting Leg1 order...");
                                    println!(
                                        "\x1b[31m  ✗ Direct submit disabled. Use platform/coordinator intent ingress.\x1b[0m"
                                    );
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
                                        println!(
                                            "  Leg1 ({:?}): {:.4}",
                                            pos.leg1_side, pos.leg1_price
                                        );
                                        println!(
                                            "  Leg2 ({:?}): {:.4}",
                                            opposite_side, opposite_ask
                                        );
                                        println!("  Sum: {:.4} <= Target: {:.4}", sum, target);
                                        println!(
                                            "  Potential Profit: {:.2}%",
                                            (Decimal::ONE - sum) * Decimal::from(100)
                                        );

                                        // Get token ID for opposite side
                                        let side_map = token_to_side.read().await;
                                        let token_id = side_map
                                            .iter()
                                            .find(|(_, &s)| s == opposite_side)
                                            .map(|(id, _)| id.clone());
                                        drop(side_map);

                                        if let Some(token_id) = token_id {
                                            let _ = token_id;
                                            println!("\n  Submitting Leg2 order...");
                                            println!(
                                                "\x1b[31m  ✗ Direct submit disabled. Use platform/coordinator intent ingress.\x1b[0m"
                                            );
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
    let ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));

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
    println!(
        "  Mode: {}",
        if continuous { "Continuous" } else { "One-shot" }
    );
    println!();

    // Create multi-event monitor
    let mut monitor = MultiEventMonitor::new(series_id, config.clone());

    // Initial event discovery
    println!("\x1b[33mDiscovering active events...\x1b[0m");
    let token_ids = monitor.refresh_events(&client).await?;

    if token_ids.is_empty() {
        println!(
            "\x1b[31mNo active events found in series {}.\x1b[0m",
            series_id
        );
        return Ok(());
    }

    println!(
        "\x1b[32m✓ Found {} active events ({} tokens)\x1b[0m\n",
        monitor.event_count(),
        token_ids.len()
    );

    // Display discovered events
    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║                    Active Events                             ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");

    for summary in monitor.summary() {
        let time_str = format!(
            "{}m {}s",
            summary.time_remaining.num_minutes(),
            summary.time_remaining.num_seconds() % 60
        );
        println!(
            "  {} - {} remaining",
            if summary.event_slug.is_empty() {
                &summary.event_id
            } else {
                &summary.event_slug
            },
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
    println!(
        "\x1b[36m║  Scanning for arbitrage opportunities (sum <= {:.4})         ║\x1b[0m",
        sum_target
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Process quote updates
    let mut updates = ws.subscribe_updates();
    let scan_handle = {
        let running = Arc::clone(&running);
        let ws = Arc::clone(&ws);
        let target = Decimal::from_str(&format!("{:.4}", sum_target)).unwrap_or(dec!(0.95));

        tokio::spawn(async move {
            let mut last_summary_time = std::time::Instant::now();
            let mut up_quotes: std::collections::HashMap<String, rust_decimal::Decimal> =
                std::collections::HashMap::new();
            let mut down_quotes: std::collections::HashMap<String, rust_decimal::Decimal> =
                std::collections::HashMap::new();

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
        let yes_str = summary
            .yes_price
            .map(|p| format!("{:.2}¢", p * dec!(100)))
            .unwrap_or("-".into());
        let no_str = summary
            .no_price
            .map(|p| format!("{:.2}¢", p * dec!(100)))
            .unwrap_or("-".into());
        let prob_str = summary
            .implied_prob_pct
            .map(|p| format!("{:.1}%", p))
            .unwrap_or("-".into());
        println!(
            "  {} | Yes: {} | No: {} | Prob: {}",
            summary.name, yes_str, no_str, prob_str
        );
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

/// Scan a Polymarket event for external-data-driven mispricing and optionally trade.
async fn run_event_edge_mode(
    event_id: Option<&str>,
    title: Option<&str>,
    min_edge: f64,
    max_entry: f64,
    shares: u64,
    interval_secs: u64,
    watch: bool,
    trade: bool,
    dry_run: bool,
) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{run_event_edge, EventEdgeConfig};
    use rust_decimal::prelude::FromPrimitive;
    use std::time::Duration;

    let client = if trade && !dry_run {
        // Authenticated trading client (requires env vars).
        let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
        let funder = std::env::var("POLYMARKET_FUNDER").ok();
        if let Some(funder_addr) = funder {
            PolymarketClient::new_authenticated_proxy(
                "https://clob.polymarket.com",
                wallet,
                &funder_addr,
                true,
            )
            .await?
        } else {
            PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, true).await?
        }
    } else {
        // Read-only client is enough for scanning and dry-run execution.
        PolymarketClient::new("https://clob.polymarket.com", true)?
    };

    let cfg = EventEdgeConfig {
        event_id: event_id.map(|s| s.to_string()),
        title: title.map(|s| s.to_string()),
        min_edge: rust_decimal::Decimal::from_f64(min_edge)
            .ok_or_else(|| anyhow::anyhow!("Invalid min_edge"))?,
        max_entry: rust_decimal::Decimal::from_f64(max_entry)
            .ok_or_else(|| anyhow::anyhow!("Invalid max_entry"))?,
        shares,
        interval: Duration::from_secs(interval_secs.max(5)),
        watch,
        trade,
        dry_run,
    };

    run_event_edge(&client, cfg).await
}

async fn create_pm_client(rest_url: &str, dry_run: bool) -> Result<PolymarketClient> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;

    if dry_run {
        return PolymarketClient::new(rest_url, true);
    }

    let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
    let funder = std::env::var("POLYMARKET_FUNDER").ok();
    if let Some(funder_addr) = funder {
        PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, true).await
    } else {
        PolymarketClient::new_authenticated(rest_url, wallet, true).await
    }
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

            // Check if funder address is set (for proxy/Magic wallets)
            let funder = std::env::var("POLYMARKET_FUNDER").ok();
            if let Some(ref funder_addr) = funder {
                println!("  Funder (proxy wallet): {}", funder_addr);
            }

            println!("  Authenticating with Polymarket CLOB...");
            let auth_result = if let Some(funder_addr) = funder {
                // Use proxy wallet authentication
                PolymarketClient::new_authenticated_proxy(
                    "https://clob.polymarket.com",
                    wallet,
                    &funder_addr,
                    true,
                )
                .await
            } else {
                // Use regular EOA authentication
                PolymarketClient::new_authenticated("https://clob.polymarket.com", wallet, true)
                    .await
            };

            match auth_result {
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
    let market_query = cli.market.as_deref().unwrap_or("btc-price-series-15m");
    println!("Searching for market: {}\n", market_query);
    let markets = client.search_markets(market_query).await?;

    if markets.is_empty() {
        println!("\x1b[31mNo markets found for: {}\x1b[0m", market_query);
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
    let ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));

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
                        let new_tokens: Vec<String> =
                            market.tokens.iter().map(|t| t.token_id.clone()).collect();

                        let tokens_read = current_tokens.read().await;
                        let tokens_changed = *tokens_read != new_tokens;
                        drop(tokens_read);

                        if tokens_changed {
                            // Market rotated!
                            println!("\n\x1b[33m═══ Market Rotation ═══\x1b[0m");
                            println!("\x1b[32mNew market:\x1b[0m {}", title);

                            // Clear old tokens from cache
                            ws.quote_cache().clear();

                            // Register new tokens
                            for token in &market.tokens {
                                let side = match token.outcome.to_lowercase().as_str() {
                                    "yes" | "up" => ploy::domain::Side::Up,
                                    _ => ploy::domain::Side::Down,
                                };
                                ws.register_token(&token.token_id, side).await;

                                let price_str = token.price.as_deref().unwrap_or("N/A");
                                println!(
                                    "  {} ({}...): {}",
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

            let token_ids: Vec<String> = market.tokens.iter().map(|t| t.token_id.clone()).collect();

            for token in &market.tokens {
                let side = match token.outcome.to_lowercase().as_str() {
                    "yes" | "up" => ploy::domain::Side::Up,
                    _ => ploy::domain::Side::Down,
                };
                ws.register_token(&token.token_id, side).await;

                let price_str = token.price.as_deref().unwrap_or("N/A");
                println!(
                    "  {} ({}...): {}",
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
                move_pct: dec!(0.05),   // 5% drop triggers (test mode)
                sum_target: dec!(0.98), // Target sum for leg2
                fee_buffer: dec!(0.005),
                slippage_buffer: dec!(0.01),
                profit_buffer: dec!(0.005),
            };
            let mut detector = SignalDetector::with_window(config, 10); // 10-second window
            let mut current_round: Option<String> = None;
            let mut leg1_price: Option<(ploy::domain::Side, rust_decimal::Decimal)> = None;

            println!(
                "\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m"
            );
            println!(
                "\x1b[36m║  Signal Detection: move_pct=5%, window=10s, target=0.98     ║\x1b[0m"
            );
            println!(
                "\x1b[36m║  (Test mode - production uses 15%/3s)                       ║\x1b[0m"
            );
            println!(
                "\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n"
            );

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
                        if let Some(signal) =
                            detector.update(&update.quote, current_round.as_deref())
                        {
                            println!("\n\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                            println!("\x1b[41;97m  🚨 DUMP SIGNAL DETECTED!                                  \x1b[0m");
                            println!("\x1b[41;97m ══════════════════════════════════════════════════════════ \x1b[0m");
                            println!("  Side: {:?}", signal.side);
                            println!(
                                "  Drop: {:.2}% ({:.4} → {:.4})",
                                signal.drop_pct * rust_decimal::Decimal::from(100),
                                signal.reference_price,
                                signal.trigger_price
                            );
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
                                        println!(
                                            "  Leg2 ({:?}): {:.4}",
                                            opposite_side, opposite_ask
                                        );
                                        println!("  Sum: {:.4} <= Target: {:.4}", sum, target);
                                        println!(
                                            "  Potential Profit: {:.2}%",
                                            (rust_decimal::Decimal::ONE - sum)
                                                * rust_decimal::Decimal::from(100)
                                        );
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
async fn run_single_market_watch(
    client: &PolymarketClient,
    market_info: ploy::adapters::MarketResponse,
) -> Result<()> {
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

    let ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));
    let token_ids: Vec<String> = market_info
        .tokens
        .iter()
        .map(|t| t.token_id.clone())
        .collect();

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
            if let Some(dry_run) = cli.dry_run {
                c.dry_run.enabled = dry_run;
            }
            if let Some(ref market) = cli.market {
                c.market.market_slug = market.clone();
            }
            c
        }
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            // Use defaults
            info!("Using default configuration");
            AppConfig::default_config(
                cli.dry_run.unwrap_or(true),
                cli.market.as_deref().unwrap_or("btc-price-series-15m"),
            )
        }
    };

    info!(
        "Configuration: market={}, dry_run={}",
        config.market.market_slug, config.dry_run.enabled
    );

    if !config.dry_run.enabled && !legacy_live_allowed() {
        return Err(PloyError::Validation(
            "legacy `ploy run` live runtime is disabled by default; use `ploy platform start` (Coordinator-only live) or set PLOY_ALLOW_LEGACY_LIVE=true for explicit override".to_string(),
        ));
    }

    let parse_bool_env = |key: &str, default: bool| -> bool {
        std::env::var(key)
            .ok()
            .map(|raw| {
                matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(default)
    };

    let run_sqlx_migrations = parse_bool_env("PLOY_RUN_SQLX_MIGRATIONS", !config.dry_run.enabled);
    let require_sqlx_migrations =
        parse_bool_env("PLOY_REQUIRE_SQLX_MIGRATIONS", !config.dry_run.enabled);

    // Check if we can connect to database
    let store = match PostgresStore::new(&config.database.url, config.database.max_connections)
        .await
    {
        Ok(s) => {
            if run_sqlx_migrations {
                if let Err(e) = s.migrate().await {
                    if require_sqlx_migrations {
                        error!("Database migration failed: {}", e);
                        return Err(e);
                    }

                    warn!(
                            "Database migration failed but continuing due PLOY_REQUIRE_SQLX_MIGRATIONS=false: {}",
                            e
                        );
                }
            } else {
                if require_sqlx_migrations && !config.dry_run.enabled {
                    return Err(PloyError::Internal(
                        "PLOY_RUN_SQLX_MIGRATIONS=false and startup requires migrations"
                            .to_string(),
                    ));
                }
                info!(
                    run_sqlx_migrations = run_sqlx_migrations,
                    "Skipping SQLx migration runner"
                );
            }
            info!("Database connected");
            Some(s)
        }
        Err(e) => {
            if config.dry_run.enabled {
                warn!(
                    "Database connection failed in dry-run mode: {} - falling back to simple mode",
                    e
                );
                None
            } else {
                error!(
                    "Database connection failed in live mode: {} - aborting startup",
                    e
                );
                return Err(e);
            }
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
    // Safety: never start live trading when the DB indicates trading was halted.
    // (Crash recovery may have been skipped/failed; this enforces the invariant anyway.)
    if !config.dry_run.enabled {
        let today = chrono::Utc::now().date_naive();
        if store.is_trading_halted(today).await? {
            return Err(PloyError::Internal(
                "Trading halted - check daily_metrics for halt_reason".to_string(),
            ));
        }
    }

    // Initialize order executor
    let executor = OrderExecutor::new(clob_client.clone(), config.execution.clone())
        .with_idempotency(Arc::new(IdempotencyManager::new_with_account(
            store.clone(),
            config.account.id.clone(),
        )));

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
            .with_metrics(Arc::clone(&metrics)),
    );
    // Wire runtime connectivity into /health reporting.
    ws_client.set_health_state(Arc::clone(&health_state));

    // Periodic DB probe (used by /health components reporting).
    let db_health_handle = {
        let store = store.clone();
        let health = Arc::clone(&health_state);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tick.tick().await;
                let ok = sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(store.pool())
                    .await
                    .is_ok();
                health.record_db_check(ok).await;
            }
        })
    };
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

    #[cfg(feature = "api")]
    let api_handle = {
        use rust_decimal::prelude::ToPrimitive;

        let api_port = config.api_port.unwrap_or(8081);
        let api_config = StrategyConfigState {
            symbols: vec![config.market.market_slug.clone()],
            min_move: config.strategy.move_pct.to_f64().unwrap_or(0.0),
            max_entry: config.strategy.sum_target.to_f64().unwrap_or(1.0),
            shares: i32::try_from(config.strategy.shares).unwrap_or(i32::MAX),
            predictive: false,
            exit_edge_floor: None,
            exit_price_band: None,
            time_decay_exit_secs: None,
            liquidity_exit_spread_bps: None,
        };

        match start_api_server_background(Arc::new(store.clone()), api_port, api_config).await {
            Ok(handle) => {
                info!("API server started on port {}", api_port);
                Some(handle)
            }
            Err(e) => {
                if config.dry_run.enabled {
                    warn!("API server failed to start in dry-run mode: {}", e);
                    None
                } else {
                    return Err(e);
                }
            }
        }
    };
    #[cfg(not(feature = "api"))]
    let api_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;

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

    // Spawn always-on event-edge agent (optional)
    let event_edge_handle = if config.openclaw_runtime_lockdown() {
        warn!("event-edge runtime disabled by openclaw lockdown");
        tokio::spawn(async {})
    } else if let Some(agent_cfg) = config.event_edge_agent.clone() {
        if agent_cfg.enabled {
            use ploy::services::{
                EventEdgeAgent, EventEdgeClaudeFrameworkAgent, EventEdgeEventDrivenAgent,
            };
            let rest_url = config.market.rest_url.clone();
            let global_dry_run = config.dry_run.enabled;
            tokio::spawn(async move {
                let dry_run = global_dry_run;
                let can_trade = agent_cfg.trade && !dry_run;
                let client = match create_pm_client(&rest_url, !can_trade).await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(
                            "EventEdgeAgent: failed to build client (trade={}): {}",
                            can_trade, e
                        );
                        return;
                    }
                };

                match agent_cfg.framework.as_str() {
                    "claude_agent_sdk" => {
                        let agent = EventEdgeClaudeFrameworkAgent::new(client, agent_cfg);
                        if let Err(e) = agent.run_forever().await {
                            error!("EventEdgeClaudeFrameworkAgent error: {}", e);
                        }
                    }
                    "event_driven" => match EventEdgeEventDrivenAgent::new(client, agent_cfg).await
                    {
                        Ok(agent) => {
                            if let Err(e) = agent.run_forever().await {
                                error!("EventEdgeEventDrivenAgent error: {}", e);
                            }
                        }
                        Err(e) => error!("EventEdgeEventDrivenAgent init error: {}", e),
                    },
                    _ => {
                        let agent = EventEdgeAgent::new(client, agent_cfg);
                        if let Err(e) = agent.run_forever().await {
                            error!("EventEdgeAgent error: {}", e);
                        }
                    }
                }
            })
        } else {
            tokio::spawn(async {})
        }
    } else {
        tokio::spawn(async {})
    };

    // Wait for shutdown signal
    info!("Bot is running. Press Ctrl+C to stop.");
    shutdown_signal().await;

    info!("Shutting down...");
    engine.shutdown().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    health_handle.abort();
    if let Some(api_handle) = api_handle {
        api_handle.abort();
    }
    ws_handle.abort();
    collector_handle.abort();
    engine_handle.abort();
    round_handle.abort();
    status_handle.abort();
    event_edge_handle.abort();
    db_health_handle.abort();

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

    let token_ids: Vec<String> = market_info
        .tokens
        .iter()
        .map(|t| t.token_id.clone())
        .collect();

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

    // Spawn always-on event-edge agent (optional)
    let event_edge_handle = if config.openclaw_runtime_lockdown() {
        warn!("event-edge runtime disabled by openclaw lockdown");
        tokio::spawn(async {})
    } else if let Some(agent_cfg) = config.event_edge_agent.clone() {
        if agent_cfg.enabled {
            use ploy::services::{
                EventEdgeAgent, EventEdgeClaudeFrameworkAgent, EventEdgeEventDrivenAgent,
            };
            let rest_url = config.market.rest_url.clone();
            let global_dry_run = config.dry_run.enabled;
            tokio::spawn(async move {
                let dry_run = global_dry_run;
                let can_trade = agent_cfg.trade && !dry_run;
                let client = match create_pm_client(&rest_url, !can_trade).await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(
                            "EventEdgeAgent: failed to build client (trade={}): {}",
                            can_trade, e
                        );
                        return;
                    }
                };

                match agent_cfg.framework.as_str() {
                    "claude_agent_sdk" => {
                        let agent = EventEdgeClaudeFrameworkAgent::new(client, agent_cfg);
                        if let Err(e) = agent.run_forever().await {
                            error!("EventEdgeClaudeFrameworkAgent error: {}", e);
                        }
                    }
                    "event_driven" => match EventEdgeEventDrivenAgent::new(client, agent_cfg).await
                    {
                        Ok(agent) => {
                            if let Err(e) = agent.run_forever().await {
                                error!("EventEdgeEventDrivenAgent error: {}", e);
                            }
                        }
                        Err(e) => error!("EventEdgeEventDrivenAgent init error: {}", e),
                    },
                    _ => {
                        let agent = EventEdgeAgent::new(client, agent_cfg);
                        if let Err(e) = agent.run_forever().await {
                            error!("EventEdgeAgent error: {}", e);
                        }
                    }
                }
            })
        } else {
            tokio::spawn(async {})
        }
    } else {
        tokio::spawn(async {})
    };

    info!("Bot is running in simple mode. Press Ctrl+C to stop.");
    shutdown_signal().await;

    ws_handle.abort();
    print_handle.abort();
    event_edge_handle.abort();

    info!("Shutdown complete");
    Ok(())
}

fn enforce_coordinator_only_live(cmd: &str) -> Result<()> {
    if legacy_live_allowed() {
        return Ok(());
    }

    let msg = format!(
        "legacy `{}` live runtime is disabled by default; use `ploy platform start` (Coordinator-only live) or set PLOY_ALLOW_LEGACY_LIVE=true for explicit override",
        cmd
    );
    warn!("{msg}");
    println!("\x1b[31m✗ {}\x1b[0m", msg);
    Err(PloyError::Validation(msg))
}

fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ploy=debug,sqlx=warn"));

    // Check if we should write to file (prefer PLOY_LOG_DIR, fallback to LOG_DIR or /var/log/ploy).
    let log_dir = std::env::var("PLOY_LOG_DIR")
        .or_else(|_| std::env::var("LOG_DIR"))
        .unwrap_or_else(|_| "/var/log/ploy".to_string());

    // Try to create log directory.
    //
    // Important: `tracing_appender::rolling::daily` will panic (and in our release build,
    // abort) if it can't create the initial log file. So we must preflight writability.
    let file_layer = if std::fs::create_dir_all(&log_dir).is_ok() {
        let test_path = std::path::Path::new(&log_dir).join(".ploy_write_test");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&test_path)
        {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_path);

                // Daily rotating file appender
                let file_appender = tracing_appender::rolling::daily(&log_dir, "ploy.log");
                let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

                // Keep the guard alive by leaking it (acceptable for long-running process)
                Box::leak(Box::new(_guard));

                Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false) // No color codes in file
                        .with_target(true),
                )
            }
            Err(e) => {
                eprintln!(
                    "Warning: Could not write to log directory {} ({}), file logging disabled",
                    log_dir, e
                );
                None
            }
        }
    } else {
        eprintln!(
            "Warning: Could not create log directory {}, file logging disabled",
            log_dir
        );
        None
    };

    // Console layer
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    // Combine layers
    let file_logging_enabled = file_layer.is_some();
    tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer)
        .init();

    if file_logging_enabled {
        eprintln!("Logging to: {}/ploy.log", log_dir);
    }
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
            Ok(mut stream) => {
                stream.recv().await;
            }
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
        Err(_) => warn!(
            "{} shutdown timed out after {}s, forcing",
            name, timeout_secs
        ),
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
                    .abort_cycle(
                        cycle.cycle_id,
                        "Insufficient time remaining after crash recovery",
                    )
                    .await?;
            } else {
                warn!(
                    "Cycle {} in state {} could potentially be resumed ({:?} remaining) - aborting for safety",
                    cycle.cycle_id, cycle.state, remaining
                );
                store
                    .abort_cycle(
                        cycle.cycle_id,
                        "Aborted during crash recovery - manual resume not implemented",
                    )
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
    predictive: bool,
    min_time: u64,
    max_time: u64,
    vwap_confirm: bool,
    vwap_lookback: u64,
    vwap_min_dev: f64,
) -> Result<()> {
    use ploy::adapters::{BinanceWebSocket, PolymarketWebSocket};
    use ploy::signing::Wallet;
    use ploy::strategy::{
        AutoClaimer, ClaimerConfig, ExitConfig, MomentumConfig, MomentumEngine, OrderExecutor,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};

    info!("Starting momentum strategy mode");

    // Parse symbols
    let symbols_vec: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();
    info!("Trading symbols: {:?}", symbols_vec);

    // Determine mode settings
    let (hold_to_resolution, min_time_remaining, max_time_remaining) = if predictive {
        // Predictive mode: enter early, use take-profit/stop-loss exits
        (false, min_time, max_time)
    } else {
        // Confirmatory mode (CRYINGLITTLEBABY): enter late, hold to resolution
        (true, 60, 300)
    };

    // Build baseline volatility map for each symbol
    let mut baseline_volatility = std::collections::HashMap::new();
    baseline_volatility.insert("BTCUSDT".into(), dec!(0.0005)); // 0.05%
    baseline_volatility.insert("ETHUSDT".into(), dec!(0.0008)); // 0.08%
    baseline_volatility.insert("SOLUSDT".into(), dec!(0.0015)); // 0.15%
    baseline_volatility.insert("XRPUSDT".into(), dec!(0.0012)); // 0.12%

    // Build momentum config
    let momentum_config = MomentumConfig {
        min_move_pct: Decimal::from_str(&format!("{:.6}", min_move / 100.0))
            .unwrap_or(dec!(0.0005)),
        max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
            .unwrap_or(dec!(0.35)),
        min_edge: Decimal::from_str(&format!("{:.6}", min_edge / 100.0)).unwrap_or(dec!(0.03)),
        lookback_secs: 5,
        // Multi-timeframe momentum (always enabled) with volatility adjustment
        use_volatility_adjustment: true, // Adjust threshold by current volatility
        baseline_volatility,
        volatility_lookback_secs: 60, // 60-second rolling volatility
        shares_per_trade: shares,
        max_positions,
        cooldown_secs: 60,
        max_daily_trades: 20,
        symbols: symbols_vec.clone(),
        // Mode selection
        hold_to_resolution,
        min_time_remaining_secs: min_time_remaining,
        max_time_remaining_secs: max_time_remaining,
        // Cross-symbol risk control
        max_window_exposure_usd: dec!(25), // Max $25 total per 15-min window
        best_edge_only: true,              // Only take highest edge signal
        signal_collection_delay_ms: 2000,  // 2 second delay to collect signals
        // === ENHANCED MOMENTUM DETECTION ===
        require_mtf_agreement: true, // Require all timeframes to agree on direction
        min_obi_confirmation: dec!(0.05), // 5% OBI confirmation
        use_kline_volatility: true,  // Use K-line historical volatility
        time_decay_factor: dec!(0.30), // 30% decay in later window
        use_price_to_beat: true,     // Consider price-to-beat from market question
        dynamic_position_sizing: true, // Scale position by confidence
        min_confidence: 0.5,         // Minimum 50% confidence
        use_kelly_sizing: true,      // Kelly scaling enabled
        kelly_fraction_cap: dec!(0.25), // Quarter-Kelly cap

        // VWAP confirmation (optional)
        require_vwap_confirmation: vwap_confirm,
        vwap_lookback_secs: vwap_lookback,
        min_vwap_deviation: Decimal::from_str(&format!("{:.6}", vwap_min_dev / 100.0))
            .unwrap_or(dec!(0)),
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

    // Print config - different banner for each mode
    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    if predictive {
        println!("\x1b[33m║       PREDICTIVE MODE 📈 (Early Entry + TP/SL)              ║\x1b[0m");
        println!(
            "\x1b[33m║   Entry: {}-{}s before resolution | Exit: TP/SL           ║\x1b[0m",
            min_time_remaining, max_time_remaining
        );
    } else {
        println!("\x1b[36m║       CRYINGLITTLEBABY CONFIRMATORY MODE 🎯                  ║\x1b[0m");
        println!("\x1b[36m║   (Buy confirmed winners near resolution → collect $1)       ║\x1b[0m");
    }
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
    if predictive {
        println!(
            "\x1b[36m║\x1b[0m  Take Profit: {:.0}%  |  Stop Loss: {:.0}%                    \x1b[36m║\x1b[0m",
            take_profit, stop_loss
        );
    } else {
        println!(
            "\x1b[36m║\x1b[0m  Exit: Hold to resolution ($1 payout)                       \x1b[36m║\x1b[0m"
        );
    }
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

        // Check for proxy wallet funder address
        let funder = std::env::var("POLYMARKET_FUNDER").ok();
        if let Some(ref funder_addr) = funder {
            info!("Using proxy wallet authentication, funder: {}", funder_addr);
            PolymarketClient::new_authenticated_proxy(
                "https://clob.polymarket.com",
                wallet,
                funder_addr,
                true, // neg_risk for UP/DOWN markets
            )
            .await?
        } else {
            PolymarketClient::new_authenticated(
                "https://clob.polymarket.com",
                wallet,
                true, // neg_risk for UP/DOWN markets
            )
            .await?
        }
    };

    // Create executor
    let executor = OrderExecutor::new(pm_client.clone(), Default::default());

    // Create risk config with fund management
    // Position sizing: use percentage of available balance per trade
    // With 4 symbols and max 1 position each, 20% = up to 80% deployed
    let risk_config = ploy::config::RiskConfig {
        max_single_exposure_usd: dec!(50), // Max $50 per single trade
        min_remaining_seconds: 30,
        max_consecutive_failures: 3,
        daily_loss_limit_usd: dec!(100), // Stop trading after $100 daily loss
        leg2_force_close_seconds: 20,
        // Fund management settings
        max_positions: max_positions as u32, // Limit concurrent positions
        max_positions_per_symbol: 1,         // Only 1 position per symbol (BTC, ETH, etc.)
        position_size_pct: Some(dec!(0.20)), // 20% of available balance per trade
        fixed_amount_usd: None,              // Use percentage-based sizing instead
        min_balance_usd: dec!(5),            // Keep $5 minimum reserve
    };

    // Create momentum engine with fund management
    let mut engine = MomentumEngine::new_with_fund_manager(
        momentum_config,
        exit_config,
        pm_client.clone(),
        executor,
        risk_config,
        dry_run,
    );

    // Optional auto-claim wiring for live momentum mode.
    // Enabled by default in live mode; disable with PLOY_AUTO_CLAIM=false.
    if !dry_run {
        let auto_claim_enabled = std::env::var("PLOY_AUTO_CLAIM")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "y" | "on"
                )
            })
            .unwrap_or(true);

        if auto_claim_enabled {
            let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
                .or_else(|_| std::env::var("PRIVATE_KEY"))
                .ok();

            if let Some(private_key) = private_key {
                let check_interval_secs = std::env::var("CLAIMER_CHECK_INTERVAL_SECS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(60);
                let min_claim_size = std::env::var("CLAIMER_MIN_CLAIM_SIZE")
                    .ok()
                    .and_then(|v| Decimal::from_str(v.trim()).ok())
                    .filter(|v| *v > Decimal::ZERO)
                    .unwrap_or(Decimal::ONE);

                let claimer = AutoClaimer::new(
                    pm_client.clone(),
                    ClaimerConfig {
                        check_interval_secs,
                        min_claim_size,
                        auto_claim: true,
                        private_key: Some(private_key),
                    },
                );

                engine = engine.with_claimer(claimer);
                info!(
                    "Auto-claimer enabled for momentum (interval={}s, min_claim_size=${})",
                    check_interval_secs, min_claim_size
                );
            } else {
                warn!(
                    "Auto-claimer requested for live momentum, but POLYMARKET_PRIVATE_KEY/PRIVATE_KEY is missing"
                );
            }
        } else {
            info!("Auto-claimer disabled via PLOY_AUTO_CLAIM=false");
        }
    }

    // Refresh events to get token IDs
    info!("Fetching active Polymarket events...");
    if let Err(e) = engine.event_matcher().refresh().await {
        error!("Failed to fetch events: {}", e);
    }

    let token_mappings = engine.event_matcher().get_token_mappings().await;
    let token_ids: Vec<String> = token_mappings.iter().map(|(id, _)| id.clone()).collect();
    info!("Found {} tokens to subscribe", token_ids.len());

    if token_ids.is_empty() {
        warn!("No active events found for the specified symbols");
        warn!(
            "Make sure there are active UP/DOWN markets for: {:?}",
            symbols_vec
        );
        return Ok(());
    }

    // Create Binance WebSocket
    info!("Connecting to Binance WebSocket...");
    let binance_ws = Arc::new(BinanceWebSocket::new(symbols_vec));
    let binance_cache = binance_ws.price_cache().clone();
    // Subscribe BEFORE spawning the task (must happen before run() consumes messages)
    let binance_rx = binance_ws.subscribe();

    // Create Polymarket WebSocket
    info!("Connecting to Polymarket WebSocket...");
    let pm_ws = Arc::new(PolymarketWebSocket::new(
        "wss://ws-subscriptions-clob.polymarket.com/ws/market",
    ));
    // Register tokens with their correct sides (UP or DOWN)
    for (token_id, side) in &token_mappings {
        pm_ws.register_token(token_id, *side).await;
    }
    info!(
        "Registered {} tokens with UP/DOWN mappings",
        token_mappings.len()
    );
    let pm_cache = pm_ws.quote_cache().clone();
    // Subscribe BEFORE spawning the task
    let pm_rx = pm_ws.subscribe_updates();

    // Running flag for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let _running_ws = Arc::clone(&running);
    let _running_engine = Arc::clone(&running);

    // Spawn Binance WebSocket task
    let binance_ws_clone = Arc::clone(&binance_ws);
    let binance_handle = tokio::spawn(async move {
        if let Err(e) = binance_ws_clone.run().await {
            error!("Binance WebSocket error: {}", e);
        }
    });

    // Spawn Polymarket WebSocket task
    let pm_ws_clone = Arc::clone(&pm_ws);
    let pm_handle = tokio::spawn(async move {
        let _ = pm_ws_clone.run(token_ids).await;
    });

    // Spawn engine task
    let engine_handle = tokio::spawn(async move {
        if let Err(e) = engine
            .run(binance_rx, pm_rx, &binance_cache, &pm_cache)
            .await
        {
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
            .unwrap_or_else(|_| Decimal::new(35, 2)), // 0.35 fallback
        target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
            .unwrap_or_else(|_| Decimal::new(70, 2)), // 0.70 fallback
        min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
            .unwrap_or_else(|_| Decimal::new(5, 2)), // 0.05 fallback
        max_hedge_wait_secs: max_wait,
        shares_per_trade: shares,
        max_unhedged_positions: max_unhedged,
        unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
            .unwrap_or_else(|_| Decimal::new(15, 2)), // 0.15 fallback
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
        )
        .await?
    };

    // Initialize executor with default config
    let executor = OrderExecutor::new(client.clone(), Default::default());

    // Run split arbitrage
    run_split_arb(config, client, executor, dry_run).await?;

    Ok(())
}

/// Map coin aliases to preferred series IDs.
#[cfg(test)]
fn map_crypto_coin_to_series_ids(coin_or_series: &str) -> Vec<String> {
    main_commands::crypto::map_crypto_coin_to_series_ids(coin_or_series)
}

/// Handle crypto subcommands
async fn run_crypto_command(cmd: &CryptoCommands) -> Result<()> {
    main_commands::crypto::run_crypto_command(cmd).await
}

/// Handle sports subcommands
async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    main_commands::sports::run_sports_command(cmd).await
}

/// Handle politics subcommands
async fn run_politics_command(cmd: &PoliticsCommands) -> Result<()> {
    main_commands::politics::run_politics_command(cmd).await
}

/// RL strategy commands
#[cfg(feature = "rl")]
async fn run_rl_command(cmd: &RlCommands) -> Result<()> {
    main_commands::rl::run_rl_command(cmd).await
}

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

async fn run_orderbook_history_mode(
    config_path: &str,
    asset_ids: &str,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    lookback_secs: u64,
    levels: usize,
    sample_ms: i64,
    limit: usize,
    max_pages: usize,
    base_url: &str,
    resume_from_db: bool,
) -> Result<()> {
    use ploy::collector::{OrderbookHistoryCollector, OrderbookHistoryCollectorConfig};

    let ids: Vec<String> = asset_ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if ids.is_empty() {
        return Err(PloyError::Validation(
            "--asset-ids must contain at least one token id".to_string(),
        ));
    }

    // Load config for database URL.
    let cfg = AppConfig::load_from(config_path)?;
    let store = PostgresStore::new(&cfg.database.url, 5).await?;

    let mut col_cfg = OrderbookHistoryCollectorConfig::default();
    col_cfg.clob_base_url = base_url.trim_end_matches('/').to_string();
    col_cfg.levels = levels;
    col_cfg.sample_ms = sample_ms;
    col_cfg.page_limit = limit;
    col_cfg.max_pages = max_pages;

    let collector = OrderbookHistoryCollector::new(store.pool().clone(), col_cfg);
    collector.ensure_tables().await?;

    let now_ms: i64 = Utc::now().timestamp_millis();
    let end_ms = end_ms.unwrap_or(now_ms);

    for asset_id in &ids {
        let fallback_start_ms =
            start_ms.unwrap_or_else(|| end_ms.saturating_sub(lookback_secs as i64 * 1000));
        let start_ms = if resume_from_db {
            let last_ms = collector.last_ts_ms_for_asset(asset_id).await?;
            let resumed_ms = last_ms.saturating_add(1);

            // Safety: if there is no history for this asset yet, or the resume point is
            // far in the past, clamp to a sane lookback window instead of requesting
            // from the unix epoch (which can trigger huge backfills / rate limiting).
            if last_ms <= 0 || resumed_ms < fallback_start_ms {
                fallback_start_ms
            } else {
                resumed_ms
            }
        } else {
            fallback_start_ms
        };

        info!(
            asset_id = asset_id.as_str(),
            start_ms,
            end_ms,
            levels,
            sample_ms,
            limit,
            max_pages,
            "starting orderbook-history backfill"
        );

        let inserted = collector.backfill_asset(asset_id, start_ms, end_ms).await?;
        info!(
            asset_id = asset_id.as_str(),
            inserted, "orderbook-history backfill done"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::map_crypto_coin_to_series_ids;

    #[test]
    fn test_map_crypto_coin_to_series_ids_prefers_5m_with_15m_fallback() {
        assert_eq!(
            map_crypto_coin_to_series_ids("BTC"),
            vec!["10684".to_string(), "10192".to_string()]
        );
        assert_eq!(
            map_crypto_coin_to_series_ids("ETH"),
            vec!["10683".to_string(), "10191".to_string()]
        );
        assert_eq!(
            map_crypto_coin_to_series_ids("SOL"),
            vec!["10686".to_string(), "10423".to_string()]
        );
        assert_eq!(
            map_crypto_coin_to_series_ids("XRP"),
            vec!["10685".to_string(), "10422".to_string()]
        );
    }

    #[test]
    fn test_map_crypto_coin_to_series_ids_accepts_raw_series() {
        assert_eq!(
            map_crypto_coin_to_series_ids("10192"),
            vec!["10192".to_string()]
        );
        assert_eq!(
            map_crypto_coin_to_series_ids("10684"),
            vec!["10684".to_string()]
        );
    }
}
