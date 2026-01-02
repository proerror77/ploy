use clap::{Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::io::{stdout, Write};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::adapters::{DisplayQuote, PolymarketClient, QuoteCache};
use crate::error::Result;

#[derive(Parser)]
#[command(name = "ploy")]
#[command(author = "Ploy Team")]
#[command(version = "0.1.0")]
#[command(about = "Polymarket two-leg arbitrage trading bot", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Enable dry run mode (no real orders)
    #[arg(short, long, default_value = "true")]
    pub dry_run: bool,

    /// Market slug to trade
    #[arg(short, long, default_value = "will-btc-go-up-15m")]
    pub market: String,

    /// Config file path
    #[arg(short, long, default_value = "config/default.toml")]
    pub config: String,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the trading bot
    Run,
    /// Watch market data in terminal
    Watch {
        /// Token ID to watch (optional)
        #[arg(short, long)]
        token: Option<String>,
        /// Series ID to watch (e.g., 10423 for SOL 15m)
        #[arg(short, long)]
        series: Option<String>,
    },
    /// Live trading mode with real orders
    Trade {
        /// Series ID to trade (e.g., 10423 for SOL 15m)
        #[arg(short, long)]
        series: String,
        /// Number of shares per leg (default: 20)
        #[arg(long, default_value = "20")]
        shares: u64,
        /// Move percentage threshold (e.g., 0.15 = 15%)
        #[arg(long, default_value = "0.15")]
        move_pct: f64,
        /// Target sum for leg2 (e.g., 0.95)
        #[arg(long, default_value = "0.95")]
        sum_target: f64,
        /// Enable dry-run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
    },
    /// Test market connection
    Test,
    /// Show order book for a token
    Book {
        /// Token ID
        token: String,
    },
    /// Search for markets
    Search {
        /// Search query
        query: String,
    },
    /// Show current active market for a series
    Current {
        /// Series ID (e.g., 10423 for SOL 15m)
        series: String,
    },
    /// Scan all events in a series for arbitrage opportunities
    Scan {
        /// Series ID to scan (e.g., 10423 for SOL 15m)
        #[arg(short, long)]
        series: String,
        /// Sum threshold for opportunity detection (e.g., 0.95)
        #[arg(long, default_value = "0.95")]
        sum_target: f64,
        /// Move percentage threshold for dump detection (e.g., 0.15 = 15%)
        #[arg(long, default_value = "0.15")]
        move_pct: f64,
        /// Continuous monitoring mode (vs one-shot)
        #[arg(long)]
        watch: bool,
    },
    /// Analyze multi-outcome market for arbitrage opportunities
    Analyze {
        /// Event ID to analyze (e.g., from Polymarket URL)
        #[arg(short, long)]
        event: String,
    },
    /// Show account balance and positions
    Account {
        /// Show open orders
        #[arg(long)]
        orders: bool,
        /// Show positions
        #[arg(long)]
        positions: bool,
    },
    /// Calculate expected value for near-settlement betting strategy
    Ev {
        /// Entry price in cents (e.g., 95 for 95¢)
        #[arg(short, long)]
        price: f64,
        /// Estimated true probability percentage (e.g., 97 for 97%)
        #[arg(short = 'P', long)]
        probability: f64,
        /// Hours to settlement (for risk assessment)
        #[arg(short = 'H', long, default_value = "24")]
        hours: f64,
        /// Show full EV table for comparison
        #[arg(long)]
        table: bool,
    },
    /// Analyze market making opportunities for a binary market
    MarketMake {
        /// Token ID for the Yes side
        #[arg(short, long)]
        token: String,
        /// Show detailed Split/Merge analysis
        #[arg(long)]
        detail: bool,
    },
    /// Run momentum strategy (gabagool22 style)
    Momentum {
        /// Symbols to trade (comma-separated: BTCUSDT,ETHUSDT,SOLUSDT)
        #[arg(short, long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT")]
        symbols: String,
        /// Minimum CEX move percentage to trigger (e.g., 0.5 = 0.5%)
        #[arg(long, default_value = "0.5")]
        min_move: f64,
        /// Maximum entry price in cents (e.g., 55 = 55¢)
        #[arg(long, default_value = "55")]
        max_entry: f64,
        /// Minimum edge percentage (e.g., 5 = 5%)
        #[arg(long, default_value = "5")]
        min_edge: f64,
        /// Shares per trade
        #[arg(long, default_value = "100")]
        shares: u64,
        /// Maximum concurrent positions
        #[arg(long, default_value = "5")]
        max_positions: usize,
        /// Take profit percentage (e.g., 20 = 20%)
        #[arg(long, default_value = "20")]
        take_profit: f64,
        /// Stop loss percentage (e.g., 15 = 15%)
        #[arg(long, default_value = "15")]
        stop_loss: f64,
        /// Dry run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
    },
    /// Split arbitrage strategy (gabagool22 分时套利)
    /// Buy UP when cheap, wait for DOWN to be cheap, lock profit
    SplitArb {
        /// Maximum entry price in cents (e.g., 35 = 35¢)
        #[arg(long, default_value = "35")]
        max_entry: f64,
        /// Target total cost in cents (e.g., 70 = 70¢ for 30¢ profit)
        #[arg(long, default_value = "70")]
        target_cost: f64,
        /// Minimum profit margin in cents (e.g., 5 = 5¢)
        #[arg(long, default_value = "5")]
        min_profit: f64,
        /// Maximum wait for hedge in seconds
        #[arg(long, default_value = "900")]
        max_wait: u64,
        /// Shares per trade
        #[arg(long, default_value = "100")]
        shares: u64,
        /// Maximum unhedged positions
        #[arg(long, default_value = "3")]
        max_unhedged: usize,
        /// Stop loss percentage for unhedged exit (e.g., 15 = 15%)
        #[arg(long, default_value = "15")]
        stop_loss: f64,
        /// Series IDs to monitor (comma-separated)
        #[arg(long, default_value = "10423,10191,41")]
        series: String,
        /// Dry run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
    },
    /// Claude AI agent for trading assistance
    Agent {
        /// Agent mode: advisory, autonomous
        #[arg(short = 'M', long, default_value = "advisory")]
        mode: String,
        /// Market/event to analyze (optional)
        #[arg(short = 'e', long)]
        market: Option<String>,
        /// Maximum trade size in USDC (for autonomous mode)
        #[arg(long, default_value = "50")]
        max_trade: f64,
        /// Maximum total exposure in USDC (for autonomous mode)
        #[arg(long, default_value = "200")]
        max_exposure: f64,
        /// Enable trading (autonomous mode only)
        #[arg(long)]
        enable_trading: bool,
        /// Interactive chat mode
        #[arg(long)]
        chat: bool,
    },
    /// Run the TUI dashboard
    Dashboard {
        /// Series ID to monitor (optional)
        #[arg(short, long)]
        series: Option<String>,
        /// Run with demo data
        #[arg(long)]
        demo: bool,
    },
}

