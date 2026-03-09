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
use backtest_ops::{
    run_backtest, run_backtest_diff, run_backtest_list, run_live_backtest_compare,
};
use runtime_ops::{
    list_strategies, reload_strategy, show_logs, show_status, start_strategy, stop_strategy,
};

mod backtest_ops;
mod runtime_ops;

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

#[derive(Debug, Clone)]
struct CryptoLobDatasetRow {
    executed_at: chrono::DateTime<chrono::Utc>,
    intent_id: uuid::Uuid,
    account_id: String,
    agent_id: String,
    market_slug: String,
    token_id: String,
    market_side: String,
    is_buy: bool,
    limit_price: rust_decimal::Decimal,
    p_up: Option<f64>,
    obi5: f64,
    obi10: f64,
    spread_bps: f64,
    bid_volume_5: f64,
    ask_volume_5: f64,
    momentum_1s: f64,
    momentum_5s: f64,
    pm_up_ask: Option<f64>,
    pm_down_ask: Option<f64>,
    settled_price: rust_decimal::Decimal,
    y_up: i32,
    model_type: String,
    model_version: String,
    config_hash: String,
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('\"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('\"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_crypto_lob_dataset_csv(
    output: &std::path::Path,
    rows: &[CryptoLobDatasetRow],
) -> Result<()> {
    let mut f = std::fs::File::create(output).context("Failed to create output file")?;
    writeln!(
        f,
        "executed_at,intent_id,account_id,agent_id,market_slug,token_id,market_side,is_buy,limit_price,p_up,obi5,obi10,spread_bps,bid_volume_5,ask_volume_5,momentum_1s,momentum_5s,pm_up_ask,pm_down_ask,settled_price,y_up,model_type,model_version,config_hash"
    )?;

    for r in rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&r.executed_at.to_rfc3339()),
            csv_escape(&r.intent_id.to_string()),
            csv_escape(&r.account_id),
            csv_escape(&r.agent_id),
            csv_escape(&r.market_slug),
            csv_escape(&r.token_id),
            csv_escape(&r.market_side),
            if r.is_buy { "1" } else { "0" },
            r.limit_price,
            r.p_up.map(|v| format!("{v:.6}")).unwrap_or_default(),
            format!("{:.10}", r.obi5),
            format!("{:.10}", r.obi10),
            format!("{:.10}", r.spread_bps),
            format!("{:.10}", r.bid_volume_5),
            format!("{:.10}", r.ask_volume_5),
            format!("{:.10}", r.momentum_1s),
            format!("{:.10}", r.momentum_5s),
            r.pm_up_ask.map(|v| format!("{v:.10}")).unwrap_or_default(),
            r.pm_down_ask
                .map(|v| format!("{v:.10}"))
                .unwrap_or_default(),
            r.settled_price,
            r.y_up,
            csv_escape(&r.model_type),
            csv_escape(&r.model_version),
            csv_escape(&r.config_hash),
        )?;
    }

    Ok(())
}

#[cfg(feature = "analysis")]
fn sanitize_duckdb_copy_path(path: &std::path::Path) -> std::result::Result<String, duckdb::Error> {
    let s = path.display().to_string();
    if s.contains('\'') || s.contains(';') || s.contains("--") {
        return Err(duckdb::Error::InvalidParameterName(
            "path contains SQL metacharacters".into(),
        ));
    }
    Ok(s)
}

#[cfg(feature = "analysis")]
fn write_crypto_lob_dataset_parquet(
    output: &std::path::Path,
    rows: &[CryptoLobDatasetRow],
) -> Result<()> {
    use duckdb::{params, Connection};
    use rust_decimal::prelude::ToPrimitive;

    let conn = Connection::open_in_memory().context("Failed to open DuckDB")?;
    conn.execute_batch(
        r#"
        CREATE TABLE dataset (
          executed_at VARCHAR,
          intent_id VARCHAR,
          account_id VARCHAR,
          agent_id VARCHAR,
          market_slug VARCHAR,
          token_id VARCHAR,
          market_side VARCHAR,
          is_buy BOOLEAN,
          limit_price DOUBLE,
          p_up DOUBLE,
          obi5 DOUBLE,
          obi10 DOUBLE,
          spread_bps DOUBLE,
          bid_volume_5 DOUBLE,
          ask_volume_5 DOUBLE,
          momentum_1s DOUBLE,
          momentum_5s DOUBLE,
          pm_up_ask DOUBLE,
          pm_down_ask DOUBLE,
          settled_price DOUBLE,
          y_up INTEGER,
          model_type VARCHAR,
          model_version VARCHAR,
          config_hash VARCHAR
        );
        "#,
    )
    .context("Failed to create DuckDB dataset table")?;

    let mut stmt = conn
        .prepare(
            r#"
            INSERT INTO dataset VALUES (
              ?,?,?,?,?,?,?,?,
              ?,?,?,?,?,?,?,?,
              ?,?,?,?,?,?,?,?
            )
            "#,
        )
        .context("Failed to prepare DuckDB insert statement")?;

    for r in rows {
        let limit_price = r
            .limit_price
            .to_f64()
            .context("Failed to convert limit_price to f64")?;
        let settled_price = r
            .settled_price
            .to_f64()
            .context("Failed to convert settled_price to f64")?;

        stmt.execute(params![
            r.executed_at.to_rfc3339(),
            r.intent_id.to_string(),
            r.account_id.as_str(),
            r.agent_id.as_str(),
            r.market_slug.as_str(),
            r.token_id.as_str(),
            r.market_side.as_str(),
            r.is_buy,
            limit_price,
            r.p_up,
            r.obi5,
            r.obi10,
            r.spread_bps,
            r.bid_volume_5,
            r.ask_volume_5,
            r.momentum_1s,
            r.momentum_5s,
            r.pm_up_ask,
            r.pm_down_ask,
            settled_price,
            r.y_up,
            r.model_type.as_str(),
            r.model_version.as_str(),
            r.config_hash.as_str(),
        ])
        .context("Failed to insert row into DuckDB")?;
    }

    if output.exists() {
        std::fs::remove_file(output).context("Failed to remove existing output file")?;
    }
    let out = sanitize_duckdb_copy_path(output).context("Invalid output path for DuckDB COPY")?;
    let copy_sql = format!("COPY dataset TO '{out}' (FORMAT PARQUET);");
    conn.execute_batch(&copy_sql)
        .context("Failed to COPY dataset to Parquet")?;

    Ok(())
}

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

