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
use backtest_ops::{run_backtest, run_backtest_diff, run_backtest_list, run_live_backtest_compare};
use maintenance_ops::{
    backfill_klines, backfill_pm_replay_tables, backfill_pm_token_settlements, run_integrity_check,
    run_nba_comeback, seed_nba_stats,
};
use runtime_ops::{
    list_strategies, reload_strategy, show_logs, show_status, start_strategy, stop_strategy,
};
use settlement_ops::{export_crypto_lob_dataset, report_accuracy_pm_settlement};

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

    /// Backtest directional signals (signal_history) using Polymarket official settlement (token pays 1/0)
    ///
    /// Legacy alias for:
    /// `ploy strategy backtest directional --mode settlement ...`
    DirectionalSignalBacktest {
        /// Lookback window in hours (which directional signals are included)
        #[arg(long, default_value = "168")]
        lookback_hours: u64,

        /// Filter by account_id (defaults to all)
        #[arg(long)]
        account_id: Option<String>,

        /// Filter by agent_id (defaults to all)
        #[arg(long)]
        agent_id: Option<String>,

        /// Only include live signals (exclude context->>'dry_run' = true)
        #[arg(long)]
        live_only: bool,

        /// Max number of signals to backtest (latest first)
        #[arg(long, default_value = "50000")]
        limit: usize,

        /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
        #[arg(long)]
        no_refresh: bool,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Export a labeled dataset for crypto LOB model training (uses Polymarket settlement y_up).
    ExportCryptoLobDataset {
        /// Lookback window in hours (which entry intents are exported)
        #[arg(long, default_value = "168")]
        lookback_hours: u64,

        /// Filter by account_id (defaults to all)
        #[arg(long)]
        account_id: Option<String>,

        /// Filter by agent_id (defaults to all)
        #[arg(long)]
        agent_id: Option<String>,

        /// Only include live orders (exclude dry-run)
        #[arg(long)]
        live_only: bool,

        /// Skip refreshing settlement status via Gamma API (uses cached DB rows only)
        #[arg(long)]
        no_refresh: bool,

        /// Max number of intents to export (latest first)
        #[arg(long, default_value = "50000")]
        limit: usize,

        /// Output format (default: parquet if built with --features analysis, else csv)
        #[arg(long, value_enum, default_value_t = CryptoLobDatasetFormat::default())]
        format: CryptoLobDatasetFormat,

        /// Output path (default: ./data/crypto_lob_dataset.{csv|parquet})
        #[arg(long)]
        output: Option<PathBuf>,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

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
    BacktestList {
        #[arg(long)]
        database_url: Option<String>,
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Compare two backtest runs side by side
    BacktestDiff {
        run1: String,
        run2: String,
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Compare one backtest run against recent live order outcomes
    LiveBacktestCompare {
        /// Backtest run UUID
        run_id: String,

        /// Lookback window in hours for live order observations
        #[arg(long, default_value = "72")]
        lookback_hours: u64,

        /// Filter by account_id (defaults to all)
        #[arg(long)]
        account_id: Option<String>,

        /// Filter by strategy_id (defaults to all)
        #[arg(long)]
        strategy_id: Option<String>,

        /// Database URL (uses DATABASE_URL env var if omitted)
        #[arg(long)]
        database_url: Option<String>,
    },

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
            Self::DirectionalSignalBacktest {
                lookback_hours,
                account_id,
                agent_id,
                live_only,
                limit,
                no_refresh,
                database_url,
            } => {
                run_backtest(
                    "directional",
                    StrategyBacktestMode::Settlement,
                    None,
                    None,
                    "BTCUSDT,ETHUSDT,SOLUSDT",
                    10000.0,
                    false,
                    false,
                    lookback_hours,
                    account_id,
                    agent_id,
                    live_only,
                    limit,
                    no_refresh,
                    false,
                    None,
                    false,
                    database_url,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            }
            Self::ExportCryptoLobDataset {
                lookback_hours,
                account_id,
                agent_id,
                live_only,
                no_refresh,
                limit,
                format,
                output,
                database_url,
            } => {
                export_crypto_lob_dataset(
                    lookback_hours,
                    account_id,
                    agent_id,
                    live_only,
                    no_refresh,
                    limit,
                    format,
                    output,
                    database_url,
                )
                .await
            }
            Self::IntegrityCheck { json, database_url } => {
                run_integrity_check(json, database_url).await
            }
            Self::Backtest {
                name,
                mode,
                from,
                to,
                symbols,
                capital,
                save,
                json,
                lookback_hours,
                account_id,
                agent_id,
                live_only,
                limit,
                no_refresh,
                skip_gamma,
                verify_run,
                diagnose_db,
                database_url,
                lv_profile,
                lv_price_move_threshold,
                lv_volume_multiplier_threshold,
                lv_order_concentration_threshold,
                lv_entry_deviation_threshold,
                lv_entry_zscore_threshold,
                lv_take_profit_zscore_threshold,
                lv_stop_loss_zscore_threshold,
                lv_take_profit_ema_band_pct,
                lv_stop_loss_pct,
                lv_min_edge_buffer,
                lv_zscore_lookback_samples,
                lv_max_holding_secs,
                sa_entry_after_start_max_secs,
            } => {
                run_backtest(
                    &name,
                    mode,
                    from,
                    to,
                    &symbols,
                    capital,
                    save,
                    json,
                    lookback_hours,
                    account_id,
                    agent_id,
                    live_only,
                    limit,
                    no_refresh,
                    skip_gamma,
                    verify_run,
                    diagnose_db,
                    database_url,
                    Some(lv_profile),
                    lv_price_move_threshold,
                    lv_volume_multiplier_threshold,
                    lv_order_concentration_threshold,
                    lv_entry_deviation_threshold,
                    lv_entry_zscore_threshold,
                    lv_take_profit_zscore_threshold,
                    lv_stop_loss_zscore_threshold,
                    lv_take_profit_ema_band_pct,
                    lv_stop_loss_pct,
                    lv_min_edge_buffer,
                    lv_zscore_lookback_samples,
                    lv_max_holding_secs,
                    sa_entry_after_start_max_secs,
                )
                .await
            }
            Self::BacktestList {
                database_url,
                limit,
            } => run_backtest_list(database_url, limit).await,
            Self::BacktestDiff {
                run1,
                run2,
                database_url,
            } => run_backtest_diff(&run1, &run2, database_url).await,
            Self::LiveBacktestCompare {
                run_id,
                lookback_hours,
                account_id,
                strategy_id,
                database_url,
            } => {
                run_live_backtest_compare(
                    &run_id,
                    lookback_hours,
                    account_id,
                    strategy_id,
                    database_url,
                )
                .await
            }
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