/// Terminal UI for monitoring
pub struct TerminalUI {
    quote_cache: QuoteCache,
    client: PolymarketClient,
    running: Arc<RwLock<bool>>,
}

impl TerminalUI {
    pub fn new(quote_cache: QuoteCache, client: PolymarketClient) -> Self {
        Self {
            quote_cache,
            client,
            running: Arc::new(RwLock::new(true)),
        }
    }

    /// Run the terminal UI
    pub async fn run(&self) -> Result<()> {
        terminal::enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

        let result = self.run_loop(&mut stdout).await;

        // Cleanup
        execute!(stdout, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()?;

        result
    }

    async fn run_loop(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        let mut last_update = std::time::Instant::now();

        loop {
            // Check for key events
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        _ => {}
                    }
                }
            }

            // Update display every 500ms
            if last_update.elapsed() >= Duration::from_millis(500) {
                self.render(stdout).await?;
                last_update = std::time::Instant::now();
            }
        }

        Ok(())
    }

    async fn render(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        execute!(
            stdout,
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;

        // Header
        self.print_header(stdout)?;

        // Quote data
        execute!(stdout, cursor::MoveTo(0, 3))?;
        self.print_quotes(stdout).await?;

        // Status bar
        let (_, rows) = terminal::size()?;
        execute!(stdout, cursor::MoveTo(0, rows - 2))?;
        self.print_status_bar(stdout)?;

        stdout.flush()?;
        Ok(())
    }

    fn print_header(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        execute!(
            stdout,
            SetForegroundColor(Color::Cyan),
            Print("╔══════════════════════════════════════════════════════════════╗\n"),
            Print("║          PLOY - Polymarket Trading Bot [DRY RUN]             ║\n"),
            Print("╚══════════════════════════════════════════════════════════════╝"),
            ResetColor
        )?;
        Ok(())
    }

    async fn print_quotes(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        let (up_quote, down_quote) = self.quote_cache.get_quotes().await;

        execute!(stdout, Print("\n"))?;

        // UP side
        execute!(
            stdout,
            SetForegroundColor(Color::Green),
            Print("  ▲ UP   "),
            ResetColor
        )?;

        if let Some(ref q) = up_quote {
            let spread = q.best_ask - q.best_bid;
            let spread_bps = if q.best_bid > Decimal::ZERO {
                (spread / q.best_bid * Decimal::from(10000)).round()
            } else {
                Decimal::ZERO
            };

            execute!(
                stdout,
                Print(format!(
                    "Bid: {:.4}  Ask: {:.4}  Spread: {:.0} bps  Size: {:.2}/{:.2}\n",
                    q.best_bid, q.best_ask, spread_bps, q.bid_size, q.ask_size
                ))
            )?;
        } else {
            execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print("No data\n"),
                ResetColor
            )?;
        }

        // DOWN side
        execute!(
            stdout,
            SetForegroundColor(Color::Red),
            Print("  ▼ DOWN "),
            ResetColor
        )?;

        if let Some(ref q) = down_quote {
            let spread = q.best_ask - q.best_bid;
            let spread_bps = if q.best_bid > Decimal::ZERO {
                (spread / q.best_bid * Decimal::from(10000)).round()
            } else {
                Decimal::ZERO
            };

            execute!(
                stdout,
                Print(format!(
                    "Bid: {:.4}  Ask: {:.4}  Spread: {:.0} bps  Size: {:.2}/{:.2}\n",
                    q.best_bid, q.best_ask, spread_bps, q.bid_size, q.ask_size
                ))
            )?;
        } else {
            execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print("No data\n"),
                ResetColor
            )?;
        }

        // Price sum
        execute!(stdout, Print("\n"))?;
        if let (Some(ref up), Some(ref down)) = (up_quote, down_quote) {
            let sum = up.best_ask + down.best_ask;
            let sum_color = if sum <= dec!(0.95) {
                Color::Green
            } else if sum <= dec!(1.00) {
                Color::Yellow
            } else {
                Color::Red
            };

            execute!(
                stdout,
                Print("  Sum (Ask+Ask): "),
                SetForegroundColor(sum_color),
                Print(format!("{:.4}", sum)),
                ResetColor,
                Print("  Target: ≤0.95 for Leg2\n")
            )?;
        }

        // Strategy status
        execute!(
            stdout,
            Print("\n"),
            SetForegroundColor(Color::Yellow),
            Print("  Strategy: "),
            ResetColor,
            Print("IDLE - Waiting for dump signal\n")
        )?;

        Ok(())
    }

    fn print_status_bar(&self, stdout: &mut std::io::Stdout) -> Result<()> {
        let now = chrono::Local::now().format("%H:%M:%S");
        execute!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print("─".repeat(66)),
            Print("\n"),
            Print(format!("  {} │ Press 'q' to quit │ DRY RUN MODE", now)),
            ResetColor
        )?;
        Ok(())
    }
}