/// Seed NBA team comeback stats into the database
async fn seed_nba_stats(season: &str, database_url: Option<String>) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::nba_comeback::nba_data_collector::TeamStats;

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

    crate::persistence::ensure_pm_token_settlements_table(store.pool())
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
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
                    continue;
                }
            }

            let clob_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default();
            let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
            let price_strs: Vec<String> = market
                .outcome_prices
                .as_ref()
                .map(|ps| ps.iter().map(|d| d.to_string()).collect())
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
            let condition_id = market.condition_id.map(|b| b.to_string());

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

    #[derive(Debug, Clone, Copy, Default)]
    struct PredAgg {
        n: usize,
        correct: usize,
        brier_sum: f64,
        logloss_sum: f64,
    }

    let mut pred_total = 0usize;
    let mut pred_correct = 0usize;
    let mut pred_brier_sum = 0.0_f64;
    let mut pred_logloss_sum = 0.0_f64;
    let mut pred_by_agent: BTreeMap<String, PredAgg> = BTreeMap::new();

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

        // Optional: prediction scoring for strategies that log p_up.
        // We score p_up against official settlement as y_up in {0,1}.
        let meta: serde_json::Value = row.try_get("metadata").unwrap_or(serde_json::Value::Null);
        let p_up_opt = meta
            .get("p_up")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|p| p.is_finite() && (0.0..=1.0).contains(p));

        if let Some(p_up) = p_up_opt {
            let market_side: String = row.get("market_side");
            let y_up: f64 = match market_side.as_str() {
                "UP" => {
                    if sp > dec!(0.5) {
                        1.0
                    } else {
                        0.0
                    }
                }
                "DOWN" => {
                    if sp > dec!(0.5) {
                        0.0
                    } else {
                        1.0
                    }
                }
                _ => continue,
            };

            let pred_label_up = p_up >= 0.5;
            let y_label_up = y_up >= 0.5;
            let correct = pred_label_up == y_label_up;

            let brier = (p_up - y_up).powi(2);
            let p = p_up.clamp(1e-6, 1.0 - 1e-6);
            let logloss = -(y_up * p.ln() + (1.0 - y_up) * (1.0 - p).ln());

            pred_total += 1;
            if correct {
                pred_correct += 1;
            }
            pred_brier_sum += brier;
            pred_logloss_sum += logloss;

            let agg = pred_by_agent.entry(row.get("agent_id")).or_default();
            agg.n += 1;
            if correct {
                agg.correct += 1;
            }
            agg.brier_sum += brier;
            agg.logloss_sum += logloss;
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

    if pred_total > 0 {
        let pred_acc = 100.0 * (pred_correct as f64) / (pred_total as f64);
        let brier = pred_brier_sum / (pred_total as f64);
        let logloss = pred_logloss_sum / (pred_total as f64);
        println!("\n  Prediction metrics (p_up vs settlement y_up):");
        println!("  - preds_scored:      {}", pred_total);
        println!("  - preds_acc@0.5:     {:.2}%", pred_acc);
        println!("  - brier_score:       {:.6}", brier);
        println!("  - log_loss:          {:.6}", logloss);

        if !pred_by_agent.is_empty() {
            println!("\n  Prediction by agent (n, acc@0.5, brier, logloss):");
            for (agent, agg) in pred_by_agent.iter() {
                if agg.n == 0 {
                    continue;
                }
                let a_acc = 100.0 * (agg.correct as f64) / (agg.n as f64);
                let a_brier = agg.brier_sum / (agg.n as f64);
                let a_ll = agg.logloss_sum / (agg.n as f64);
                println!(
                    "  - {:<20} n={:<5} acc={:>6.2}% brier={:.6} ll={:.6}",
                    agent, agg.n, a_acc, a_brier, a_ll
                );
            }
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

async fn backtest_directional_signals_pm_settlement(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use crate::adapters::PostgresStore;
    use crate::strategy::fee_model::FeeModel;
    use chrono::{DateTime, Utc};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::persistence::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;
    crate::persistence::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Directional Signal Backtest (Settlement)                     ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} account_id={} agent_id={} live_only={} limit={} refresh={}",
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh
    );

    let rows = sqlx::query(
        r#"
        SELECT
            recorded_at,
            account_id,
            agent_id,
            strategy_id,
            token_id,
            symbol,
            side,
            confidence,
            market_price,
            edge,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = 'directional_entry'
          AND ($2::text IS NULL OR account_id = $2)
          AND ($3::text IS NULL OR agent_id = $3)
          AND ($4::bool = FALSE OR COALESCE((context->>'dry_run')::bool, false) = false)
        ORDER BY recorded_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query signal_history")?;

    if rows.is_empty() {
        println!("\n  No directional signals found in this window.\n");
        return Ok(());
    }

    #[derive(Debug, Clone)]
    struct SignalRow {
        recorded_at: DateTime<Utc>,
        token_id: String,
        symbol: Option<String>,
        entry_price: Decimal,
    }

    let mut signals: Vec<SignalRow> = Vec::with_capacity(rows.len());
    let mut token_ids: Vec<String> = Vec::with_capacity(rows.len());

    for row in rows {
        let recorded_at: DateTime<Utc> = row.get("recorded_at");
        let token_id: Option<String> = row.get("token_id");
        let Some(token_id) = token_id else { continue };
        let entry_price: Option<Decimal> = row.get("market_price");
        let Some(entry_price) = entry_price else {
            continue;
        };

        let symbol: Option<String> = row.get("symbol");
        token_ids.push(token_id.clone());
        signals.push(SignalRow {
            recorded_at,
            token_id,
            symbol,
            entry_price,
        });
    }

    if signals.is_empty() {
        println!("\n  No usable signals found (missing token_id/market_price).\n");
        return Ok(());
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
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
                    continue;
                }
            }

            let clob_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default();
            let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
            let price_strs: Vec<String> = market
                .outcome_prices
                .as_ref()
                .map(|ps| ps.iter().map(|d| d.to_string()).collect())
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
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                if let Err(e) = sqlx::query(
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
                {
                    warn!(token_id = %token_id, error = %e, "failed to upsert pm_token_settlements row");
                    continue;
                }

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows\n",
                refreshed_markets, refreshed_tokens
            );
        }
    }

    let settlement_rows = sqlx::query(
        r#"
        SELECT token_id, resolved, settled_price, resolved_at
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(&token_ids)
    .fetch_all(store.pool())
    .await
    .context("Failed to query pm_token_settlements for signal tokens")?;

    #[derive(Debug, Clone)]
    struct SettlementRow {
        resolved: bool,
        settled_price: Option<Decimal>,
    }

    let mut settlements: HashMap<String, SettlementRow> = HashMap::new();
    for row in settlement_rows {
        let token_id: String = row.get("token_id");
        settlements.insert(
            token_id,
            SettlementRow {
                resolved: row.get("resolved"),
                settled_price: row.get("settled_price"),
            },
        );
    }

    let fee_model = FeeModel::crypto();
    let spread_cost = dec!(0.01);

    let mut total = 0usize;
    let mut resolved = 0usize;
    let mut wins = 0usize;
    let mut sum_pnl = 0.0f64;
    let mut equity = 0.0f64;
    let mut peak = 0.0f64;
    let mut max_dd = 0.0f64;

    let mut by_symbol: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new(); // (n, wins, pnl_sum)

    // Process oldest→newest for drawdown.
    signals.sort_by_key(|s| s.recorded_at);
    for s in &signals {
        total += 1;

        let entry_price_f64 = s.entry_price.to_f64().unwrap_or(0.0);
        let fee_rate = fee_model.effective_rate(s.entry_price);
        let fee_per_share = (s.entry_price * fee_rate).to_f64().unwrap_or(0.0);
        let costs = fee_per_share + spread_cost.to_f64().unwrap_or(0.01);

        let Some(settlement) = settlements.get(&s.token_id) else {
            continue;
        };
        if !settlement.resolved {
            continue;
        }
        let Some(settled_price) = settlement.settled_price else {
            continue;
        };

        resolved += 1;
        let payout = settled_price.to_f64().unwrap_or(0.0);
        let win = payout >= 0.99;
        if win {
            wins += 1;
        }

        let pnl = payout - entry_price_f64 - costs;
        sum_pnl += pnl;
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }

        let sym = s.symbol.clone().unwrap_or_else(|| "UNKNOWN".to_string());
        let entry = by_symbol.entry(sym).or_insert((0, 0, 0.0));
        entry.0 += 1;
        if win {
            entry.1 += 1;
        }
        entry.2 += pnl;
    }

    if resolved == 0 {
        println!("\n  Signals: {} (0 resolved yet). Wait for settlements, or run with longer lookback.\n", total);
        return Ok(());
    }

    let win_rate = wins as f64 / resolved as f64;
    let avg_pnl = sum_pnl / resolved as f64;

    println!(
        "\n  Signals: {} (resolved: {}) | Win rate: {:.1}% | Avg PnL/share: {:+.4} | Total PnL: {:+.4} | Max DD: {:.4}\n",
        total,
        resolved,
        win_rate * 100.0,
        avg_pnl,
        sum_pnl,
        max_dd
    );

    println!("  By symbol (resolved only):");
    for (sym, (n, w, pnl_sum)) in by_symbol {
        if n == 0 {
            continue;
        }
        println!(
            "    {:<8} n={:<5} win={:>5.1}% pnl_sum={:+.4} avg={:+.4}",
            sym,
            n,
            (w as f64 / n as f64) * 100.0,
            pnl_sum,
            pnl_sum / n as f64
        );
    }

    Ok(())
}

