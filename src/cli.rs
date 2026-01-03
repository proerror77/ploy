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
        /// Agent mode: advisory, autonomous, sports
        #[arg(short = 'M', long, default_value = "advisory")]
        mode: String,
        /// Market/event to analyze (optional)
        #[arg(short = 'e', long)]
        market: Option<String>,
        /// Sports event URL (for sports mode)
        /// Example: https://polymarket.com/event/nba-phi-dal-2026-01-01
        #[arg(long)]
        sports_url: Option<String>,
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

    /// Collect synchronized data for lag analysis
    Collect {
        /// Binance symbols to track (comma-separated: BTCUSDT,ETHUSDT,SOLUSDT)
        #[arg(short, long, default_value = "BTCUSDT")]
        symbols: String,
        /// Polymarket market slugs to track (comma-separated)
        #[arg(short, long)]
        markets: Option<String>,
        /// Duration to collect in minutes (0 = indefinite)
        #[arg(short, long, default_value = "0")]
        duration: u64,
    },

    /// Crypto market strategies (BTC, ETH, SOL UP/DOWN)
    #[command(subcommand)]
    Crypto(CryptoCommands),

    /// Sports market strategies (NBA, NFL, etc.)
    #[command(subcommand)]
    Sports(SportsCommands),

    /// Political prediction markets (elections, approval ratings)
    #[command(subcommand)]
    Politics(PoliticsCommands),

    /// Reinforcement learning strategies (requires 'rl' feature)
    #[cfg(feature = "rl")]
    #[command(subcommand)]
    Rl(RlCommands),
}

/// Crypto market subcommands
#[derive(Subcommand, Debug)]
pub enum CryptoCommands {
    /// Split arbitrage on crypto UP/DOWN markets
    SplitArb {
        /// Maximum entry price in cents (e.g., 35 = 35¢)
        #[arg(long, default_value = "35")]
        max_entry: f64,
        /// Target total cost in cents (e.g., 70 = 70¢)
        #[arg(long, default_value = "70")]
        target_cost: f64,
        /// Minimum profit margin in cents
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
        /// Stop loss percentage for unhedged exit
        #[arg(long, default_value = "15")]
        stop_loss: f64,
        /// Coins to monitor (comma-separated: BTC,ETH,SOL)
        #[arg(long, default_value = "SOL,ETH,BTC")]
        coins: String,
        /// Dry run mode
        #[arg(long)]
        dry_run: bool,
    },
    /// Monitor crypto markets for opportunities
    Monitor {
        /// Coins to monitor (comma-separated)
        #[arg(long, default_value = "SOL,ETH,BTC")]
        coins: String,
    },
}

/// Sports market subcommands
#[derive(Subcommand, Debug)]
pub enum SportsCommands {
    /// Split arbitrage on sports markets
    SplitArb {
        /// Maximum entry price in cents
        #[arg(long, default_value = "45")]
        max_entry: f64,
        /// Target total cost in cents
        #[arg(long, default_value = "92")]
        target_cost: f64,
        /// Minimum profit margin in cents
        #[arg(long, default_value = "3")]
        min_profit: f64,
        /// Maximum wait for hedge in seconds
        #[arg(long, default_value = "3600")]
        max_wait: u64,
        /// Shares per trade
        #[arg(long, default_value = "100")]
        shares: u64,
        /// Maximum unhedged positions
        #[arg(long, default_value = "5")]
        max_unhedged: usize,
        /// Stop loss percentage
        #[arg(long, default_value = "20")]
        stop_loss: f64,
        /// Leagues to monitor (comma-separated: NBA,NFL)
        #[arg(long, default_value = "NBA,NFL")]
        leagues: String,
        /// Dry run mode
        #[arg(long)]
        dry_run: bool,
    },
    /// Monitor sports markets for opportunities
    Monitor {
        /// Leagues to monitor (comma-separated)
        #[arg(long, default_value = "NBA,NFL")]
        leagues: String,
    },
    /// DraftKings odds comparison and arbitrage
    Draftkings {
        /// Sport to analyze (nba, nfl, nhl, mlb)
        #[arg(long, default_value = "nba")]
        sport: String,
        /// Minimum edge threshold (percentage)
        #[arg(long, default_value = "5.0")]
        min_edge: f64,
        /// Show all games (not just those with edge)
        #[arg(long)]
        all: bool,
    },
    /// Analyze a specific game with DraftKings comparison
    Analyze {
        /// Polymarket event URL
        #[arg(long)]
        url: Option<String>,
        /// Team 1 name (if not using URL)
        #[arg(long)]
        team1: Option<String>,
        /// Team 2 name (if not using URL)
        #[arg(long)]
        team2: Option<String>,
    },
    /// Polymarket sports markets (live NBA, NFL betting)
    Polymarket {
        /// League to filter (nba, nfl, all)
        #[arg(long, default_value = "all")]
        league: String,
        /// Search for specific team or matchup
        #[arg(long)]
        search: Option<String>,
        /// Compare with DraftKings odds for edge detection
        #[arg(long)]
        compare_dk: bool,
        /// Minimum edge percentage for alerts
        #[arg(long, default_value = "5.0")]
        min_edge: f64,
        /// Show live in-play games with real-time scores
        #[arg(long)]
        live: bool,
    },
    /// Full decision chain: Grok -> Claude -> DraftKings -> Polymarket
    Chain {
        /// Team 1 name
        #[arg(long)]
        team1: String,
        /// Team 2 name
        #[arg(long)]
        team2: String,
        /// Sport (nba, nfl)
        #[arg(long, default_value = "nba")]
        sport: String,
        /// Execute bet on Polymarket (requires wallet)
        #[arg(long)]
        execute: bool,
        /// Bet amount in USDC
        #[arg(long, default_value = "10")]
        amount: f64,
    },
    /// Live edge scanner - continuously monitor live games for arbitrage
    LiveScan {
        /// Sport to scan (nba, nfl)
        #[arg(long, default_value = "nba")]
        sport: String,
        /// Minimum edge percentage to alert
        #[arg(long, default_value = "3.0")]
        min_edge: f64,
        /// Scan interval in seconds
        #[arg(long, default_value = "30")]
        interval: u64,
        /// Include spreads in scan
        #[arg(long)]
        spreads: bool,
        /// Include moneyline in scan
        #[arg(long)]
        moneyline: bool,
        /// Include player props in scan
        #[arg(long)]
        props: bool,
        /// Alert sound on edge detection
        #[arg(long)]
        alert: bool,
    },
}