/// Test market connection
pub async fn test_connection(client: &PolymarketClient) -> Result<()> {
    println!("Testing connection to Polymarket CLOB...\n");

    // Test markets endpoint
    print!("  Searching markets... ");
    stdout().flush()?;

    match client.search_markets("btc").await {
        Ok(markets) => {
            println!(
                "{}",
                format_args!("\x1b[32mOK\x1b[0m ({} markets found)", markets.len())
            );

            if let Some(market) = markets.first() {
                println!("\n  Sample market:");
                println!("    Condition ID: {}", market.condition_id);
                if let Some(q) = &market.question {
                    println!("    Question: {}", q);
                }
                println!("    Active: {}", market.active);
            }
        }
        Err(e) => {
            println!("\x1b[31mFAILED\x1b[0m");
            println!("    Error: {}", e);
        }
    }

    println!();
    Ok(())
}

/// Show order book
pub async fn show_order_book(client: &PolymarketClient, token_id: &str) -> Result<()> {
    println!("Fetching order book for token: {}\n", token_id);

    match client.get_order_book(token_id).await {
        Ok(book) => {
            println!("  Asset ID: {}", book.asset_id);
            if let Some(ts) = &book.timestamp {
                println!("  Timestamp: {}", ts);
            }

            println!("\n  \x1b[32mBids (Buy Orders):\x1b[0m");
            if book.bids.is_empty() {
                println!("    (none)");
            } else {
                for (i, bid) in book.bids.iter().take(5).enumerate() {
                    println!("    {}. Price: {} Size: {}", i + 1, bid.price, bid.size);
                }
            }

            println!("\n  \x1b[31mAsks (Sell Orders):\x1b[0m");
            if book.asks.is_empty() {
                println!("    (none)");
            } else {
                for (i, ask) in book.asks.iter().take(5).enumerate() {
                    println!("    {}. Price: {} Size: {}", i + 1, ask.price, ask.size);
                }
            }
        }
        Err(e) => {
            println!("\x1b[31mError:\x1b[0m {}", e);
        }
    }

    println!();
    Ok(())
}