async fn export_crypto_lob_dataset(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    no_refresh: bool,
    limit: usize,
    format: CryptoLobDatasetFormat,
    output: Option<PathBuf>,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use anyhow::bail;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let output: PathBuf = output.unwrap_or_else(|| match format {
        CryptoLobDatasetFormat::Csv => PathBuf::from("./data/crypto_lob_dataset.csv"),
        CryptoLobDatasetFormat::Parquet => PathBuf::from("./data/crypto_lob_dataset.parquet"),
    });

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Export Dataset (crypto LOB)                                  ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} account_id={} agent_id={} live_only={} limit={} refresh={} format={:?} output={}",
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh,
        format,
        output.display()
    );

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::persistence::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

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
          AND LOWER(domain) = 'crypto'
          AND ($2::text IS NULL OR account_id = $2)
          AND ($3::text IS NULL OR agent_id = $3)
          AND ($4::bool = FALSE OR dry_run = FALSE)
        ORDER BY executed_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
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
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
                    continue;
                }
            }

            let clob_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default();
            let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
            let price_strs: Vec<String> = market
                .outcome_prices
                .as_ref()
                .map(|ps| ps.iter().map(|d| d.to_string()).collect())
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

            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

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

    let scored_rows = sqlx::query(
        r#"
        SELECT
            e.executed_at,
            e.intent_id,
            e.agent_id,
            e.account_id,
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
          AND LOWER(e.domain) = 'crypto'
          AND ($2::text IS NULL OR e.account_id = $2)
          AND ($3::text IS NULL OR e.agent_id = $3)
          AND ($4::bool = FALSE OR e.dry_run = FALSE)
        ORDER BY e.executed_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query joined export rows")?;

    if scored_rows.is_empty() {
        bail!("no rows returned for export query (unexpected)");
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).context("Failed to create output directory")?;
        }
    }

    fn meta_f64(meta: &serde_json::Value, key: &str) -> Option<f64> {
        let v = meta.get(key)?;
        match v {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
        .filter(|x| x.is_finite())
    }

    fn meta_str<'a>(meta: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        meta.get(key).and_then(|v| v.as_str())
    }

    let mut dataset: Vec<CryptoLobDatasetRow> = Vec::new();
    let mut skipped_pending = 0usize;
    let mut skipped_missing = 0usize;

    for row in &scored_rows {
        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let is_resolved = resolved.unwrap_or(false) && settled_price.is_some();
        if !is_resolved {
            skipped_pending += 1;
            continue;
        }

        let sp = settled_price.unwrap_or(Decimal::ZERO);
        let market_side: String = row.get("market_side");
        let y_up: i32 = match market_side.as_str() {
            "UP" => {
                if sp > dec!(0.5) {
                    1
                } else {
                    0
                }
            }
            "DOWN" => {
                if sp > dec!(0.5) {
                    0
                } else {
                    1
                }
            }
            _ => continue,
        };

        let meta: serde_json::Value = row.try_get("metadata").unwrap_or(serde_json::Value::Null);

        let p_up = meta_f64(&meta, "p_up");
        let obi5 = meta_f64(&meta, "lob_obi_5");
        let obi10 = meta_f64(&meta, "lob_obi_10");
        let spread = meta_f64(&meta, "lob_spread_bps");
        let bidv5 = meta_f64(&meta, "lob_bid_volume_5");
        let askv5 = meta_f64(&meta, "lob_ask_volume_5");
        let m1 = meta_f64(&meta, "signal_momentum_1s");
        let m5 = meta_f64(&meta, "signal_momentum_5s");
        let pm_up_ask = meta_f64(&meta, "pm_up_ask");
        let pm_down_ask = meta_f64(&meta, "pm_down_ask");
        let model_type = meta_str(&meta, "model_type").unwrap_or("").to_string();
        let model_version = meta_str(&meta, "model_version").unwrap_or("").to_string();
        let config_hash = meta_str(&meta, "config_hash").unwrap_or("").to_string();

        // Require core features for training a DL model (MLP).
        if obi5.is_none()
            || obi10.is_none()
            || spread.is_none()
            || bidv5.is_none()
            || askv5.is_none()
            || m1.is_none()
            || m5.is_none()
        {
            skipped_missing += 1;
            continue;
        }

        let executed_at: DateTime<Utc> = row.get("executed_at");
        let intent_id: uuid::Uuid = row.get("intent_id");
        let account_id: String = row.get("account_id");
        let agent_id: String = row.get("agent_id");
        let market_slug: String = row.get("market_slug");
        let token_id: String = row.get("token_id");
        let is_buy: bool = row.get("is_buy");
        let limit_price: Decimal = row.get("limit_price");

        dataset.push(CryptoLobDatasetRow {
            executed_at,
            intent_id,
            account_id,
            agent_id,
            market_slug,
            token_id,
            market_side,
            is_buy,
            limit_price,
            p_up,
            obi5: obi5.unwrap_or(0.0),
            obi10: obi10.unwrap_or(0.0),
            spread_bps: spread.unwrap_or(0.0),
            bid_volume_5: bidv5.unwrap_or(0.0),
            ask_volume_5: askv5.unwrap_or(0.0),
            momentum_1s: m1.unwrap_or(0.0),
            momentum_5s: m5.unwrap_or(0.0),
            pm_up_ask,
            pm_down_ask,
            settled_price: sp,
            y_up,
            model_type,
            model_version,
            config_hash,
        });
    }

    if dataset.is_empty() {
        println!("\n  No resolved rows to export (all pending/missing features).\n");
        return Ok(());
    }

    match format {
        CryptoLobDatasetFormat::Csv => write_crypto_lob_dataset_csv(output.as_path(), &dataset)?,
        CryptoLobDatasetFormat::Parquet => {
            #[cfg(feature = "analysis")]
            {
                write_crypto_lob_dataset_parquet(output.as_path(), &dataset)?;
            }
            #[cfg(not(feature = "analysis"))]
            {
                bail!("parquet export requires building with --features analysis");
            }
        }
    }

    println!("\n  Export complete:");
    println!("  - exported_rows:    {}", dataset.len());
    println!("  - skipped_pending:  {}", skipped_pending);
    println!("  - skipped_missing:  {}", skipped_missing);
    println!("  - output:           {}", output.display());
    println!();

    Ok(())
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

