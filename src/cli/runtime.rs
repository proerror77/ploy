use clap::{Parser, Subcommand};

/// Runtime CLI for coordinator + agent execution flow.
#[derive(Parser, Debug)]
#[command(name = "ploy")]
#[command(author = "Ploy Team")]
#[command(version = "0.1.0")]
#[command(
    about = "Agent-based Polymarket trading platform runtime",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Override dry run mode: `--dry-run` = true, `--dry-run=false` = false
    #[arg(short, long, num_args = 0..=1, default_missing_value = "true")]
    pub dry_run: Option<bool>,

    /// Optional market slug override (otherwise use config file value)
    #[arg(short, long)]
    pub market: Option<String>,

    /// Config file path
    #[arg(short, long, default_value = "config/default.toml")]
    pub config: String,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the API server only (for dashboards / docker-compose)
    Serve {
        /// Port to listen on (default: from config/env, usually 8081)
        #[arg(long)]
        port: Option<u16>,
    },

    /// JSON-RPC 2.0 over stdin/stdout (for remote agent tool integration)
    Rpc,

    /// Show account balance and positions
    Account {
        /// Show open orders
        #[arg(long)]
        orders: bool,
        /// Show positions
        #[arg(long)]
        positions: bool,
    },

    /// OpenClaw / agent runtime commands
    Agent {
        /// Agent mode: advisory, autonomous, sports
        #[arg(short = 'M', long, default_value = "advisory")]
        mode: String,
        /// Market/event to analyze (optional)
        #[arg(short = 'e', long)]
        market: Option<String>,
        /// Sports event URL (for sports mode)
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
        /// Print a lightweight collector data-quality report and exit
        #[arg(long)]
        check_only: bool,
        /// Lookback window in minutes for duplicate-ratio checks
        #[arg(long, default_value = "60")]
        lookback_minutes: u64,
        /// Mark a table stale when its latest timestamp is older than this threshold
        #[arg(long, default_value = "120")]
        freshness_warn_secs: u64,
    },

    /// Backfill Polymarket L2 orderbook history via `clob.polymarket.com/orderbook-history`
    OrderbookHistory {
        /// Asset IDs (token IDs) to backfill (comma-separated)
        #[arg(long)]
        asset_ids: String,

        /// Start timestamp (milliseconds since epoch). If omitted, uses now - lookback_secs.
        #[arg(long)]
        start_ms: Option<i64>,

        /// End timestamp (milliseconds since epoch). If omitted, uses now.
        #[arg(long)]
        end_ms: Option<i64>,

        /// Lookback window (seconds) when start_ms is omitted.
        #[arg(long, default_value = "300")]
        lookback_secs: u64,

        /// Max depth levels to persist per side (bids/asks).
        #[arg(long, default_value = "20")]
        levels: usize,

        /// Sampling cadence (milliseconds). Set 0 to persist every snapshot returned.
        #[arg(long, default_value = "1000")]
        sample_ms: i64,

        /// Page size for API requests.
        #[arg(long, default_value = "500")]
        limit: usize,

        /// Max pages per backfill call (safety guard).
        #[arg(long, default_value = "50")]
        max_pages: usize,

        /// Override API base URL (default: https://clob.polymarket.com).
        #[arg(long, default_value = "https://clob.polymarket.com")]
        base_url: String,

        /// Resume from DB high-water mark per asset (ignores start_ms when set).
        #[arg(long)]
        resume_from_db: bool,
    },

    /// Backfill Deribit volatility index (DVOL) history into Postgres
    DeribitIvBackfill {
        /// Currencies to backfill (comma-separated: BTC,ETH)
        #[arg(long, default_value = "BTC,ETH")]
        currencies: String,

        /// Start timestamp (RFC3339). If omitted, uses now - lookback_days.
        #[arg(long)]
        start: Option<String>,

        /// End timestamp (RFC3339). If omitted, uses now.
        #[arg(long)]
        end: Option<String>,

        /// Lookback window (days) when start is omitted.
        #[arg(long, default_value = "30")]
        lookback_days: u64,

        /// Bar resolution in seconds (Deribit TradingView resolution).
        #[arg(long, default_value = "60")]
        resolution_secs: u32,

        /// Sleep between API pages (milliseconds).
        #[arg(long, default_value = "200")]
        sleep_ms: u64,

        /// Override Deribit public API base URL
        #[arg(long, default_value = "https://www.deribit.com/api/v2/public")]
        base_url: String,

        /// Fetch only; don't write to DB
        #[arg(long)]
        dry_run: bool,
    },

    /// Crypto market strategies (BTC, ETH, SOL UP/DOWN)
    #[command(subcommand)]
    Crypto(CryptoCommands),

    /// Sports market strategies (NBA, NFL, etc.)
    #[command(subcommand)]
    Sports(SportsCommands),

    /// Manage trading strategies
    #[command(subcommand)]
    Strategy(super::strategy::StrategyCommands),

    /// Claim/redeem winning positions from resolved markets
    #[cfg(feature = "claimer_cli")]
    Claim {
        /// Check only (don't actually claim)
        #[arg(long)]
        check_only: bool,
        /// Minimum position size to claim (in USDC)
        #[arg(long, default_value = "1")]
        min_size: f64,
        /// Poll interval in seconds (0 = one-shot)
        #[arg(long, default_value = "0")]
        interval: u64,
    },

    /// View trading history and statistics
    History {
        /// Number of recent trades to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
        /// Filter by symbol (e.g., BTCUSDT)
        #[arg(short, long)]
        symbol: Option<String>,
        /// Show statistics only (no trade list)
        #[arg(long)]
        stats_only: bool,
        /// Show open trades only
        #[arg(long)]
        open_only: bool,
    },

    /// Reinforcement learning strategies (requires 'rl' feature)
    #[cfg(feature = "rl")]
    #[command(subcommand)]
    Rl(RlCommands),

    /// Run volatility arbitrage in paper trading mode (signals only, no execution)
    Paper {
        /// Symbols to monitor (comma-separated)
        #[arg(short, long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT")]
        symbols: String,

        /// Minimum volatility edge percentage
        #[arg(long, default_value = "5.0")]
        min_vol_edge: f64,

        /// Minimum price edge in cents
        #[arg(long, default_value = "2.0")]
        min_price_edge: f64,

        /// Log file path for signals
        #[arg(long, default_value = "./data/paper_signals.json")]
        log_file: String,

        /// Stats print interval (seconds)
        #[arg(long, default_value = "300")]
        stats_interval: u64,

        /// Reverse-engineered profile URL (when set, runs Shadow Pulse paper mode instead of vol-arb paper mode)
        #[arg(long)]
        reverse_profile_url: Option<String>,

        /// Poll interval (seconds) for reverse paper mode
        #[arg(long, default_value = "30")]
        reverse_poll_secs: u64,

        /// Min buy notional per synthetic order in reverse paper mode
        #[arg(long, default_value = "5.0")]
        reverse_min_trade_usdc: f64,

        /// Max notional per event in reverse paper mode
        #[arg(long, default_value = "250.0")]
        reverse_max_event_usdc: f64,

        /// Max total notional in reverse paper mode
        #[arg(long, default_value = "2000.0")]
        reverse_max_total_usdc: f64,

        /// Target assets for reverse paper mode (comma-separated keyword filter)
        #[arg(long, default_value = "Bitcoin,Ethereum,Solana,XRP")]
        reverse_target_assets: String,
    },

    /// Multi-agent platform (Coordinator + Agents)
    Platform {
        /// Subcommand: start
        #[arg(default_value = "start", value_parser = ["start"])]
        action: String,
        /// Enable crypto agent
        #[arg(long)]
        crypto: bool,
        /// Enable sports agent
        #[arg(long)]
        sports: bool,
        /// Dry run mode
        #[arg(long)]
        dry_run: bool,
        /// Pause a specific agent
        #[arg(long)]
        pause: Option<String>,
        /// Resume a specific agent
        #[arg(long)]
        resume: Option<String>,
    },

    /// Polymarket CLI (markets, orders, wallet, CTF, bridge, shell)
    Pm(super::pm::PmCli),
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
    /// Backtest crypto UP/DOWN markets (5m + 15m) using Gamma settled events + Binance spot.
    #[cfg(feature = "analysis_tools")]
    BacktestUpDown {
        /// Symbols to analyze (comma-separated: BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT)
        #[arg(long, default_value = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT")]
        symbols: String,
        /// Look back this many days of settled events
        #[arg(long, default_value = "7")]
        days: u64,
        /// Max settled events per series (cap API + runtime)
        #[arg(long, default_value = "500")]
        max_events_per_series: usize,
        /// Entry time(s) as seconds remaining (comma-separated)
        #[arg(long, default_value = "60,120,300,600,900")]
        entry_remaining_secs: String,
        /// Minimum window move filter (abs return since start) (comma-separated)
        #[arg(long, default_value = "0,0.0001,0.0002")]
        min_window_move_pcts: String,
        /// Binance kline interval (recommended: 1m)
        #[arg(long, default_value = "1m")]
        binance_interval: String,
        /// Volatility lookback in minutes (used for p_up estimate)
        #[arg(long, default_value = "60")]
        vol_lookback_minutes: usize,
        /// Use Postgres clob_orderbook_snapshots for historical best-ask (EV/PNL)
        #[arg(long)]
        use_db_prices: bool,
        /// Optional DB URL override (otherwise use PLOY_DATABASE__URL / DATABASE_URL)
        #[arg(long)]
        db_url: Option<String>,
        /// Reject DB snapshots older than this many seconds vs entry time
        #[arg(long, default_value = "120")]
        max_snapshot_age_secs: i64,
    },
    /// Deribit-implied probability arbitrage with automatic Polymarket execution
    DeribitProbArb {
        /// Symbols to trade (comma-separated: BTC,ETH)
        #[arg(long, default_value = "BTC,ETH")]
        symbols: String,
        /// Minimum net edge in cents after fee buffer
        #[arg(long, default_value = "3.0")]
        min_edge: f64,
        /// Fee/slippage buffer in cents deducted from model edge
        #[arg(long, default_value = "2.0")]
        fee_buffer: f64,
        /// Main loop poll interval in milliseconds
        #[arg(long, default_value = "2000")]
        poll_ms: u64,
        /// Surface refresh interval in seconds
        #[arg(long, default_value = "5")]
        surface_refresh_secs: u64,
        /// Market discovery refresh interval in seconds
        #[arg(long, default_value = "30")]
        discovery_refresh_secs: u64,
        /// Minimum seconds to expiry for new entries
        #[arg(long, default_value = "45")]
        min_remaining_secs: u64,
        /// Maximum seconds to expiry for new entries
        #[arg(long, default_value = "900")]
        max_remaining_secs: u64,
        /// Max acceptable bid/ask spread (bps)
        #[arg(long, default_value = "250")]
        max_spread_bps: u32,
        /// Maximum USD notional per trade
        #[arg(long, default_value = "50")]
        max_trade_usd: f64,
        /// Maximum symbol exposure in USD
        #[arg(long, default_value = "150")]
        max_symbol_exposure_usd: f64,
        /// Kelly scaling factor (0.0 - 1.0)
        #[arg(long, default_value = "0.25")]
        kelly_fraction: f64,
        /// Cooldown per condition in seconds
        #[arg(long, default_value = "45")]
        cooldown_secs: u64,
        /// Dry run mode
        #[arg(long)]
        dry_run: bool,
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
        #[arg(long, default_value = "NBA")]
        leagues: String,
        /// Dry run mode
        #[arg(long)]
        dry_run: bool,
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
        /// Polymarket market slug (e.g., "btc-price-series-15m")
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
        /// Polymarket market slug (e.g., "btc-price-series-15m")
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

        /// Optional ONNX policy model path for action selection.
        #[arg(long)]
        policy_onnx: Option<String>,

        /// How to interpret the policy model output.
        #[arg(long, default_value = "continuous")]
        policy_output: String,

        /// Optional policy model version label recorded in order metadata.
        #[arg(long)]
        policy_version: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn collect_check_only_flags_parse() {
        let cli = Cli::try_parse_from([
            "ploy",
            "collect",
            "--symbols",
            "BTCUSDT,ETHUSDT",
            "--check-only",
            "--lookback-minutes",
            "45",
            "--freshness-warn-secs",
            "90",
        ])
        .expect("collect check-only args should parse");

        match cli.command.expect("command should parse") {
            Commands::Collect {
                symbols,
                check_only,
                lookback_minutes,
                freshness_warn_secs,
                ..
            } => {
                assert_eq!(symbols, "BTCUSDT,ETHUSDT");
                assert!(check_only);
                assert_eq!(lookback_minutes, 45);
                assert_eq!(freshness_warn_secs, 90);
            }
            other => panic!("expected Collect command, got {other:?}"),
        }
    }
}