/// Political market subcommands
#[derive(Subcommand, Debug)]
pub enum PoliticsCommands {
    /// Show political prediction markets
    Markets {
        /// Category filter (all, presidential, congressional, approval, geopolitical, executive)
        #[arg(long, default_value = "all")]
        category: String,
        /// Search for specific keyword
        #[arg(long)]
        search: Option<String>,
        /// Show only markets with high volume
        #[arg(long)]
        high_volume: bool,
    },
    /// Search for specific candidate or topic
    Search {
        /// Search query (e.g., "trump", "election", "approval")
        query: String,
    },
    /// Analyze a specific political market
    Analyze {
        /// Event ID to analyze
        #[arg(long)]
        event: Option<String>,
        /// Candidate name to search
        #[arg(long)]
        candidate: Option<String>,
    },
    /// Fetch Trump-related markets
    Trump {
        /// Market type (all, favorability, approval, cabinet)
        #[arg(long, default_value = "all")]
        market_type: String,
    },
    /// Show election markets
    Elections {
        /// Year filter (e.g., 2024, 2025, 2026)
        #[arg(long)]
        year: Option<String>,
    },
}

/// Reinforcement Learning subcommands
#[cfg(feature = "rl")]
#[derive(Subcommand, Debug)]
pub enum RlCommands {
    /// Train RL model on historical or simulated data
    Train {
        /// Number of training episodes
        #[arg(short, long, default_value = "1000")]
        episodes: usize,
        /// Checkpoint directory for saving models
        #[arg(short, long, default_value = "./models")]
        checkpoint: String,
        /// Learning rate
        #[arg(long, default_value = "0.0003")]
        lr: f64,
        /// Batch size for training
        #[arg(long, default_value = "64")]
        batch_size: usize,
        /// Update frequency (steps between updates)
        #[arg(long, default_value = "2048")]
        update_freq: usize,
        /// Series ID to train on (for historical data)
        #[arg(short, long)]
        series: Option<String>,
        /// Binance symbol to track
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Resume from checkpoint
        #[arg(long)]
        resume: Option<String>,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },
    /// Run with RL strategy (live or paper trading)
    Run {
        /// Model checkpoint to load
        #[arg(short, long)]
        model: Option<String>,
        /// Enable online learning during trading
        #[arg(long)]
        online_learning: bool,
        /// Series ID to trade
        #[arg(short, long)]
        series: String,
        /// Binance symbol to track
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Initial exploration rate
        #[arg(long, default_value = "0.1")]
        exploration: f32,
        /// Dry run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
    },
    /// Evaluate model performance on test data
    Eval {
        /// Model checkpoint to evaluate
        #[arg(short, long)]
        model: String,
        /// Test data file (CSV format)
        #[arg(short, long)]
        data: String,
        /// Number of evaluation episodes
        #[arg(short, long, default_value = "100")]
        episodes: usize,
        /// Output results to file
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Show RL model info and statistics
    Info {
        /// Model checkpoint to inspect
        #[arg(short, long)]
        model: String,
    },
    /// Export model for deployment
    Export {
        /// Model checkpoint to export
        #[arg(short, long)]
        model: String,
        /// Output format (onnx, torch, json)
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Output file path
        #[arg(short, long)]
        output: String,
    },
    /// Backtest RL strategy on historical or sample data
    Backtest {
        /// Number of backtest episodes
        #[arg(short, long, default_value = "100")]
        episodes: usize,
        /// Duration of each episode in minutes (for sample data)
        #[arg(short, long, default_value = "60")]
        duration: u64,
        /// Market volatility (for sample data)
        #[arg(long, default_value = "0.02")]
        volatility: f64,
        /// Round ID to backtest (uses real data from DB)
        #[arg(short, long)]
        round: Option<i32>,
        /// Initial capital
        #[arg(long, default_value = "1000.0")]
        capital: f64,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },
    /// Train lead-lag RL strategy using LOB data
    LeadLag {
        /// Number of training episodes
        #[arg(short, long, default_value = "1000")]
        episodes: usize,
        /// Trade size in USD
        #[arg(long, default_value = "1.0")]
        trade_size: f64,
        /// Maximum total position in USD
        #[arg(long, default_value = "50.0")]
        max_position: f64,
        /// Binance symbol to train on
        #[arg(short, long, default_value = "BTCUSDT")]
        symbol: String,
        /// Learning rate
        #[arg(long, default_value = "0.0003")]
        lr: f64,
        /// Checkpoint directory
        #[arg(short, long, default_value = "./models/leadlag")]
        checkpoint: String,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },
    /// Run live trading with trained lead-lag model
    LeadLagLive {
        /// Binance symbol to trade
        #[arg(short, long, default_value = "BTCUSDT")]
        symbol: String,
        /// Trade size in USD
        #[arg(long, default_value = "1.0")]
        trade_size: f64,
        /// Maximum total position in USD
        #[arg(long, default_value = "50.0")]
        max_position: f64,
        /// Polymarket market slug (e.g., "will-btc-go-up-15m")
        #[arg(short, long)]
        market: String,
        /// Checkpoint directory to load model from
        #[arg(short, long, default_value = "./models/leadlag")]
        checkpoint: String,
        /// Dry run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
        /// Minimum confidence to trade (0.0-1.0)
        #[arg(long, default_value = "0.6")]
        min_confidence: f64,
    },
    /// Run RL-powered agent with Order Platform (full integration)
    Agent {
        /// Binance symbol to trade
        #[arg(short, long, default_value = "BTCUSDT")]
        symbol: String,
        /// Polymarket market slug (e.g., "will-btc-go-up-15m")
        #[arg(short, long)]
        market: String,
        /// UP token ID
        #[arg(long)]
        up_token: String,
        /// DOWN token ID
        #[arg(long)]
        down_token: String,
        /// Trade size in shares
        #[arg(long, default_value = "100")]
        shares: u64,
        /// Maximum total exposure in USD
        #[arg(long, default_value = "100.0")]
        max_exposure: f64,
        /// Exploration rate (0.0-1.0)
        #[arg(long, default_value = "0.1")]
        exploration: f32,
        /// Enable online learning
        #[arg(long)]
        online_learning: bool,
        /// Dry run mode (no real orders)
        #[arg(long)]
        dry_run: bool,
        /// Tick interval in milliseconds
        #[arg(long, default_value = "1000")]
        tick_interval: u64,
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

/// Show Polymarket sports markets
pub async fn show_polymarket_sports(league: &str, search: Option<&str>, compare_dk: bool, min_edge: f64, live: bool) -> Result<()> {
    use crate::agent::{PolymarketSportsClient, OddsProvider, Sport, Market, NBA_SERIES_ID};
    use rust_decimal::prelude::FromPrimitive;

    let client = PolymarketSportsClient::new()?;

    // If live flag, show live games with scores
    if live {
        println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
        println!("\x1b[36m║           LIVE SPORTS - In-Play Betting                      ║\x1b[0m");
        println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

        println!("Fetching live {} games...\n", league.to_uppercase());

        let series_id = match league.to_lowercase().as_str() {
            "nba" => NBA_SERIES_ID,
            _ => NBA_SERIES_ID, // Default to NBA for now
        };

        let events = client.fetch_series_events(series_id).await?;

        // Get live games first, then today's scheduled games
        let mut live_games = Vec::new();
        let mut scheduled_games = Vec::new();

        for event in events {
            let details = client.get_event_details(&event.id).await?;
            if details.live && !details.ended {
                live_games.push(details);
            } else if !details.ended {
                // Check if it's today's game
                let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                if event.slug.contains(&today) {
                    scheduled_games.push(details);
                }
            }
        }

        if live_games.is_empty() && scheduled_games.is_empty() {
            println!("\x1b[33m⚠ No live or upcoming games found.\x1b[0m\n");
            return Ok(());
        }

        // Display live games
        if !live_games.is_empty() {
            println!("\x1b[31m● LIVE NOW\x1b[0m ({} games)\n", live_games.len());
            println!("┌──────────────────────────────────────────────────────────────────┐");

            for game in &live_games {
                let status = game.live_status();
                let score = game.score.as_deref().unwrap_or("--");

                println!("│ \x1b[31m{}\x1b[0m  \x1b[1m{}\x1b[0m", status, game.title);
                println!("│ Score: \x1b[1;33m{}\x1b[0m  Vol: ${:.0}k",
                    score,
                    game.volume.unwrap_or(0.0) / 1000.0);

                // Show moneyline
                if let Some(ml) = game.moneyline() {
                    if let Some((p1, p2)) = ml.get_prices() {
                        println!("│ Moneyline: \x1b[32m{}¢\x1b[0m / \x1b[31m{}¢\x1b[0m",
                            (p1 * dec!(100)).round_dp(1),
                            (p2 * dec!(100)).round_dp(1));
                    }
                }

                // Show main spread
                let spreads = game.spreads();
                if let Some(spread) = spreads.first() {
                    if let Some((p1, p2)) = spread.get_prices() {
                        println!("│ {}: {}¢ / {}¢", spread.question,
                            (p1 * dec!(100)).round_dp(0),
                            (p2 * dec!(100)).round_dp(0));
                    }
                }

                println!("├──────────────────────────────────────────────────────────────────┤");
            }
            println!("└──────────────────────────────────────────────────────────────────┘\n");
        }

        // Display scheduled games
        if !scheduled_games.is_empty() {
            println!("\x1b[34m○ TODAY'S GAMES\x1b[0m ({} games)\n", scheduled_games.len());

            for game in &scheduled_games {
                if let Some(ml) = game.moneyline() {
                    if let Some((p1, p2)) = ml.get_prices() {
                        println!("  {} - \x1b[32m{}¢\x1b[0m / \x1b[31m{}¢\x1b[0m",
                            game.title,
                            (p1 * dec!(100)).round_dp(1),
                            (p2 * dec!(100)).round_dp(1));
                    }
                } else {
                    println!("  {}", game.title);
                }
            }
            println!();
        }

        println!("\x1b[90mTip: Prices auto-refresh. Use --search \"lakers\" to filter.\x1b[0m\n");
        return Ok(());
    }

    // Original futures markets display
    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║             Polymarket Sports Markets                        ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Fetch markets based on league
    let markets = match league.to_lowercase().as_str() {
        "nba" => {
            println!("Fetching NBA markets from Polymarket...\n");
            client.fetch_nba_markets().await?
        }
        "nfl" => {
            println!("Fetching NFL markets from Polymarket...\n");
            client.fetch_nfl_markets().await?
        }
        _ => {
            println!("Fetching all sports markets from Polymarket...\n");
            client.fetch_sports_markets().await?
        }
    };

    // Filter by search term if provided
    let markets = if let Some(term) = search {
        let term_lower = term.to_lowercase();
        markets.into_iter()
            .filter(|m| {
                m.question.as_ref()
                    .map(|q| q.to_lowercase().contains(&term_lower))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        markets
    };

    if markets.is_empty() {
        println!("\x1b[33m⚠ No sports markets found matching criteria.\x1b[0m\n");
        return Ok(());
    }

    println!("Found {} markets:\n", markets.len());

    // Get DraftKings odds if comparison requested
    let dk_odds = if compare_dk {
        println!("Fetching DraftKings odds for comparison...\n");
        let provider = OddsProvider::from_env().ok();
        if let Some(ref p) = provider {
            let sport = match league.to_lowercase().as_str() {
                "nba" => Sport::NBA,
                "nfl" => Sport::NFL,
                _ => Sport::NBA,
            };
            p.get_odds(sport, Market::Moneyline).await.ok()
        } else {
            None
        }
    } else {
        None
    };

    // Display markets
    println!("┌────────────────────────────────────────────────────────────────┐");
    println!("│ {:^62} │", "POLYMARKET SPORTS MARKETS");
    println!("├────────────────────────────────────────────────────────────────┤");

    for market in &markets {
        let question = market.question.as_deref().unwrap_or("Unknown");
        let truncated = if question.len() > 55 {
            format!("{}...", &question[..52])
        } else {
            question.to_string()
        };

        println!("│ {:<62} │", truncated);

        // Get prices if available
        if let Some((yes_price, no_price)) = market.get_prices() {
            println!("│   Yes: \x1b[32m{:.1}¢\x1b[0m  No: \x1b[31m{:.1}¢\x1b[0m                                        │",
                yes_price * dec!(100), no_price * dec!(100));
        }

        // Compare with DraftKings if we have odds
        if compare_dk {
            if let Some(ref events) = dk_odds {
                if let Some((team1, team2)) = market.extract_teams() {
                    let matching_event = events.iter().find(|e| {
                        let q = format!("{} {}", e.home_team, e.away_team).to_lowercase();
                        q.contains(&team1.to_lowercase()) || q.contains(&team2.to_lowercase())
                    });

                    if let Some(event) = matching_event {
                        if let Some(best) = event.best_odds() {
                            let dk_prob = best.home_implied_prob;
                            if let Some((poly_yes, _)) = market.get_prices() {
                                let edge = dk_prob - poly_yes;
                                let edge_pct = edge * dec!(100);
                                if edge_pct.abs() >= Decimal::from_f64(min_edge).unwrap_or(dec!(5)) {
                                    if edge > Decimal::ZERO {
                                        println!("│   \x1b[32m✓ EDGE: +{:.1}% (DK favors YES)\x1b[0m                            │", edge_pct);
                                    } else {
                                        println!("│   \x1b[31m✓ EDGE: {:.1}% (DK favors NO)\x1b[0m                             │", edge_pct);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Show token IDs
        if let Some((yes_token, no_token)) = market.get_token_ids() {
            println!("│   Yes Token: {}... │", &yes_token[..40.min(yes_token.len())]);
        }

        println!("├────────────────────────────────────────────────────────────────┤");
    }
    println!("└────────────────────────────────────────────────────────────────┘\n");

    if compare_dk && dk_odds.is_none() {
        println!("\x1b[33m⚠ DraftKings comparison unavailable. Set THE_ODDS_API_KEY.\x1b[0m\n");
    }

    println!("\x1b[90mTip: Use --search \"lakers\" to find specific matchups\x1b[0m");
    println!("\x1b[90m     Use --compare-dk to see edges vs sportsbook odds\x1b[0m\n");

    Ok(())
}

/// Execute full decision chain: Grok -> Claude -> DraftKings -> Polymarket
pub async fn run_sports_chain(
    team1: &str,
    team2: &str,
    sport: &str,
    execute: bool,
    amount: f64,
) -> Result<()> {
    use crate::agent::{
        PolymarketSportsClient, OddsProvider, Sport, Market,
        GrokClient, GrokConfig, PolymarketEdgeAnalysis,
    };

    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║         Sports Betting Decision Chain                        ║\x1b[0m");
    println!("\x1b[36m║   Grok → Claude → DraftKings → Polymarket                   ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("🏀 Analyzing: {} vs {}\n", team1, team2);

    // Step 1: Grok - Get real-time data
    println!("\x1b[33m[Step 1/4] Fetching real-time data via Grok...\x1b[0m");
    let grok_config = GrokConfig::from_env();
    let grok_data = if grok_config.is_configured() {
        match GrokClient::new(grok_config) {
            Ok(client) => {
                let query = format!("{} vs {} latest news injuries lineup", team1, team2);
                match client.search(&query).await {
                    Ok(result) => {
                        println!("   ✓ Grok analysis complete");
                        println!("   Summary: {}", &result.summary[..100.min(result.summary.len())]);
                        if let Some(sentiment) = result.sentiment {
                            println!("   Sentiment: {}", sentiment);
                        }
                        for (i, point) in result.key_points.iter().take(3).enumerate() {
                            println!("   {}. {}", i + 1, point);
                        }
                        Some(result)
                    }
                    Err(e) => {
                        println!("   ⚠ Grok search failed: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                println!("   ⚠ Grok client error: {}", e);
                None
            }
        }
    } else {
        println!("   ⚠ GROK_API_KEY not set, skipping real-time data");
        None
    };

    // Step 2: DraftKings - Get sportsbook odds
    println!("\n\x1b[33m[Step 2/4] Fetching DraftKings odds...\x1b[0m");
    let sport_enum = match sport.to_lowercase().as_str() {
        "nba" => Sport::NBA,
        "nfl" => Sport::NFL,
        "nhl" => Sport::NHL,
        "mlb" => Sport::MLB,
        _ => Sport::NBA,
    };

    let dk_odds = OddsProvider::from_env().ok();
    let sportsbook_prob = if let Some(ref provider) = dk_odds {
        match provider.get_odds(sport_enum, Market::Moneyline).await {
            Ok(events) => {
                let matching = events.iter().find(|e| {
                    let matchup = format!("{} {}", e.home_team, e.away_team).to_lowercase();
                    matchup.contains(&team1.to_lowercase()) || matchup.contains(&team2.to_lowercase())
                });

                if let Some(event) = matching {
                    if let Some(best) = event.best_odds() {
                        println!("   ✓ {} vs {}", event.home_team, event.away_team);
                        println!("   Home ({}) odds: {} ({:.1}%)",
                            best.home_bookmaker, best.home_american_odds, best.home_implied_prob * dec!(100));
                        println!("   Away ({}) odds: {} ({:.1}%)",
                            best.away_bookmaker, best.away_american_odds, best.away_implied_prob * dec!(100));
                        Some(best.home_implied_prob)
                    } else {
                        println!("   ⚠ No odds available for this matchup");
                        None
                    }
                } else {
                    println!("   ⚠ Game not found in DraftKings odds");
                    None
                }
            }
            Err(e) => {
                println!("   ⚠ DraftKings error: {}", e);
                None
            }
        }
    } else {
        println!("   ⚠ THE_ODDS_API_KEY not set");
        None
    };

    // Step 3: Polymarket - Find market and get prices
    println!("\n\x1b[33m[Step 3/4] Finding Polymarket market...\x1b[0m");
    let poly_client = PolymarketSportsClient::new()?;

    let market_details = poly_client.find_game_market(team1, team2).await?;

    let (edge_analysis, yes_token, no_token) = if let Some(ref details) = market_details {
        println!("   ✓ Found: {}", details.market.question.as_deref().unwrap_or("Unknown"));
        if let Some(yes_price) = details.yes_price() {
            println!("   Current Polymarket: Yes={:.1}¢ No={:.1}¢",
                yes_price * dec!(100),
                details.no_price().unwrap_or(dec!(0)) * dec!(100));
        }

        let edge = if let Some(sb_prob) = sportsbook_prob {
            PolymarketEdgeAnalysis::calculate(details, sb_prob)
        } else {
            None
        };

        (edge, Some(details.yes_token_id.clone()), Some(details.no_token_id.clone()))
    } else {
        println!("   ⚠ No Polymarket market found for {} vs {}", team1, team2);
        (None, None, None)
    };

    // Step 4: Decision
    println!("\n\x1b[33m[Step 4/4] Decision Analysis...\x1b[0m");
    println!("\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");

    if let Some(ref edge) = edge_analysis {
        println!("\n   \x1b[32m📊 Edge Analysis:\x1b[0m");
        println!("   Polymarket: Yes={:.1}% No={:.1}%",
            edge.polymarket_yes_prob * dec!(100), edge.polymarket_no_prob * dec!(100));
        println!("   DraftKings:  Yes={:.1}% No={:.1}%",
            edge.sportsbook_yes_prob * dec!(100), edge.sportsbook_no_prob * dec!(100));
        println!("   Edge on YES: {}{:.1}%",
            if edge.yes_edge > Decimal::ZERO { "+" } else { "" }, edge.yes_edge * dec!(100));
        println!("   Edge on NO:  {}{:.1}%",
            if edge.no_edge > Decimal::ZERO { "+" } else { "" }, edge.no_edge * dec!(100));

        if edge.is_significant() {
            println!("\n   \x1b[32m✓ RECOMMENDATION: Bet {} on {}\x1b[0m",
                edge.recommended_side, edge.market);
            println!("   Kelly fraction: {:.1}% of bankroll", edge.kelly_fraction() * dec!(100));

            if execute {
                println!("\n   \x1b[33m⚠ Order execution not yet implemented.\x1b[0m");
                println!("   Token to bet: {}", edge.recommended_token());
                println!("   Amount: ${}", amount);
            }
        } else {
            println!("\n   \x1b[33m⚠ No significant edge detected (need >5%)\x1b[0m");
        }
    } else {
        println!("\n   \x1b[33m⚠ Unable to calculate edge.\x1b[0m");
        println!("   Missing: {}", if sportsbook_prob.is_none() { "DraftKings odds" } else { "Polymarket market" });
    }

    println!("\n\x1b[36m─────────────────────────────────────────────────────────────────\x1b[0m");
    println!("\x1b[90mDecision chain complete.\x1b[0m\n");

    Ok(())
}

/// Show Polymarket political markets
pub async fn show_polymarket_politics(category: &str, search: Option<&str>, high_volume: bool) -> Result<()> {
    use crate::agent::{PolymarketPoliticsClient, PoliticalCategory};

    let client = PolymarketPoliticsClient::new()?;

    println!("\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║           POLITICAL PREDICTION MARKETS                       ║\x1b[0m");
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Fetch markets based on category
    let cat = PoliticalCategory::from_str(category);
    let markets = match cat {
        PoliticalCategory::All => {
            println!("Fetching all political markets...\n");
            client.fetch_politics_markets().await?
        }
        _ => {
            println!("Fetching {:?} markets...\n", cat);
            client.fetch_by_category(cat).await?
        }
    };

    // Filter by search term if provided
    let markets = if let Some(term) = search {
        let term_lower = term.to_lowercase();
        markets.into_iter()
            .filter(|m| {
                m.question.as_ref()
                    .map(|q| q.to_lowercase().contains(&term_lower))
                    .unwrap_or(false) ||
                m.description.as_ref()
                    .map(|d| d.to_lowercase().contains(&term_lower))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        markets
    };

    // Filter by volume if requested
    let markets: Vec<_> = if high_volume {
        markets.into_iter()
            .filter(|m| m.volume.unwrap_or(0.0) > 100000.0)
            .collect()
    } else {
        markets
    };

    if markets.is_empty() {
        println!("\x1b[33m⚠ No political markets found matching criteria.\x1b[0m\n");
        return Ok(());
    }

    println!("Found {} markets:\n", markets.len());

    // Display markets
    println!("┌────────────────────────────────────────────────────────────────┐");
    println!("│ {:^62} │", "POLITICAL MARKETS");
    println!("├────────────────────────────────────────────────────────────────┤");

    for market in &markets {
        let question = market.question.as_deref().unwrap_or("Unknown");
        let truncated = if question.len() > 55 {
            format!("{}...", &question[..52])
        } else {
            question.to_string()
        };

        println!("│ {:<62} │", truncated);

        // Get prices if available
        if let Some((yes_price, no_price)) = market.get_prices() {
            let vol_str = match market.volume {
                Some(v) if v >= 1_000_000.0 => format!("${:.1}M", v / 1_000_000.0),
                Some(v) if v >= 1_000.0 => format!("${:.0}K", v / 1_000.0),
                Some(v) => format!("${:.0}", v),
                None => "N/A".to_string(),
            };

            println!("│   \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m  Vol: {}                │",
                yes_price * dec!(100), no_price * dec!(100), vol_str);
        }

        // Show end date if available
        if let Some(end_date) = &market.end_date {
            if let Some(date_part) = end_date.split('T').next() {
                println!("│   Ends: {}                                            │", date_part);
            }
        }

        // Show token IDs
        if let Some((yes_token, _)) = market.get_token_ids() {
            let truncated_token = if yes_token.len() > 40 {
                format!("{}...", &yes_token[..37])
            } else {
                yes_token.clone()
            };
            println!("│   Token: {}                   │", truncated_token);
        }

        println!("├────────────────────────────────────────────────────────────────┤");
    }
    println!("└────────────────────────────────────────────────────────────────┘\n");

    println!("\x1b[90mTip: Use --search \"trump\" to find specific topics\x1b[0m");
    println!("\x1b[90m     Use --category approval for approval ratings\x1b[0m");
    println!("\x1b[90m     Categories: all, presidential, congressional, approval, geopolitical, executive\x1b[0m\n");

    Ok(())
}

/// Search political markets
pub async fn search_politics_markets(query: &str) -> Result<()> {
    use crate::agent::PolymarketPoliticsClient;

    let client = PolymarketPoliticsClient::new()?;

    println!("\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║           POLITICAL MARKET SEARCH                            ║\x1b[0m");
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    println!("Searching for: \"{}\"\n", query);

    let markets = client.search_markets(query).await?;

    if markets.is_empty() {
        println!("\x1b[33m⚠ No markets found matching \"{}\".\x1b[0m\n", query);
        return Ok(());
    }

    println!("Found {} matching markets:\n", markets.len());

    for (i, market) in markets.iter().enumerate().take(20) {
        let question = market.question.as_deref().unwrap_or("Unknown");

        print!("{}. ", i + 1);
        println!("\x1b[1m{}\x1b[0m", question);

        if let Some((yes_price, no_price)) = market.get_prices() {
            let vol_str = match market.volume {
                Some(v) if v >= 1_000_000.0 => format!("${:.1}M", v / 1_000_000.0),
                Some(v) if v >= 1_000.0 => format!("${:.0}K", v / 1_000.0),
                Some(v) => format!("${:.0}", v),
                None => "N/A".to_string(),
            };

            println!("   \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m  Volume: {}",
                yes_price * dec!(100), no_price * dec!(100), vol_str);
        }

        if let Some(end_date) = &market.end_date {
            if let Some(date_part) = end_date.split('T').next() {
                println!("   \x1b[90mEnds: {}\x1b[0m", date_part);
            }
        }
        println!();
    }

    if markets.len() > 20 {
        println!("\x1b[90m... and {} more markets\x1b[0m\n", markets.len() - 20);
    }

    Ok(())
}

/// Analyze specific political market or candidate
pub async fn analyze_politics_market(event_id: Option<&str>, candidate: Option<&str>) -> Result<()> {
    use crate::agent::PolymarketPoliticsClient;

    let client = PolymarketPoliticsClient::new()?;

    println!("\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║           POLITICAL MARKET ANALYSIS                          ║\x1b[0m");
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    if let Some(eid) = event_id {
        println!("Fetching event {}...\n", eid);

        let event = client.get_event_details(eid).await?;

        println!("\x1b[1m{}\x1b[0m\n", event.title);

        if let Some(desc) = &event.description {
            let truncated = if desc.len() > 200 {
                format!("{}...", &desc[..197])
            } else {
                desc.clone()
            };
            println!("{}\n", truncated);
        }

        println!("End Date: {}", event.end_date_formatted());

        if let Some(vol) = event.volume {
            println!("Volume: ${:.0}", vol);
        }

        println!("\nMarkets:");
        for (i, market) in event.markets.iter().enumerate() {
            println!("  {}. {}", i + 1, market.question);
            if let Some((p1, p2)) = market.get_prices() {
                println!("     \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m",
                    p1 * dec!(100), p2 * dec!(100));
            }
        }
    } else if let Some(name) = candidate {
        println!("Searching for {} markets...\n", name);

        let markets = client.search_candidate(name).await?;

        if markets.is_empty() {
            println!("\x1b[33m⚠ No markets found for \"{}\".\x1b[0m\n", name);
            return Ok(());
        }

        println!("Found {} markets for {}:\n", markets.len(), name);

        for market in markets.iter().take(10) {
            println!("\x1b[1m{}\x1b[0m", market.question.as_deref().unwrap_or("Unknown"));
            if let Some((p1, p2)) = market.get_prices() {
                println!("  \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m",
                    p1 * dec!(100), p2 * dec!(100));
            }
            println!();
        }
    } else {
        println!("\x1b[33m⚠ Please specify --event or --candidate\x1b[0m\n");
    }

    Ok(())
}

/// Show Trump-related markets
pub async fn show_trump_markets(market_type: &str) -> Result<()> {
    use crate::agent::PolymarketPoliticsClient;

    let client = PolymarketPoliticsClient::new()?;

    println!("\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║              TRUMP PREDICTION MARKETS                        ║\x1b[0m");
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    let markets = match market_type.to_lowercase().as_str() {
        "favorability" => {
            println!("Fetching Trump favorability markets...\n");
            let events = client.fetch_trump_favorability_events().await?;
            // Convert events to markets for display
            let mut markets = Vec::new();
            for event in events {
                for market in event.markets {
                    if let Some(cid) = market.condition_id {
                        markets.push(crate::agent::PolymarketPoliticsMarket {
                            condition_id: cid,
                            question: Some(market.question),
                            slug: Some(event.slug.clone()),
                            active: true,
                            closed: event.closed,
                            end_date: None,
                            clob_token_ids: market.clob_token_ids,
                            outcome_prices: market.outcome_prices,
                            volume: market.volume,
                            liquidity: None,
                            description: None,
                            tags: vec![],
                        });
                    }
                }
            }
            markets
        }
        "approval" => {
            println!("Fetching Trump approval markets...\n");
            let events = client.fetch_trump_approval_events().await?;
            let mut markets = Vec::new();
            for event in events {
                for market in event.markets {
                    if let Some(cid) = market.condition_id {
                        markets.push(crate::agent::PolymarketPoliticsMarket {
                            condition_id: cid,
                            question: Some(market.question),
                            slug: Some(event.slug.clone()),
                            active: true,
                            closed: event.closed,
                            end_date: None,
                            clob_token_ids: market.clob_token_ids,
                            outcome_prices: market.outcome_prices,
                            volume: market.volume,
                            liquidity: None,
                            description: None,
                            tags: vec![],
                        });
                    }
                }
            }
            markets
        }
        _ => {
            println!("Fetching all Trump-related markets...\n");
            client.fetch_trump_markets().await?
        }
    };

    if markets.is_empty() {
        println!("\x1b[33m⚠ No Trump markets found.\x1b[0m\n");
        return Ok(());
    }

    println!("Found {} Trump markets:\n", markets.len());

    for market in markets.iter().take(15) {
        println!("\x1b[1m{}\x1b[0m", market.question.as_deref().unwrap_or("Unknown"));

        if let Some((yes_price, no_price)) = market.get_prices() {
            let vol_str = match market.volume {
                Some(v) if v >= 1_000_000.0 => format!("${:.1}M", v / 1_000_000.0),
                Some(v) if v >= 1_000.0 => format!("${:.0}K", v / 1_000.0),
                Some(v) => format!("${:.0}", v),
                None => "N/A".to_string(),
            };

            println!("  \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m  Volume: {}",
                yes_price * dec!(100), no_price * dec!(100), vol_str);
        }
        println!();
    }

    if markets.len() > 15 {
        println!("\x1b[90m... and {} more markets\x1b[0m\n", markets.len() - 15);
    }

    Ok(())
}

/// Show election markets
pub async fn show_election_markets(year: Option<&str>) -> Result<()> {
    use crate::agent::PolymarketPoliticsClient;

    let client = PolymarketPoliticsClient::new()?;

    println!("\x1b[35m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[35m║              ELECTION PREDICTION MARKETS                     ║\x1b[0m");
    println!("\x1b[35m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    let markets = client.fetch_election_markets().await?;

    // Filter by year if provided
    let markets: Vec<_> = if let Some(y) = year {
        markets.into_iter()
            .filter(|m| {
                m.question.as_ref()
                    .map(|q| q.contains(y))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        markets
    };

    if markets.is_empty() {
        println!("\x1b[33m⚠ No election markets found.\x1b[0m\n");
        return Ok(());
    }

    println!("Found {} election markets:\n", markets.len());

    for market in markets.iter().take(20) {
        println!("\x1b[1m{}\x1b[0m", market.question.as_deref().unwrap_or("Unknown"));

        if let Some((yes_price, no_price)) = market.get_prices() {
            let vol_str = match market.volume {
                Some(v) if v >= 1_000_000.0 => format!("${:.1}M", v / 1_000_000.0),
                Some(v) if v >= 1_000.0 => format!("${:.0}K", v / 1_000.0),
                Some(v) => format!("${:.0}", v),
                None => "N/A".to_string(),
            };

            println!("  \x1b[32mYES: {:.1}¢\x1b[0m  \x1b[31mNO: {:.1}¢\x1b[0m  Volume: {}",
                yes_price * dec!(100), no_price * dec!(100), vol_str);
        }

        if let Some(end_date) = &market.end_date {
            if let Some(date_part) = end_date.split('T').next() {
                println!("  \x1b[90mEnds: {}\x1b[0m", date_part);
            }
        }
        println!();
    }

    if markets.len() > 20 {
        println!("\x1b[90m... and {} more markets\x1b[0m\n", markets.len() - 20);
    }

    Ok(())
}

/// Edge opportunity found by the scanner
#[derive(Debug, Clone)]
pub struct EdgeOpportunity {
    pub game: String,
    pub market_type: String,
    pub market_question: String,
    pub pm_price: f64,
    pub dk_fair_prob: f64,
    pub edge: f64,
    pub token_id: String,
    pub condition_id: String,
    pub event_id: String,
    pub is_live: bool,
    pub score: Option<String>,
    pub period: Option<String>,
}

/// Live edge scanner - continuously monitors live games for arbitrage opportunities
pub async fn run_live_edge_scanner(
    sport: &str,
    min_edge: f64,
    interval: u64,
    scan_spreads: bool,
    scan_moneyline: bool,
    scan_props: bool,
    alert_sound: bool,
) -> Result<()> {
    use crate::agent::{
        PolymarketSportsClient, OddsProvider, Sport, Market,
        EventDetails, NBA_SERIES_ID,
    };
    use std::collections::HashMap;

    println!("\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║         🎯 LIVE EDGE SCANNER                                 ║\x1b[0m");
    println!("\x1b[36m║   Monitoring {} | Min Edge: {}% | Interval: {}s             ║\x1b[0m",
        sport.to_uppercase(), min_edge, interval);
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    let pm_client = PolymarketSportsClient::new()?;

    // Check for DK API key
    let dk_provider = OddsProvider::from_env().ok();
    if dk_provider.is_none() {
        println!("\x1b[33m⚠ THE_ODDS_API_KEY not set. Using Polymarket-only mode.\x1b[0m\n");
    }

    let sport_enum = match sport.to_lowercase().as_str() {
        "nba" => Sport::NBA,
        "nfl" => Sport::NFL,
        _ => Sport::NBA,
    };

    let series_id = match sport.to_lowercase().as_str() {
        "nba" => NBA_SERIES_ID,
        _ => NBA_SERIES_ID,
    };

    let scan_all = !scan_spreads && !scan_moneyline && !scan_props;

    println!("Scanning: {}{}{}",
        if scan_spreads || scan_all { "Spreads " } else { "" },
        if scan_moneyline || scan_all { "Moneyline " } else { "" },
        if scan_props || scan_all { "Props " } else { "" });
    println!("Press Ctrl+C to stop\n");

    let mut scan_count = 0u64;
    let mut found_opportunities: Vec<EdgeOpportunity> = Vec::new();

    loop {
        scan_count += 1;
        let now = chrono::Utc::now().format("%H:%M:%S");
        println!("\x1b[90m[{}] Scan #{} starting...\x1b[0m", now, scan_count);

        // Fetch all today's games from Polymarket (including scheduled and live)
        let live_games = match pm_client.fetch_todays_games_with_details(series_id).await {
            Ok(games) => games,
            Err(e) => {
                println!("\x1b[31m  Error fetching PM games: {}\x1b[0m", e);
                tokio::time::sleep(Duration::from_secs(interval)).await;
                continue;
            }
        };

        // Helper function to extract team nickname from full name (e.g., "Miami Heat" -> "heat")
        fn extract_nickname(full_name: &str) -> &str {
            full_name.split_whitespace().last().unwrap_or(full_name)
        }

        // Fetch DraftKings odds if available
        let dk_odds: HashMap<String, (f64, f64)> = if let Some(ref provider) = dk_provider {
            let mut odds_map = HashMap::new();

            // Fetch moneyline
            match provider.get_odds(sport_enum, Market::Moneyline).await {
                Ok(events) => {
                    for event in events {
                        if let Some(best) = event.best_odds() {
                            let home_prob: f64 = best.home_implied_prob.try_into().unwrap_or(0.5);
                            let away_prob: f64 = best.away_implied_prob.try_into().unwrap_or(0.5);
                            // Remove vig to get fair probability
                            let total = home_prob + away_prob;
                            let fair_home = home_prob / total;
                            let fair_away = away_prob / total;

                            // Store both full name key and nickname key
                            let key = format!("{} {}", event.home_team, event.away_team).to_lowercase();
                            odds_map.insert(key, (fair_home, fair_away));

                            // Also store with nicknames for PM matching (PM uses "Timberwolves vs. Heat" format)
                            let home_nick = extract_nickname(&event.home_team).to_lowercase();
                            let away_nick = extract_nickname(&event.away_team).to_lowercase();
                            let nick_key = format!("{} vs. {}", away_nick, home_nick); // PM format: Away vs. Home
                            odds_map.insert(nick_key, (fair_home, fair_away));
                        }
                    }
                }
                Err(e) => {
                    println!("  \x1b[31mDK moneyline fetch error: {}\x1b[0m", e);
                }
            }

            // Fetch spreads
            if scan_spreads || scan_all {
                if let Ok(events) = provider.get_odds(sport_enum, Market::Spread).await {
                    for event in events {
                        if let Some(best) = event.best_odds() {
                            let home_prob: f64 = best.home_implied_prob.try_into().unwrap_or(0.5);
                            let away_prob: f64 = best.away_implied_prob.try_into().unwrap_or(0.5);
                            let total = home_prob + away_prob;
                            let fair_home = home_prob / total;
                            let fair_away = away_prob / total;

                            let key = format!("{} {} spread", event.home_team, event.away_team).to_lowercase();
                            odds_map.insert(key, (fair_home, fair_away));

                            // Also store with nicknames for PM matching
                            let home_nick = extract_nickname(&event.home_team).to_lowercase();
                            let away_nick = extract_nickname(&event.away_team).to_lowercase();
                            let nick_key = format!("{} vs. {} spread", away_nick, home_nick);
                            odds_map.insert(nick_key, (fair_home, fair_away));
                        }
                    }
                }
            }

            odds_map
        } else {
            HashMap::new()
        };

        let live_count = live_games.iter().filter(|g| g.live).count();
        let scheduled_count = live_games.len() - live_count;

        println!("  Found {} games ({} LIVE, {} scheduled)",
            live_games.len(), live_count, scheduled_count);

        // Warn if DK comparison unavailable
        if dk_odds.is_empty() && dk_provider.is_some() {
            println!("  \x1b[33m⚠ DK odds empty - edge comparison unavailable\x1b[0m");
        } else if !dk_odds.is_empty() {
            println!("  \x1b[32m✓ DK odds loaded ({} markets)\x1b[0m", dk_odds.len());
        }

        let mut new_opportunities: Vec<EdgeOpportunity> = Vec::new();

        for game in &live_games {
            // Process each market in the game
            for market in &game.markets {
                let question = &market.question;

                // Determine market type
                let is_spread = question.contains("Spread:");
                let is_moneyline = !is_spread && !question.contains("Over") &&
                                   !question.contains("Points") && !question.contains("Rebounds") &&
                                   !question.contains("Assists") && !question.contains("O/U");
                let is_prop = question.contains("Points Over") || question.contains("Rebounds Over") ||
                              question.contains("Assists Over");

                // Skip if not scanning this type
                if !scan_all {
                    if is_spread && !scan_spreads { continue; }
                    if is_moneyline && !scan_moneyline { continue; }
                    if is_prop && !scan_props { continue; }
                }

                // Parse Polymarket prices (stored as JSON string like "[\"0.5\", \"0.5\"]")
                let pm_yes_price: f64 = market.outcome_prices.as_ref()
                    .and_then(|p| {
                        serde_json::from_str::<Vec<String>>(p).ok()
                    })
                    .and_then(|prices| prices.get(0).cloned())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.5);

                // Try to find matching DK odds
                let game_key = game.title.to_lowercase();
                let dk_fair = if is_moneyline {
                    dk_odds.get(&game_key).map(|(home, away)| {
                        // Determine which side this market represents
                        let parts: Vec<&str> = game.title.split(" vs. ").collect();
                        if parts.len() == 2 {
                            // First team in title is typically away team on Polymarket
                            *away
                        } else {
                            0.5
                        }
                    })
                } else if is_spread {
                    let spread_key = format!("{} spread", game_key);
                    dk_odds.get(&spread_key).map(|(home, _)| *home)
                } else {
                    None
                };

                // Calculate edge
                if let Some(dk_prob) = dk_fair {
                    let edge = (dk_prob - pm_yes_price) * 100.0;

                    if edge.abs() >= min_edge {
                        // Parse token IDs (stored as JSON string like "[\"123...\", \"456...\"]")
                        let token_id = market.clob_token_ids.as_ref()
                            .and_then(|ids| serde_json::from_str::<Vec<String>>(ids).ok())
                            .and_then(|ids| ids.get(0).cloned())
                            .unwrap_or_default();

                        let opp = EdgeOpportunity {
                            game: game.title.clone(),
                            market_type: if is_spread { "Spread".to_string() }
                                        else if is_moneyline { "Moneyline".to_string() }
                                        else { "Prop".to_string() },
                            market_question: question.clone(),
                            pm_price: pm_yes_price,
                            dk_fair_prob: dk_prob,
                            edge,
                            token_id,
                            condition_id: market.condition_id.clone().unwrap_or_default(),
                            event_id: game.id.clone(),
                            is_live: game.live,
                            score: game.score.clone(),
                            period: game.period.clone(),
                        };

                        new_opportunities.push(opp);
                    }
                }
            }
        }

        // Display new opportunities
        if !new_opportunities.is_empty() {
            println!("\n\x1b[32m🎯 Found {} opportunities with edge >= {}%:\x1b[0m\n",
                new_opportunities.len(), min_edge);

            for opp in &new_opportunities {
                let live_tag = if opp.is_live {
                    format!("\x1b[31m🔴 LIVE {}\x1b[0m", opp.period.as_deref().unwrap_or(""))
                } else {
                    "\x1b[34m○ SCHEDULED\x1b[0m".to_string()
                };

                println!("┌────────────────────────────────────────────────────────────────┐");
                println!("│ {} {} │", live_tag, opp.game);
                if let Some(ref score) = opp.score {
                    println!("│ Score: \x1b[1;33m{}\x1b[0m │", score);
                }
                println!("├────────────────────────────────────────────────────────────────┤");
                println!("│ Market: {} │", opp.market_question);
                println!("│ Type: {} │", opp.market_type);
                println!("├────────────────────────────────────────────────────────────────┤");
                println!("│ PM Price: \x1b[33m{:.1}¢\x1b[0m │ DK Fair: \x1b[36m{:.1}%\x1b[0m │ Edge: \x1b[32m{:+.1}%\x1b[0m │",
                    opp.pm_price * 100.0, opp.dk_fair_prob * 100.0, opp.edge);
                println!("├────────────────────────────────────────────────────────────────┤");
                println!("│ Action: \x1b[1;32mBUY YES @ {:.1}¢\x1b[0m │", opp.pm_price * 100.0);
                println!("│ Token: {}... │", &opp.token_id[..20.min(opp.token_id.len())]);
                println!("└────────────────────────────────────────────────────────────────┘\n");

                // Play alert sound if enabled
                if alert_sound {
                    print!("\x07"); // Bell character
                    std::io::stdout().flush().ok();
                }
            }

            found_opportunities.extend(new_opportunities);
        } else {
            println!("  No opportunities found with edge >= {}%", min_edge);
        }

        // Summary
        println!("\n\x1b[90m  Total opportunities found this session: {}\x1b[0m",
            found_opportunities.len());
        println!("\x1b[90m  Next scan in {} seconds...\x1b[0m\n", interval);

        // Wait for next scan
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}