/// The standalone NBA comeback runtime has been retired in favor of managed deployments.
async fn run_nba_comeback(_config: Option<PathBuf>, _dry_run: bool) -> Result<()> {
    anyhow::bail!(
        "standalone `ploy strategy nba-comeback` runtime is retired; use canonical managed strategy deployments via `ploy platform start`"
    )
}

// ─────────────────────────────────────────────────────────────
// Integrity Check handler
// ─────────────────────────────────────────────────────────────

async fn run_integrity_check(json_output: bool, database_url: Option<String>) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::integrity::IntegrityChecker;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    let store = PostgresStore::new(&db_url, 5).await?;
    let checker = IntegrityChecker::new(store.pool().clone());
    let report = checker.run_full_check().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report);
    }

    if !report.healthy {
        std::process::exit(1);
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backfill handlers
// ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn backfill_klines(
    symbols: &str,
    from: &str,
    to: &str,
    interval: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::collector::BinanceKlineClient;
    use chrono::DateTime;

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();
    if symbol_list.is_empty() {
        anyhow::bail!("No symbols provided");
    }

    let from_dt = DateTime::parse_from_rfc3339(from)
        .or_else(|_| DateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --from date (expected ISO 8601, e.g. 2026-02-20T00:00:00Z)")?;

    let to_dt = DateTime::parse_from_rfc3339(to)
        .or_else(|_| DateTime::parse_from_str(to, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --to date (expected ISO 8601, e.g. 2026-02-28T00:00:00Z)")?;

    if to_dt <= from_dt {
        anyhow::bail!("--to must be after --from");
    }

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    // Ensure binance_klines table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_klines (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            interval TEXT NOT NULL,
            open_time TIMESTAMPTZ NOT NULL,
            close_time TIMESTAMPTZ NOT NULL,
            open NUMERIC(20,10) NOT NULL,
            high NUMERIC(20,10) NOT NULL,
            low NUMERIC(20,10) NOT NULL,
            close NUMERIC(20,10) NOT NULL,
            volume NUMERIC(20,10) NOT NULL,
            quote_volume NUMERIC(20,10) NOT NULL,
            trades BIGINT NOT NULL DEFAULT 0,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (symbol, interval, open_time)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure binance_klines table")?;

    let client = BinanceKlineClient::new();

    println!(
        "\nBackfilling klines: {} symbols, interval={}, {} → {}",
        symbol_list.len(),
        interval,
        from_dt.format("%Y-%m-%d"),
        to_dt.format("%Y-%m-%d")
    );

    let mut grand_total = 0usize;
    for sym in &symbol_list {
        print!("  {} ... ", sym);
        std::io::stdout().flush().ok();

        let klines = client
            .fetch_klines_range(sym, interval, from_dt, to_dt)
            .await
            .with_context(|| format!("Failed to fetch klines for {}", sym))?;

        let fetched = klines.len();
        let saved = BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines)
            .await
            .with_context(|| format!("Failed to save klines for {}", sym))?;

        println!("{} fetched, {} new", fetched, saved);
        grand_total += saved;
    }

    println!("\nDone. {} new klines inserted total.\n", grand_total);
    Ok(())
}

/// Backfill PM replay tables from sync_records:
/// - clob_quote_ticks
/// - clob_orderbook_snapshots (synthetic depth from prices)
/// - pm_market_metadata
async fn backfill_pm_replay_tables(
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    synthetic_depth: u64,
    database_url: Option<String>,
) -> Result<()> {
    use chrono::DateTime;

    let from_dt = from
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --from date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let to_dt = to
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --to date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&chrono::Utc));

    if let (Some(f), Some(t)) = (from_dt, to_dt) {
        if t <= f {
            anyhow::bail!("--to must be after --from");
        }
    }

    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let symbols_param = if symbol_list.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbol_list.clone())
    };

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    // Ensure required tables exist (idempotent), centralized with runtime path.
    crate::platform::persistence_schema::ensure_clob_quote_ticks_table(pool)
        .await
        .context("Failed to ensure clob_quote_ticks table")?;
    crate::platform::persistence_schema::ensure_clob_orderbook_snapshots_table(pool)
        .await
        .context("Failed to ensure clob_orderbook_snapshots table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_market_metadata (
            market_slug TEXT PRIMARY KEY,
            price_to_beat NUMERIC(20,8) NOT NULL,
            start_time TIMESTAMPTZ,
            end_time TIMESTAMPTZ,
            horizon TEXT,
            symbol TEXT,
            raw_market JSONB,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure pm_market_metadata table")?;

    println!("\nBackfilling PM replay tables from sync_records...");
    println!(
        "  symbols: {}",
        if symbol_list.is_empty() {
            "(all)".to_string()
        } else {
            symbol_list.join(",")
        }
    );
    println!(
        "  from: {}",
        from_dt
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  to:   {}",
        to_dt
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );

    let quote_up = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_yes_token_id)
                sr.timestamp AS received_at,
                sr.pm_yes_token_id AS token_id,
                sr.pm_yes_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND sr.pm_yes_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_yes_token_id
        )
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
        )
        SELECT
            src.token_id,
            'UP',
            GREATEST(src.best_ask - 0.01, 0.0001)::NUMERIC(10,6),
            src.best_ask::NUMERIC(10,6),
            NULL,
            NULL,
            'sync_backfill',
            src.received_at,
            'crypto'
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_quote_ticks q
            WHERE q.token_id = src.token_id
              AND q.side = 'UP'
              AND q.received_at = src.received_at
              AND q.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill clob_quote_ticks UP rows")?
    .rows_affected();

    let quote_down = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_no_token_id)
                sr.timestamp AS received_at,
                sr.pm_no_token_id AS token_id,
                sr.pm_no_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND sr.pm_no_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_no_token_id
        )
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
        )
        SELECT
            src.token_id,
            'DOWN',
            GREATEST(src.best_ask - 0.01, 0.0001)::NUMERIC(10,6),
            src.best_ask::NUMERIC(10,6),
            NULL,
            NULL,
            'sync_backfill',
            src.received_at,
            'crypto'
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_quote_ticks q
            WHERE q.token_id = src.token_id
              AND q.side = 'DOWN'
              AND q.received_at = src.received_at
              AND q.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill clob_quote_ticks DOWN rows")?
    .rows_affected();

    let md_rows = sqlx::query(
        r#"
        WITH agg AS (
            SELECT
                sr.pm_market_slug AS market_slug,
                (array_agg(sr.symbol ORDER BY sr.timestamp ASC))[1] AS symbol,
                MIN(sr.timestamp) AS start_time,
                MAX(sr.timestamp) AS observed_end_time,
                (array_agg(sr.bn_mid_price ORDER BY sr.timestamp ASC))[1] AS price_to_beat
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            GROUP BY sr.pm_market_slug
        )
        INSERT INTO pm_market_metadata (
            market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at
        )
        SELECT
            market_slug,
            COALESCE(price_to_beat, 0),
            start_time,
            CASE
                WHEN market_slug LIKE '%-5m-%' THEN start_time + INTERVAL '5 minutes'
                WHEN market_slug LIKE '%-15m-%' THEN start_time + INTERVAL '15 minutes'
                WHEN market_slug LIKE '%-60m-%' THEN start_time + INTERVAL '60 minutes'
                ELSE observed_end_time
            END AS end_time,
            CASE
                WHEN market_slug LIKE '%-5m-%' THEN '5m'
                WHEN market_slug LIKE '%-15m-%' THEN '15m'
                WHEN market_slug LIKE '%-60m-%' THEN '60m'
                ELSE NULL
            END AS horizon,
            symbol,
            jsonb_build_object(
                'source', 'sync_backfill',
                'derived_from', 'sync_records',
                'market_slug', market_slug,
                'symbol', symbol
            ),
            NOW()
        FROM agg
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = CASE
                WHEN EXCLUDED.price_to_beat > 0 THEN EXCLUDED.price_to_beat
                ELSE pm_market_metadata.price_to_beat
            END,
            start_time = COALESCE(pm_market_metadata.start_time, EXCLUDED.start_time),
            end_time = COALESCE(pm_market_metadata.end_time, EXCLUDED.end_time),
            horizon = COALESCE(pm_market_metadata.horizon, EXCLUDED.horizon),
            symbol = COALESCE(pm_market_metadata.symbol, EXCLUDED.symbol),
            raw_market = COALESCE(pm_market_metadata.raw_market, EXCLUDED.raw_market),
            updated_at = NOW()
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill pm_market_metadata")?
    .rows_affected();

    let ob_up = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_yes_token_id)
                sr.timestamp AS received_at,
                sr.pm_market_slug AS market_slug,
                sr.pm_yes_token_id AS token_id,
                sr.pm_yes_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND sr.pm_yes_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_yes_token_id
        )
        INSERT INTO clob_orderbook_snapshots (
            domain, token_id, market, bids, asks, book_timestamp, hash, source, context, received_at
        )
        SELECT
            'crypto',
            src.token_id,
            src.market_slug,
            jsonb_build_array(
                jsonb_build_object(
                    'price', GREATEST((1 - src.best_ask), 0.0001)::text,
                    'size', $4::text
                )
            ),
            jsonb_build_array(
                jsonb_build_object(
                    'price', src.best_ask::text,
                    'size', $4::text
                )
            ),
            src.received_at,
            NULL,
            'sync_backfill',
            jsonb_build_object('synthetic', true, 'side', 'UP', 'source', 'sync_records'),
            src.received_at
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_orderbook_snapshots s
            WHERE s.token_id = src.token_id
              AND s.received_at = src.received_at
              AND s.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(synthetic_depth as i64)
    .execute(pool)
    .await
    .context("Failed to backfill clob_orderbook_snapshots UP rows")?
    .rows_affected();

    let ob_down = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_no_token_id)
                sr.timestamp AS received_at,
                sr.pm_market_slug AS market_slug,
                sr.pm_no_token_id AS token_id,
                sr.pm_no_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND sr.pm_no_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_no_token_id
        )
        INSERT INTO clob_orderbook_snapshots (
            domain, token_id, market, bids, asks, book_timestamp, hash, source, context, received_at
        )
        SELECT
            'crypto',
            src.token_id,
            src.market_slug,
            jsonb_build_array(
                jsonb_build_object(
                    'price', GREATEST((1 - src.best_ask), 0.0001)::text,
                    'size', $4::text
                )
            ),
            jsonb_build_array(
                jsonb_build_object(
                    'price', src.best_ask::text,
                    'size', $4::text
                )
            ),
            src.received_at,
            NULL,
            'sync_backfill',
            jsonb_build_object('synthetic', true, 'side', 'DOWN', 'source', 'sync_records'),
            src.received_at
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_orderbook_snapshots s
            WHERE s.token_id = src.token_id
              AND s.received_at = src.received_at
              AND s.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(synthetic_depth as i64)
    .execute(pool)
    .await
    .context("Failed to backfill clob_orderbook_snapshots DOWN rows")?
    .rows_affected();

    let quote_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM clob_quote_ticks
        WHERE source = 'sync_backfill'
          AND ($1::timestamptz IS NULL OR received_at >= $1)
          AND ($2::timestamptz IS NULL OR received_at <= $2)
        "#,
    )
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let ob_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM clob_orderbook_snapshots
        WHERE source = 'sync_backfill'
          AND ($1::timestamptz IS NULL OR received_at >= $1)
          AND ($2::timestamptz IS NULL OR received_at <= $2)
        "#,
    )
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let md_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM pm_market_metadata
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2)
          AND ($3::timestamptz IS NULL OR start_time <= $3)
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    println!("\nBackfill complete:");
    println!(
        "  clob_quote_ticks inserted: {} (UP {}) + (DOWN {})",
        quote_up + quote_down,
        quote_up,
        quote_down
    );
    println!(
        "  clob_orderbook_snapshots inserted: {} (UP {}) + (DOWN {})",
        ob_up + ob_down,
        ob_up,
        ob_down
    );
    println!("  pm_market_metadata upsert affected rows: {}", md_rows);
    println!("\nCurrent totals in selected window:");
    println!("  clob_quote_ticks (sync_backfill): {}", quote_total);
    println!("  clob_orderbook_snapshots (sync_backfill): {}", ob_total);
    println!("  pm_market_metadata: {}", md_total);
    println!();

    Ok(())
}