/// Search markets
pub async fn search_markets(client: &PolymarketClient, query: &str) -> Result<()> {
    println!("Searching for: \"{}\"\n", query);

    match client.search_markets(query).await {
        Ok(markets) => {
            if markets.is_empty() {
                println!("  No markets found.");
            } else {
                println!("  Found {} markets:\n", markets.len());
                for (i, market) in markets.iter().take(10).enumerate() {
                    println!("  {}. {}", i + 1, market.condition_id);
                    if let Some(q) = &market.question {
                        println!("     {}", q);
                    }
                    if let Some(slug) = &market.slug {
                        println!("     Slug: {}", slug);
                    }
                    println!("     Active: {}", market.active);
                    println!();
                }
            }
        }
        Err(e) => {
            println!("\x1b[31mError:\x1b[0m {}", e);
        }
    }

    Ok(())
}

/// Show current active market for a series
pub async fn show_current_market(client: &PolymarketClient, series_id: &str) -> Result<()> {
    println!("Fetching current market for series: {}\n", series_id);

    // Get series info first
    match client.get_series(series_id).await {
        Ok(series) => {
            println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
            println!("\x1b[36m║  Series: {:<52} ║\x1b[0m", series.ticker.as_deref().unwrap_or("Unknown"));
            println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

            println!("  Title: {}", series.title.as_deref().unwrap_or("N/A"));
            println!("  Recurrence: {}", series.recurrence.as_deref().unwrap_or("N/A"));
            if let Some(vol) = series.volume {
                println!("  Volume: ${:.2}", vol);
            }
            if let Some(liq) = series.liquidity {
                println!("  Liquidity: ${:.2}", liq);
            }

            // Find current active events
            let active_events: Vec<_> = series.events.iter()
                .filter(|e| !e.closed)
                .take(3)
                .collect();

            if active_events.is_empty() {
                println!("\n\x1b[33m  No active events found.\x1b[0m");
            } else {
                println!("\n\x1b[32m  Active Events:\x1b[0m");
                for event in &active_events {
                    println!("\n  Event: {}", event.title.as_deref().unwrap_or("Unknown"));
                    println!("    ID: {}", event.id);
                    if let Some(slug) = &event.slug {
                        println!("    Slug: {}", slug);
                    }
                    if let Some(end) = &event.end_date {
                        println!("    End: {}", end);
                    }

                    // Try to get tokens for this event
                    if let Ok(event_details) = client.get_event_details(&event.id).await {
                        if let Some(market) = event_details.markets.first() {
                            if let Some(cid) = &market.condition_id {
                                println!("    Condition ID: {}", cid);

                                // Get tokens from CLOB
                                if let Ok(clob_market) = client.get_market(cid).await {
                                    println!("\n    \x1b[32mTokens:\x1b[0m");
                                    for token in &clob_market.tokens {
                                        println!("      {} ({}): Price={}",
                                            token.outcome,
                                            &token.token_id[..20.min(token.token_id.len())],
                                            token.price.as_deref().unwrap_or("N/A")
                                        );
                                    }

                                    // Show order book for first token
                                    if let Some(first_token) = clob_market.tokens.first() {
                                        println!("\n    \x1b[33mOrder Book ({}):\x1b[0m", first_token.outcome);
                                        if let Ok(book) = client.get_order_book(&first_token.token_id).await {
                                            if let Some(bid) = book.bids.first() {
                                                println!("      Best Bid: {} @ {}", bid.size, bid.price);
                                            }
                                            if let Some(ask) = book.asks.first() {
                                                println!("      Best Ask: {} @ {}", ask.size, ask.price);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("\x1b[31mError:\x1b[0m {}", e);
        }
    }

    println!();
    Ok(())
}

/// Analyze a multi-outcome market for arbitrage opportunities
pub async fn analyze_multi_outcome(client: &PolymarketClient, event_id: &str) -> Result<()> {
    use crate::strategy::{fetch_multi_outcome_event, ArbitrageType, OutcomeDirection};

    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║         Multi-Outcome Market Arbitrage Analyzer              ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("Fetching event: {}\n", event_id);

    let monitor = fetch_multi_outcome_event(client, event_id).await?;

    println!("\x1b[32mEvent:\x1b[0m {}", monitor.event_title);
    println!("\x1b[32mOutcomes:\x1b[0m {}\n", monitor.outcome_count());

    // Print summary table
    println!("┌────────────────────┬───────────┬───────────┬──────────┬───────────┐");
    println!("│      Outcome       │  Yes (¢)  │  No (¢)   │  Spread  │  Prob %   │");
    println!("├────────────────────┼───────────┼───────────┼──────────┼───────────┤");

    for summary in monitor.summary() {
        let direction_icon = match summary.direction {
            Some(OutcomeDirection::Up) => "↑",
            Some(OutcomeDirection::Down) => "↓",
            None => " ",
        };

        let name = format!("{} {}", direction_icon,
            summary.name.chars().take(16).collect::<String>());

        let yes_str = summary.yes_price
            .map(|p| format!("{:.1}", p * dec!(100)))
            .unwrap_or_else(|| "-".to_string());

        let no_str = summary.no_price
            .map(|p| format!("{:.1}", p * dec!(100)))
            .unwrap_or_else(|| "-".to_string());

        let spread_str = summary.spread
            .map(|s| format!("{:.1}%", s * dec!(100)))
            .unwrap_or_else(|| "-".to_string());

        let prob_str = summary.implied_prob_pct
            .map(|p| format!("{:.1}%", p))
            .unwrap_or_else(|| "-".to_string());

        // Color based on spread
        let spread_color = match summary.spread {
            Some(s) if s > dec!(0.03) => "\x1b[31m", // Red for high spread
            Some(s) if s > dec!(0.01) => "\x1b[33m", // Yellow for medium
            _ => "\x1b[32m", // Green for low/none
        };

        println!(
            "│ {:<18} │ {:>9} │ {:>9} │ {}{:>8}\x1b[0m │ {:>9} │",
            name, yes_str, no_str, spread_color, spread_str, prob_str
        );
    }
    println!("└────────────────────┴───────────┴───────────┴──────────┴───────────┘\n");

    // Find and display arbitrage opportunities
    let arbitrages = monitor.find_all_arbitrage();

    if arbitrages.is_empty() {
        println!("\x1b[33m⚠ No arbitrage opportunities detected.\x1b[0m\n");
    } else {
        println!("\x1b[32m✓ Found {} arbitrage opportunities:\x1b[0m\n", arbitrages.len());

        for (i, arb) in arbitrages.iter().enumerate() {
            match &arb.arb_type {
                ArbitrageType::MonotonicityViolation {
                    outcome_a,
                    outcome_b,
                    prob_a,
                    prob_b,
                    expected_relationship,
                } => {
                    println!("\x1b[31m{}. MONOTONICITY VIOLATION\x1b[0m", i + 1);
                    println!("   {} ({:.1}%) vs {} ({:.1}%)",
                        outcome_a, prob_a * dec!(100),
                        outcome_b, prob_b * dec!(100));
                    println!("   \x1b[33m→ {}\x1b[0m", expected_relationship);
                    println!("   Estimated profit: {:.2}%\n", arb.profit_per_dollar * dec!(100));
                }
                ArbitrageType::SpreadArbitrage {
                    outcome,
                    yes_price,
                    no_price,
                    profit,
                } => {
                    println!("\x1b[32m{}. SPREAD ARBITRAGE\x1b[0m", i + 1);
                    println!("   {}: Yes={:.1}¢ + No={:.1}¢ = {:.1}¢ < 100¢",
                        outcome,
                        yes_price * dec!(100),
                        no_price * dec!(100),
                        (yes_price + no_price) * dec!(100));
                    println!("   Profit per $1: ${:.4}\n", profit);
                }
                ArbitrageType::CrossOutcomeArbitrage {
                    description,
                    outcomes,
                    estimated_profit,
                } => {
                    println!("\x1b[35m{}. CROSS-OUTCOME ARBITRAGE\x1b[0m", i + 1);
                    println!("   {}", description);
                    println!("   Outcomes: {:?}", outcomes);
                    println!("   Estimated profit: {:.2}%\n", estimated_profit * dec!(100));
                }
            }
        }
    }

    // Summary
    println!("\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");
    println!("Analysis complete. {} outcomes analyzed.", monitor.outcome_count());

    Ok(())
}

/// Show account balance, positions, and orders
pub async fn show_account(client: &PolymarketClient, show_orders: bool, show_positions: bool) -> Result<()> {
    println!("\x1b[36m");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    POLYMARKET ACCOUNT                        ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\x1b[0m");

    // Get USDC balance
    print!("  Fetching balance... ");
    stdout().flush()?;

    match client.get_usdc_balance().await {
        Ok(balance) => {
            println!("\x1b[32mOK\x1b[0m");
            println!("\n  \x1b[33m💰 USDC Balance: ${:.2}\x1b[0m\n", balance);
        }
        Err(e) => {
            println!("\x1b[31mFAILED\x1b[0m");
            println!("    Error: {}", e);
            println!("\n  \x1b[31mNote: Balance API requires authentication.\x1b[0m");
            println!("  Make sure POLYMARKET_PRIVATE_KEY is set in environment.\n");
        }
    }

    // Show positions if requested
    if show_positions {
        print!("  Fetching positions... ");
        stdout().flush()?;

        match client.get_positions().await {
            Ok(positions) => {
                println!("\x1b[32mOK\x1b[0m");
                if positions.is_empty() {
                    println!("\n  \x1b[33m📊 Positions: None\x1b[0m\n");
                } else {
                    println!("\n  \x1b[33m📊 Positions ({}):\x1b[0m", positions.len());
                    for (i, pos) in positions.iter().enumerate() {
                        let size: f64 = pos.size.parse().unwrap_or(0.0);
                        if size.abs() > 0.0001 {
                            println!("    {}. Token: {}", i + 1,
                                pos.token_id.as_ref().unwrap_or(&pos.asset_id));
                            println!("       Size: {} shares", pos.size);
                            if let Some(avg) = &pos.avg_price {
                                println!("       Avg Price: ${}", avg);
                            }
                            if let Some(cur) = &pos.cur_price {
                                println!("       Current Price: ${}", cur);
                            }
                            if let Some(val) = pos.market_value() {
                                println!("       Market Value: \x1b[32m${:.2}\x1b[0m", val);
                            }
                            println!();
                        }
                    }
                }
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    // Show open orders if requested
    if show_orders {
        print!("  Fetching open orders... ");
        stdout().flush()?;

        match client.get_open_orders().await {
            Ok(orders) => {
                println!("\x1b[32mOK\x1b[0m");
                if orders.is_empty() {
                    println!("\n  \x1b[33m📋 Open Orders: None\x1b[0m\n");
                } else {
                    println!("\n  \x1b[33m📋 Open Orders ({}):\x1b[0m", orders.len());
                    for (i, order) in orders.iter().enumerate() {
                        println!("    {}. Order ID: {}", i + 1, order.id);
                        println!("       Token: {}",
                            order.asset_id.as_deref().unwrap_or("N/A"));
                        println!("       Side: {} @ ${}",
                            order.side.as_deref().unwrap_or("N/A"),
                            order.price.as_deref().unwrap_or("N/A"));
                        println!("       Size: {} (filled: {})",
                            order.original_size.as_deref().unwrap_or("0"),
                            order.size_matched.as_deref().unwrap_or("0"));
                        println!("       Status: {}", order.status);
                        println!();
                    }
                }
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    // If neither flag specified, show summary
    if !show_orders && !show_positions {
        // Try to get account summary
        print!("  Fetching account summary... ");
        stdout().flush()?;

        match client.get_account_summary().await {
            Ok(summary) => {
                println!("\x1b[32mOK\x1b[0m\n");
                println!("  \x1b[36m─────────────────────────────────────\x1b[0m");
                println!("  Total Equity:     \x1b[32m${:.2}\x1b[0m", summary.total_equity);
                println!("  USDC Balance:     ${:.2}", summary.usdc_balance);
                println!("  Position Value:   ${:.2} ({} positions)",
                    summary.position_value, summary.position_count);
                println!("  Open Orders:      ${:.2} ({} orders)",
                    summary.open_order_value, summary.open_order_count);
                println!("  \x1b[36m─────────────────────────────────────\x1b[0m\n");
            }
            Err(e) => {
                println!("\x1b[31mFAILED\x1b[0m");
                println!("    Error: {}", e);
            }
        }
    }

    println!("\x1b[90mTip: Use --orders or --positions for detailed views\x1b[0m\n");
    Ok(())
}

/// Calculate expected value for near-settlement betting
pub async fn calculate_ev(price_cents: f64, probability_pct: f64, hours: f64, show_table: bool) -> Result<()> {
    use crate::strategy::{ExpectedValue, analyze_near_settlement, generate_ev_table, POLYMARKET_FEE_RATE};
    use rust_decimal::prelude::FromPrimitive;

    let price = Decimal::from_f64(price_cents / 100.0).unwrap_or(dec!(0.95));
    let prob = Decimal::from_f64(probability_pct / 100.0).unwrap_or(dec!(0.97));

    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║          Expected Value Calculator (Near-Settlement)         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Near-settlement analysis
    let analysis = analyze_near_settlement(price, prob, hours);

    println!("\x1b[33m📊 Input Parameters:\x1b[0m");
    println!("   Entry Price:        {:.1}¢ per Yes share", price_cents);
    println!("   True Probability:   {:.1}%", probability_pct);
    println!("   Hours to Settlement: {:.1}h", hours);
    println!("   Platform Fee:       {:.1}%\n", POLYMARKET_FEE_RATE * dec!(100));

    println!("\x1b[33m📈 Expected Value Analysis:\x1b[0m");
    println!("   Gross EV:           ${:.4} per share", analysis.ev_analysis.gross_ev);
    println!("   Net EV (after fee): ${:.4} per share", analysis.ev_analysis.net_ev);
    println!("   ROI:                {:.2}%", analysis.ev_analysis.roi * dec!(100));
    println!("   Breakeven Prob:     {:.1}%\n", analysis.ev_analysis.breakeven_prob * dec!(100));

    println!("\x1b[33m🎯 Kelly Criterion:\x1b[0m");
    println!("   Optimal Bet Size:   {:.1}% of bankroll\n", analysis.ev_analysis.kelly_fraction * dec!(100));

    println!("\x1b[33m⚠️  Risk Assessment:\x1b[0m");
    println!("   Risk Level:         {}", analysis.risk_level);
    println!("   Recommendation:     {}\n", analysis.recommendation);

    // Scenario analysis
    println!("\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");
    println!("\x1b[33m📊 Scenario Analysis ($100 bet):\x1b[0m\n");

    let bet_size = dec!(100);
    let shares = bet_size / price;
    let win_profit = shares * (Decimal::ONE - price) * (Decimal::ONE - POLYMARKET_FEE_RATE);
    let lose_loss = bet_size;

    println!("   If WIN:  +${:.2} profit ({:.0} shares × {:.1}¢ profit × {:.0}% fee retained)",
        win_profit, shares, (Decimal::ONE - price) * dec!(100), (Decimal::ONE - POLYMARKET_FEE_RATE) * dec!(100));
    println!("   If LOSE: -${:.2} loss (full bet amount)\n", lose_loss);

    let ev_dollars = prob * win_profit - (Decimal::ONE - prob) * lose_loss;
    if ev_dollars > Decimal::ZERO {
        println!("   \x1b[32m✓ Expected Value: +${:.2}\x1b[0m", ev_dollars);
    } else {
        println!("   \x1b[31m✗ Expected Value: ${:.2}\x1b[0m", ev_dollars);
    }

    // Show table if requested
    if show_table {
        println!("\n\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[33m📋 EV Table (Net EV per $1 bet):\x1b[0m\n");

        // Header
        print!("  Price ");
        for prob_pct in [92, 94, 95, 96, 97, 98, 99].iter() {
            print!("  {:>5}%", prob_pct);
        }
        println!();
        println!("  {}", "─".repeat(58));

        // Rows
        for price_cents in [90, 92, 94, 95, 96, 97, 98, 99].iter() {
            let p = Decimal::from_f64(*price_cents as f64 / 100.0).unwrap();
            print!("  {:>4}¢ ", price_cents);

            for prob_pct in [92, 94, 95, 96, 97, 98, 99].iter() {
                let pr = Decimal::from_f64(*prob_pct as f64 / 100.0).unwrap();
                let ev = ExpectedValue::calculate(p, pr, None);
                if ev.is_positive_ev {
                    print!(" \x1b[32m{:>6.2}%\x1b[0m", ev.roi * dec!(100));
                } else {
                    print!(" \x1b[31m{:>6.2}%\x1b[0m", ev.roi * dec!(100));
                }
            }
            println!();
        }

        println!("\n  \x1b[32mGreen\x1b[0m = +EV opportunity  \x1b[31mRed\x1b[0m = -EV (avoid)");
    }

    println!("\n\x1b[90mTip: Use --table to see full comparison matrix\x1b[0m\n");
    Ok(())
}

/// Analyze market making opportunities
pub async fn analyze_market_making(client: &PolymarketClient, token_id: &str, show_detail: bool) -> Result<()> {
    use crate::strategy::{MarketMakingConfig, analyze_market_making_opportunity, SplitMergeType};
    use rust_decimal::prelude::FromStr;

    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║              Market Making Opportunity Analyzer              ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("Fetching orderbook for token: {}\n", token_id);

    // Get orderbook for Yes token
    let book = client.get_order_book(token_id).await?;

    let yes_bid = book.bids.first()
        .and_then(|b| Decimal::from_str(&b.price).ok())
        .unwrap_or(dec!(0.5));
    let yes_ask = book.asks.first()
        .and_then(|a| Decimal::from_str(&a.price).ok())
        .unwrap_or(dec!(0.5));

    // For No side, assume complement (this is simplified - in reality need No token orderbook)
    let no_bid = Decimal::ONE - yes_ask;
    let no_ask = Decimal::ONE - yes_bid;

    let config = MarketMakingConfig::default();
    let opportunity = analyze_market_making_opportunity(yes_bid, yes_ask, no_bid, no_ask, &config);

    println!("\x1b[33m📊 Current Market:\x1b[0m");
    println!("   Yes Bid/Ask:  {:.3} / {:.3}  (Spread: {:.1}%)",
        yes_bid, yes_ask, (yes_ask - yes_bid) * dec!(100));
    println!("   No Bid/Ask:   {:.3} / {:.3}  (Spread: {:.1}%)",
        no_bid, no_ask, (no_ask - no_bid) * dec!(100));
    println!("   Combined Ask: {:.3} ({:.1}% over $1.00)\n",
        opportunity.current_spread,
        (opportunity.current_spread - Decimal::ONE) * dec!(100));

    // Split/Merge opportunity
    println!("\x1b[33m🔄 Split/Merge Analysis:\x1b[0m");
    if let Some(ref sm) = opportunity.split_merge {
        match sm.opportunity_type {
            SplitMergeType::SplitAndSell => {
                println!("   \x1b[32m✓ SPLIT & SELL OPPORTUNITY!\x1b[0m");
                println!("   Yes_bid + No_bid = {:.3} > $1.00", sm.yes_bid + sm.no_bid);
                println!("   Gross Profit: ${:.4} per $1 split", sm.profit_per_dollar);
                println!("   Net Profit:   ${:.4} (after slippage)\n", sm.net_profit);
                println!("   \x1b[36mAction:\x1b[0m Split $1 USDC → 1 Yes + 1 No → Sell both → Profit");
            }
            SplitMergeType::BuyAndMerge => {
                println!("   \x1b[32m✓ BUY & MERGE OPPORTUNITY!\x1b[0m");
                println!("   Yes_ask + No_ask = {:.3} < $1.00", sm.yes_ask + sm.no_ask);
                println!("   Gross Profit: ${:.4} per pair", sm.profit_per_dollar);
                println!("   Net Profit:   ${:.4} (after slippage)\n", sm.net_profit);
                println!("   \x1b[36mAction:\x1b[0m Buy 1 Yes + 1 No → Merge → Redeem $1 → Profit");
            }
        }
    } else {
        println!("   No immediate Split/Merge opportunity");
        println!("   Yes_bid + No_bid = {:.3}", yes_bid + no_bid);
        println!("   Yes_ask + No_ask = {:.3}\n", yes_ask + no_ask);
    }

    // Market making strategy
    println!("\x1b[33m📈 Market Making Strategy:\x1b[0m");
    println!("   Target Spread Range: {:.1}% - {:.1}%",
        (config.target_spread_min - Decimal::ONE) * dec!(100),
        (config.target_spread_max - Decimal::ONE) * dec!(100));
    println!("   Current Spread:      {:.1}% ({})",
        (opportunity.current_spread - Decimal::ONE) * dec!(100),
        if opportunity.spread_in_range { "\x1b[32mIN RANGE\x1b[0m" } else { "\x1b[33mOUT OF RANGE\x1b[0m" });

    match &opportunity.recommendation {
        crate::strategy::MarketMakingAction::PostBothSides { yes_quote, no_quote } => {
            println!("\n   \x1b[32mRecommendation: POST BOTH SIDES\x1b[0m");
            println!("   Post Yes: Bid {:.3} / Ask {:.3}", yes_quote.0, yes_quote.1);
            println!("   Post No:  Bid {:.3} / Ask {:.3}", no_quote.0, no_quote.1);
            println!("   Estimated Profit: ${:.2} if both sides fill", opportunity.estimated_profit);
        }
        crate::strategy::MarketMakingAction::SplitAndSell => {
            println!("\n   \x1b[32mRecommendation: SPLIT & SELL\x1b[0m");
            println!("   Execute Split/Merge arbitrage immediately");
        }
        crate::strategy::MarketMakingAction::BuyAndMerge => {
            println!("\n   \x1b[32mRecommendation: BUY & MERGE\x1b[0m");
            println!("   Execute Split/Merge arbitrage immediately");
        }
        crate::strategy::MarketMakingAction::Wait { reason } => {
            println!("\n   \x1b[33mRecommendation: WAIT\x1b[0m");
            println!("   Reason: {}", reason);
        }
        crate::strategy::MarketMakingAction::Rebalance { sell_side, buy_side } => {
            println!("\n   \x1b[33mRecommendation: REBALANCE\x1b[0m");
            println!("   Sell {} / Buy {}", sell_side, buy_side);
        }
    }

    if show_detail {
        println!("\n\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");
        println!("\x1b[33m📚 Professional MM Strategy Guide:\x1b[0m\n");
        println!("   1. \x1b[36mSplit & Quote:\x1b[0m Split $1 USDC → 1 Yes + 1 No");
        println!("   2. \x1b[36mPost Both Sides:\x1b[0m Sell Yes @ markup, Sell No @ markup");
        println!("   3. \x1b[36mTarget Spread:\x1b[0m Yes_ask + No_ask = 1.02 to 1.08");
        println!("   4. \x1b[36mRebalance:\x1b[0m When one side fills, buy opposite to hedge");
        println!("   5. \x1b[36mMerge Exit:\x1b[0m Merge remaining inventory back to USDC\n");

        println!("   \x1b[31mKey Pitfalls to Avoid:\x1b[0m");
        println!("   • Don't hold naked exposure (always hedge)");
        println!("   • Avoid positions near settlement deadline");
        println!("   • Rebalance promptly when inventory skews");
        println!("   • Account for slippage on large orders");
        println!("   • Monitor for news that could move prices");
    }

    println!("\n\x1b[90mTip: Use --detail for full strategy guide\x1b[0m\n");
    Ok(())
}
