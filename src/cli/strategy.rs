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
use crate::strategy::executor::OrderExecutor;
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
    Backtest(BacktestArgs),

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