/// Backfill official Polymarket token settlements into pm_token_settlements.
///
/// Source of token universe:
/// - Distinct pm_yes_token_id / pm_no_token_id from sync_records in selected window.
///
/// Source of official settlement state:
/// - Gamma market lookup by token_id via Polymarket API.
async fn backfill_pm_token_settlements(
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    limit: usize,
    database_url: Option<String>,
) -> Result<()> {
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    use crate::adapters::PolymarketClient;

    let from_dt = from
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --from date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&Utc));

    let to_dt = to
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .context("Invalid --to date (use ISO 8601 format)")?
        .map(|dt| dt.with_timezone(&Utc));

    if let (Some(f), Some(t)) = (from_dt, to_dt) {
        if t <= f {
            anyhow::bail!("--to must be after --from");
        }
    }

    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let symbols_param = if symbol_list.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbol_list.clone())
    };

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    crate::persistence::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    println!("\nBackfilling pm_token_settlements from Gamma...");
    println!(
        "  symbols: {}",
        if symbol_list.is_empty() {
            "(all)".to_string()
        } else {
            symbol_list.join(",")
        }
    );
    println!(
        "  from: {}",
        from_dt
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  to:   {}",
        to_dt
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("  token limit: {}", limit);

    // Candidate token universe from sync_records.
    let token_ids: Vec<String> = sqlx::query_scalar(
        r#"
        WITH tokens AS (
            SELECT sr.pm_yes_token_id AS token_id
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            UNION
            SELECT sr.pm_no_token_id AS token_id
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
        )
        SELECT token_id
        FROM tokens
        ORDER BY token_id
        LIMIT $4
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .context("Failed to query token_ids from sync_records")?;

    if token_ids.is_empty() {
        println!("  No token_ids found in sync_records for selected window.\n");
        return Ok(());
    }

    // token_id -> market_slug fallback mapping from sync_records
    let token_slug_rows = sqlx::query(
        r#"
        SELECT DISTINCT pm_market_slug, pm_yes_token_id, pm_no_token_id
        FROM sync_records
        WHERE pm_market_slug IS NOT NULL
          AND ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR timestamp >= $2)
          AND ($3::timestamptz IS NULL OR timestamp <= $3)
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_all(pool)
    .await
    .context("Failed to query token↔slug map from sync_records")?;

    let mut token_to_slug: HashMap<String, String> = HashMap::new();
    for row in &token_slug_rows {
        let slug: Option<String> = row.try_get("pm_market_slug").ok();
        let yes: Option<String> = row.try_get("pm_yes_token_id").ok();
        let no: Option<String> = row.try_get("pm_no_token_id").ok();
        let Some(slug) = slug else { continue };
        if let Some(t) = yes {
            token_to_slug.entry(t).or_insert_with(|| slug.clone());
        }
        if let Some(t) = no {
            token_to_slug.entry(t).or_insert_with(|| slug.clone());
        }
    }

    // slug -> event end_time fallback for resolved_at (chronologically better than Utc::now()).
    let md_rows = sqlx::query(
        r#"
        SELECT market_slug, end_time
        FROM pm_market_metadata
        WHERE end_time IS NOT NULL
          AND ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2)
          AND ($3::timestamptz IS NULL OR end_time <= $3 + INTERVAL '1 day')
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_all(pool)
    .await
    .context("Failed to query pm_market_metadata end_time map")?;

    let mut slug_to_end: HashMap<String, DateTime<Utc>> = HashMap::new();
    for row in &md_rows {
        let slug: String = row.get("market_slug");
        let end_time: DateTime<Utc> = row.get("end_time");
        slug_to_end.entry(slug).or_insert(end_time);
    }

    let existing_rows = sqlx::query(
        r#"
        SELECT token_id, resolved
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(&token_ids)
    .fetch_all(pool)
    .await
    .context("Failed to query existing pm_token_settlements rows")?;

    let mut resolved_map: HashMap<String, bool> = HashMap::new();
    for row in existing_rows {
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        resolved_map.insert(token_id, resolved);
    }

    let mut to_refresh: Vec<String> = token_ids
        .iter()
        .filter(|t| !resolved_map.get(*t).copied().unwrap_or(false))
        .cloned()
        .collect();
    to_refresh.sort();
    to_refresh.dedup();

    if to_refresh.is_empty() {
        println!("  All candidate tokens already marked resolved.\n");
        return Ok(());
    }

    println!("  refreshing {} token(s) via Gamma...", to_refresh.len());

    let pm = PolymarketClient::new("https://clob.polymarket.com", true)
        .context("Failed to create Polymarket client")?;

    let mut seen_conditions: HashSet<String> = HashSet::new();
    let mut refreshed_markets = 0usize;
    let mut upserted_rows = 0usize;
    let mut resolved_rows = 0usize;

    for token_id in to_refresh {
        let market = match pm.get_gamma_market_by_token_id(&token_id).await {
            Ok(m) => m,
            Err(e) => {
                warn!(token_id = %token_id, error = %e, "Gamma fetch failed for token");
                continue;
            }
        };

        if let Some(ref cond) = market.condition_id {
            let cond_str = cond.to_string();
            if !seen_conditions.insert(cond_str) {
                continue;
            }
        }

        let clob_ids: Vec<String> = market
            .clob_token_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.to_string()).collect())
            .unwrap_or_default();
        let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
        let price_strs: Vec<String> = market
            .outcome_prices
            .as_ref()
            .map(|ps| ps.iter().map(|d| d.to_string()).collect())
            .unwrap_or_default();

        if clob_ids.is_empty() || price_strs.is_empty() {
            continue;
        }

        let mut prices: Vec<Decimal> = Vec::new();
        for s in &price_strs {
            if let Ok(p) = s.parse::<Decimal>() {
                prices.push(p);
            }
        }

        let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
        let condition_id = market.condition_id.map(|b| b.to_string());

        for (i, tid) in clob_ids.iter().enumerate() {
            let outcome = outcomes.get(i).cloned();
            let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

            let slug_fallback = token_to_slug.get(tid).cloned();
            let market_slug = market.slug.clone().or(slug_fallback);
            let resolved_at = if resolved {
                market_slug
                    .as_ref()
                    .and_then(|slug| slug_to_end.get(slug).cloned())
                    .or_else(|| Some(Utc::now()))
            } else {
                None
            };

            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

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
            .execute(pool)
            .await
            .context("Failed to upsert pm_token_settlements row")?;

            upserted_rows += 1;
            if resolved {
                resolved_rows += 1;
            }
        }

        refreshed_markets += 1;
    }

    let summary = sqlx::query(
        r#"
        SELECT
          COUNT(*)::bigint AS total_rows,
          COUNT(*) FILTER (WHERE resolved)::bigint AS resolved_rows,
          MIN(resolved_at) AS min_resolved_at,
          MAX(resolved_at) AS max_resolved_at
        FROM pm_token_settlements
        "#,
    )
    .fetch_one(pool)
    .await
    .context("Failed to query settlement summary")?;

    let total_rows: i64 = summary.get("total_rows");
    let total_resolved: i64 = summary.get("resolved_rows");
    let min_resolved: Option<DateTime<Utc>> = summary.try_get("min_resolved_at").ok();
    let max_resolved: Option<DateTime<Utc>> = summary.try_get("max_resolved_at").ok();

    println!("\nBackfill complete:");
    println!("  refreshed markets: {}", refreshed_markets);
    println!("  upserted token rows: {}", upserted_rows);
    println!("  upserted resolved rows: {}", resolved_rows);
    println!("  table total rows: {}", total_rows);
    println!("  table total resolved rows: {}", total_resolved);
    println!(
        "  resolved_at range: {} .. {}",
        min_resolved
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        max_resolved
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!();

    Ok(())
}
