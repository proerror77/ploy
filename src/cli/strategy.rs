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

/// Get the config directory path
fn config_dir() -> PathBuf {
    std::env::var("PLOY_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/config"))
}

/// Get the run directory for PID files
fn run_dir() -> PathBuf {
    std::env::var("PLOY_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/run"))
}

/// Get the log directory
fn log_dir() -> PathBuf {
    std::env::var("PLOY_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/ploy/logs"))
}

/// List all available strategies
async fn list_strategies() -> Result<()> {
    let strategies_dir = config_dir().join("strategies");

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Available Strategies                                         ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    // Get strategies from factory
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

    // Check for custom strategy configs
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
                        // Skip default configs
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

async fn start_strategy(
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

    // Check if already running.
    //
    // NOTE: when invoked as a systemd service (ExecStart), the unit can appear "active"
    // while we are starting. In that case, `get_strategy_status()` would detect the unit
    // and we'd exit immediately, causing a restart loop. Skip the check under systemd.
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

    // Find config file
    let config_path = config.unwrap_or_else(|| {
        config_dir()
            .join("strategies")
            .join(format!("{}.toml", name))
    });

    if !config_path.exists() {
        // Try to use default config
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
        // Run in foreground - exec directly
        run_strategy_foreground(name, &config_path, dry_run).await
    } else {
        // Run as daemon
        run_strategy_daemon(name, &config_path, dry_run).await
    }
}

/// Run strategy in foreground using StrategyManager
async fn run_strategy_foreground(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    use crate::adapters::PolymarketWebSocket;
    use crate::strategy::DataFeedManager;

    // Load config
    let config_content = fs::read_to_string(config_path)
        .context(format!("Failed to read config: {}", config_path.display()))?;

    println!(
        "\x1b[32m▶ Running {} in foreground (Ctrl+C to stop)\x1b[0m\n",
        name
    );

    // Create strategy via factory
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

    // Create order executor (authenticated client for live trading)
    let executor = if dry_run {
        println!("  \x1b[33m⚠ DRY RUN MODE - Orders will be simulated\x1b[0m");
        let client = PolymarketClient::new("https://clob.polymarket.com", true)?;
        Some(Arc::new(OrderExecutor::new(
            client,
            ExecutionConfig::default(),
        )))
    } else {
        // For live trading, need authenticated client
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
                        true, // neg_risk: crypto binary options use NegRisk exchange
                    )
                    .await
                } else {
                    PolymarketClient::new_authenticated(
                        "https://clob.polymarket.com",
                        wallet,
                        true, // neg_risk: crypto binary options use NegRisk exchange
                    )
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

    // Create strategy manager
    let manager = Arc::new(StrategyManager::new(1000)); // 1 second tick interval

    // Take the action receiver before starting strategy
    let action_rx = manager
        .take_action_receiver()
        .await
        .expect("Action receiver should be available");

    // Extract Binance feed requirements from strategy feeds
    let mut binance_spot_symbols: Vec<String> = Vec::new();
    let mut binance_kline_symbols: Vec<String> = Vec::new();
    let mut binance_kline_intervals: Vec<String> = Vec::new();
    let mut binance_kline_closed_only: bool = true;

    for f in &required_feeds {
        match f {
            crate::strategy::DataFeed::BinanceSpot { symbols } => {
                binance_spot_symbols.extend(symbols.clone());
            }
            crate::strategy::DataFeed::BinanceKlines {
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

    // Create data feed manager with required feeds
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
            binance_kline_symbols, binance_kline_intervals, binance_kline_closed_only, backfill_limit
        );
        feed_manager = feed_manager.with_binance_klines(
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_closed_only,
            backfill_limit,
        );
    }

    // Configure Polymarket if needed
    let has_polymarket_feed = required_feeds.iter().any(|f| {
        matches!(
            f,
            crate::strategy::DataFeed::PolymarketEvents { .. }
                | crate::strategy::DataFeed::PolymarketQuotes { .. }
        )
    });

    if has_polymarket_feed {
        println!("  \x1b[36mConfiguring Polymarket feed\x1b[0m");
        let pm_client = PolymarketClient::new("https://clob.polymarket.com", dry_run)?;
        let pm_ws =
            PolymarketWebSocket::new("wss://ws-subscriptions-clob.polymarket.com/ws/market");
        feed_manager = feed_manager.with_polymarket(pm_ws, pm_client);
    }

    // Start the strategy
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

    // Start data feeds
    println!("  \x1b[36mStarting data feeds...\x1b[0m");
    feed_manager.start().await?;

    // Discover and subscribe to events based on strategy feeds
    let tokens = feed_manager.start_for_feeds(required_feeds).await?;
    if !tokens.is_empty() {
        println!("  \x1b[36mSubscribed to {} tokens\x1b[0m", tokens.len());
    }

    println!("\x1b[32m✓ Data feeds started\x1b[0m\n");

    // Initialize optional order persistence for strategy orders
    let order_store = init_order_store().await;

    // Spawn action handler task with executor + optional order store
    let action_handle = tokio::spawn(handle_strategy_actions(action_rx, executor, order_store));

    // Wait for shutdown signal
    println!("Press Ctrl+C to stop...\n");
    tokio::signal::ctrl_c().await?;

    println!("\n\x1b[33m⚠ Shutdown signal received\x1b[0m");

    // Graceful shutdown
    println!("Stopping strategy gracefully...");
    manager
        .stop_strategy(&strategy_id, true)
        .await
        .context("Failed to stop strategy")?;

    // Cancel action handler
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

/// Handle actions emitted by strategies
async fn handle_strategy_actions(
    mut rx: tokio::sync::mpsc::Receiver<(String, crate::strategy::StrategyAction)>,
    executor: Option<Arc<OrderExecutor>>,
    store: Option<Arc<PostgresStore>>,
) {
    use crate::strategy::{StrategyAction, StrategyControlAction};

    while let Some((strategy_id, action)) = rx.recv().await {
        match action {
            StrategyAction::SubmitIntent { intent } => {
                let client_order_id = intent.client_order_id.clone();
                let mut order = intent.into_order_request();
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
                println!("  \x1b[36m║\x1b[0m  Side: {:?}, Shares: {}, Price: {:.2}¢{:<20}\x1b[36m║\x1b[0m",
                    order.market_side, order.shares, price_cents, "");
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

                // Execute order if executor is available
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
            StrategyAction::LegacyControl(control) => match control {
                StrategyControlAction::UpdateRisk { level, reason } => {
                    println!(
                        "  \x1b[35m[{}]\x1b[0m Risk: {:?} - {}",
                        strategy_id, level, reason
                    );
                }
                StrategyControlAction::SubscribeFeed { feed } => {
                    println!("  \x1b[90m[{}]\x1b[0m Subscribe: {:?}", strategy_id, feed);
                }
                StrategyControlAction::UnsubscribeFeed { feed } => {
                    println!("  \x1b[90m[{}]\x1b[0m Unsubscribe: {:?}", strategy_id, feed);
                }
            },
        }
    }
}

/// Run strategy as daemon
async fn run_strategy_daemon(name: &str, config_path: &PathBuf, dry_run: bool) -> Result<()> {
    // Ensure run directory exists
    let run_dir = run_dir();
    fs::create_dir_all(&run_dir)?;

    let pid_file = run_dir.join(format!("{}.pid", name));
    let log_file = log_dir().join(format!("{}.log", name));

    // Build command
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

    // Redirect output to log file
    fs::create_dir_all(log_dir())?;
    let log = fs::File::create(&log_file)?;
    let log_err = log.try_clone()?;

    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .stdin(Stdio::null());

    // Spawn daemon
    let child = cmd.spawn().context("Failed to spawn strategy process")?;

    // Write PID file
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

/// Stop a running strategy
async fn stop_strategy(name: &str, force: bool) -> Result<()> {
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
                // Remove PID file
                let _ = fs::remove_file(&pid_file);
                println!("\x1b[32m✓ Strategy '{}' stopped\x1b[0m", name);
            }
            Err(e) => {
                println!("\x1b[31m✗ Failed to stop: {}\x1b[0m", e);
                // Clean up stale PID file
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

/// Show strategy status
async fn show_status(name: Option<&str>) -> Result<()> {
    println!("\n{}", "=".repeat(60));
    println!("  STRATEGY STATUS");
    println!("{}\n", "=".repeat(60));

    let strategies = if let Some(n) = name {
        vec![n.to_string()]
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

    for strat_name in strategies {
        let status = get_strategy_status(&strat_name);
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
                    strat_name, "● running", pid_str, uptime
                );
            }
            StrategyStatus::Stopped => {
                println!(
                    "  {:<15} \x1b[90m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "○ stopped", "-", "-"
                );
            }
            StrategyStatus::Error(e) => {
                println!(
                    "  {:<15} \x1b[31m{:<12}\x1b[0m {:<10} {}",
                    strat_name, "✗ error", "-", e
                );
            }
        }
    }

    println!("\n{}", "=".repeat(60));

    Ok(())
}

/// Show strategy logs
async fn show_logs(name: &str, tail: usize, follow: bool) -> Result<()> {
    let log_file = log_dir().join(format!("{}.log", name));

    if !log_file.exists() {
        println!("\x1b[33m⚠ No log file found for '{}'\x1b[0m", name);
        println!("  Expected: {}", log_file.display());
        return Ok(());
    }

    if follow {
        // Use tail -f
        let mut child = Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg(tail.to_string())
            .arg(&log_file)
            .spawn()
            .context("Failed to run tail")?;

        child.wait()?;
    } else {
        // Just show last N lines
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

/// Reload strategy configuration
async fn reload_strategy(name: &str) -> Result<()> {
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

// === Helper Types and Functions ===

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
                    // Stale PID file: fall through to other detection paths.
                    let _ = fs::remove_file(&pid_file);
                }
                Err(_) => {
                    let _ = fs::remove_file(&pid_file);
                }
            },
            Err(e) => return StrategyStatus::Error(e.to_string()),
        }
    }

    // If the strategy is run under systemd (recommended on EC2), we won't have pidfiles.
    // Detect `ploy-strategy-<name>-dryrun.service` (and a few variants) and show it as running.
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
        // On non-Unix, just assume it's running if PID file exists
        true
    }
}

fn get_process_uptime(_pid: u32) -> Option<String> {
    // TODO: Implement actual uptime calculation
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
        // Compatibility fallback (older units may have kept underscores).
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
                // MainPID can be 0 for some unit types; treat as running anyway.
                let pid_out = Command::new("systemctl")
                    .arg("show")
                    .arg(&unit)
                    .arg("--property=MainPID")
                    .arg("--value")
                    .output()
                    .ok();

                let pid = pid_out
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .and_then(|s| s.parse::<u32>().ok())
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
        "momentum" => include_str!("../../config/strategies/momentum_default.toml"),
        "split_arb" => include_str!("../../config/strategies/split_arb_default.toml"),
        "pattern_memory" => include_str!("../../config/strategies/pattern_memory_default.toml"),
        _ => return Ok(()), // No default for unknown strategies
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, config)?;
    Ok(())
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

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(store.pool())
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

    crate::coordinator::bootstrap::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;
    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(store.pool())
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

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(store.pool())
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
// Backtest handler
// ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_backtest(
    name: &str,
    mode: StrategyBacktestMode,
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    capital: f64,
    save: bool,
    json_output: bool,
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    skip_gamma: bool,
    verify_run: Option<String>,
    diagnose_db: bool,
    database_url: Option<String>,
    lv_profile: Option<LiquidityVacuumProfile>,
    lv_price_move_threshold: Option<f64>,
    lv_volume_multiplier_threshold: Option<f64>,
    lv_order_concentration_threshold: Option<f64>,
    lv_entry_deviation_threshold: Option<f64>,
    lv_entry_zscore_threshold: Option<f64>,
    lv_take_profit_zscore_threshold: Option<f64>,
    lv_stop_loss_zscore_threshold: Option<f64>,
    lv_take_profit_ema_band_pct: Option<f64>,
    lv_stop_loss_pct: Option<f64>,
    lv_min_edge_buffer: Option<f64>,
    lv_zscore_lookback_samples: Option<usize>,
    lv_max_holding_secs: Option<u64>,
    sa_entry_after_start_max_secs: Option<u64>,
) -> Result<()> {
    use chrono::DateTime;
    use rust_decimal::prelude::*;
    use rust_decimal_macros::dec;

    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_feed::HistoricalFeed;
    use crate::strategy::backtest_recorder::{NullRecorder, PgBacktestRecorder};
    use crate::strategy::backtest_report;
    use crate::strategy::directional_backtest::{
        DirectionalBacktestConfig, DirectionalBacktestEngine,
    };
    use crate::strategy::garch_probability_backtest::{
        GarchProbabilityBacktestConfig, GarchProbabilityBacktestEngine,
    };
    use crate::strategy::liquidity_vacuum_backtest::{
        LiquidityVacuumBacktestConfig, LiquidityVacuumBacktestEngine,
    };
    use crate::strategy::momentum_backtest::{MomentumBacktestConfig, MomentumBacktestEngine};

    match name {
            "momentum"
            | "directional"
            | "prob-garch"
            | "prob_garch"
            | "liquidity-vacuum"
            | "liquidity_vacuum"
            | "staggered-arb"
            | "staggered_arb"
            | "gamma_scalping"
            | "gamma-scalping" => {}
            other => anyhow::bail!(
            "Unknown backtest strategy: '{}'. Supported: momentum, directional, prob-garch (alias: prob_garch), liquidity-vacuum (alias: liquidity_vacuum), staggered-arb (aliases: staggered_arb, gamma_scalping, gamma-scalping)",
            other
        ),
    }

    if mode == StrategyBacktestMode::Settlement {
        if name != "directional" {
            anyhow::bail!("Settlement mode is only supported for directional strategy");
        }
        if json_output {
            warn!("--json is not supported in settlement mode yet; falling back to text output");
        }
        if save {
            warn!("--save has no effect in settlement mode");
        }
        return backtest_directional_signals_pm_settlement(
            lookback_hours,
            account_id,
            agent_id,
            live_only,
            limit,
            no_refresh,
            database_url,
        )
        .await;
    }

    // Handle --verify-run: load and print an existing report
    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    if let Some(ref run_id_str) = verify_run {
        let run_id: uuid::Uuid = run_id_str.parse().context("Invalid run UUID")?;
        let store = PostgresStore::new(&db_url, 5).await?;
        let report = backtest_report::load_report(store.pool(), run_id).await?;
        if json_output {
            println!("{}", report.to_json()?);
        } else {
            println!("{}", report.print_report());
        }
        return Ok(());
    }

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();

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

    // Unified backtest feed path: database only.
    let store = PostgresStore::new(&db_url, 5).await?;
    if diagnose_db {
        print_backtest_db_diagnostics(store.pool(), &symbol_list, from_dt, to_dt).await?;
        return Ok(());
    }
    info!("Loading historical data from database");
    let mut feed =
        HistoricalFeed::from_database(store.pool(), &symbol_list, from_dt, to_dt).await?;

    let initial_capital = Decimal::from_f64(capital).unwrap_or_else(|| Decimal::new(10000, 0));

    let results = match name {
        "directional" => {
            let mut config = DirectionalBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            // Create recorder: PgBacktestRecorder if --save, else NullRecorder
            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "directional",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = DirectionalBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            // Print directional-specific summary (includes exit reasons, calibration)
            if !json_output {
                engine.print_directional_summary();
            }

            // Finalize recorder with summary metrics if saving
            if save {
                // Take the recorder back from the engine and downcast to PgBacktestRecorder
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                // Load and print report
                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "prob-garch" | "prob_garch" => {
            let mut config = GarchProbabilityBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;
            // PM BTC up/down 5m events only by default
            config.allowed_window_durations = vec![300];

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "prob_garch",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording prob_garch backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = GarchProbabilityBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "liquidity-vacuum" | "liquidity_vacuum" => {
            let mut config = LiquidityVacuumBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;

            match lv_profile.unwrap_or(LiquidityVacuumProfile::Prod) {
                LiquidityVacuumProfile::Prod => {}
                LiquidityVacuumProfile::Research => {
                    // Looser exploratory thresholds to discover candidate regimes quickly.
                    config.price_move_threshold = dec!(0.003);
                    config.volume_multiplier_threshold = dec!(1.2);
                    config.order_concentration_threshold = dec!(0.15);
                    // Deviation gate uses z-score in research mode.
                    config.entry_deviation_threshold = Decimal::ZERO;
                    config.entry_zscore_threshold = dec!(1.2);
                    config.take_profit_zscore_threshold = dec!(0.3);
                    config.stop_loss_zscore_threshold = dec!(2.5);
                    config.zscore_lookback_samples = 180;
                    config.max_holding_secs = 900;
                    config.max_spread_bps = 3000;
                }
                LiquidityVacuumProfile::ResearchV2 => {
                    // Research preset tuned for better trade quality / count balance
                    // on short-dated binary contracts.
                    config.price_move_threshold = dec!(0.0015);
                    config.volume_multiplier_threshold = dec!(0.9);
                    config.order_concentration_threshold = dec!(0.10);
                    config.entry_deviation_threshold = Decimal::ZERO;
                    config.entry_zscore_threshold = dec!(0.40);
                    config.take_profit_zscore_threshold = Decimal::ZERO;
                    config.stop_loss_zscore_threshold = Decimal::ZERO;
                    config.take_profit_ema_band_pct = dec!(0.10);
                    config.stop_loss_pct = dec!(0.35);
                    config.min_edge_buffer = dec!(0.018);
                    config.zscore_lookback_samples = 180;
                    config.max_holding_secs = 7200;
                    config.max_spread_bps = 3000;
                }
            }

            if let Some(v) = lv_price_move_threshold {
                config.price_move_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-price-move-threshold value")?;
            }
            if let Some(v) = lv_volume_multiplier_threshold {
                config.volume_multiplier_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-volume-multiplier-threshold value")?;
            }
            if let Some(v) = lv_order_concentration_threshold {
                config.order_concentration_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-order-concentration-threshold value")?;
            }
            if let Some(v) = lv_entry_deviation_threshold {
                config.entry_deviation_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-entry-deviation-threshold value")?;
            }
            if let Some(v) = lv_entry_zscore_threshold {
                config.entry_zscore_threshold =
                    Decimal::from_f64(v).context("Invalid --lv-entry-zscore-threshold value")?;
            }
            if let Some(v) = lv_take_profit_zscore_threshold {
                config.take_profit_zscore_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-take-profit-zscore-threshold value")?;
            }
            if let Some(v) = lv_stop_loss_zscore_threshold {
                config.stop_loss_zscore_threshold = Decimal::from_f64(v)
                    .context("Invalid --lv-stop-loss-zscore-threshold value")?;
            }
            if let Some(v) = lv_take_profit_ema_band_pct {
                config.take_profit_ema_band_pct =
                    Decimal::from_f64(v).context("Invalid --lv-take-profit-ema-band-pct value")?;
            }
            if let Some(v) = lv_stop_loss_pct {
                config.stop_loss_pct =
                    Decimal::from_f64(v).context("Invalid --lv-stop-loss-pct value")?;
            }
            if let Some(v) = lv_min_edge_buffer {
                config.min_edge_buffer =
                    Decimal::from_f64(v).context("Invalid --lv-min-edge-buffer value")?;
            }
            if let Some(v) = lv_zscore_lookback_samples {
                config.zscore_lookback_samples = v.max(2);
            }
            if let Some(v) = lv_max_holding_secs {
                config.max_holding_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "liquidity_vacuum",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording liquidity-vacuum backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = LiquidityVacuumBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_liquidity_vacuum_summary();
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "staggered-arb" | "staggered_arb" => {
            use crate::strategy::staggered_arb_backtest::{
                StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
            };

            let config_path = PathBuf::from("config/strategies/staggered_arb.toml");
            let config_content = fs::read_to_string(&config_path).with_context(|| {
                format!(
                    "Failed to read staggered-arb backtest config from {}",
                    config_path.display()
                )
            })?;
            let mut config = StaggeredArbBacktestConfig::from_toml_str(&config_content)?;
            config.initial_capital = initial_capital;
            if !symbol_list.is_empty() {
                config.symbols = symbol_list.clone();
            }
            if let Some(v) = sa_entry_after_start_max_secs {
                config.entry_after_start_max_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "staggered-arb",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording staggered-arb backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = StaggeredArbBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_staggered_summary();
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        "gamma_scalping" | "gamma-scalping" => {
            use crate::strategy::staggered_arb_backtest::{
                StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
            };

            let mut config = StaggeredArbBacktestConfig::with_symbols(symbol_list.clone());
            config.initial_capital = initial_capital;
            // PM 5m events only
            config.allowed_window_durations = vec![300];
            if let Some(v) = sa_entry_after_start_max_secs {
                config.entry_after_start_max_secs = v;
            }

            let mut saved_run_id: Option<uuid::Uuid> = None;
            let recorder: Box<dyn crate::strategy::backtest_recorder::BacktestRecorder> = if save {
                let config_json = serde_json::to_value(&config).unwrap_or_default();
                let pg_recorder = PgBacktestRecorder::new(
                    store.pool().clone(),
                    "gamma_scalping",
                    "replay",
                    &config_json,
                    &symbol_list,
                )
                .await?;
                saved_run_id = Some(pg_recorder.run_id());
                info!(run_id = %pg_recorder.run_id(), "Recording gamma_scalping backtest signals to DB");
                Box::new(pg_recorder)
            } else {
                Box::new(NullRecorder)
            };

            let mut engine = StaggeredArbBacktestEngine::new(config, recorder);
            let results = engine.run(&mut feed);

            if !json_output {
                engine.print_summary("Gamma Scalping (PM 5m)");
            }

            if save {
                let mut recorder = engine.take_recorder();
                if let Some(pg) = recorder.as_any_mut().downcast_mut::<PgBacktestRecorder>() {
                    pg.finalize(
                        Some(results.start_time),
                        Some(results.end_time),
                        results.total_trades as i32,
                        results.win_rate,
                        results.total_pnl,
                        results.sharpe_ratio,
                        results.max_drawdown,
                        results.profit_factor,
                    )
                    .await?;
                }

                let run_id = saved_run_id.expect("run_id should be set when --save is used");

                if !skip_gamma {
                    if let Err(e) = verify_backtest_trades_gamma(store.pool(), run_id).await {
                        warn!("Gamma verification failed: {e:#}");
                    }
                }

                let report = backtest_report::load_report(store.pool(), run_id).await?;
                if json_output {
                    println!("{}", report.to_json()?);
                } else {
                    println!("{}", report.print_report());
                }
            }

            results
        }
        _ => {
            let config =
                MomentumBacktestConfig::default_with_symbols(symbol_list.clone(), initial_capital);
            let mut engine = MomentumBacktestEngine::new(config);
            let results = engine.run(&mut feed);

            // Optionally save momentum results to DB
            if save {
                crate::strategy::momentum_backtest::save_backtest_results(
                    store.pool(),
                    &engine.config(),
                    &results,
                )
                .await?;
                info!("Backtest results saved to database");
            }
            results
        }
    };

    if json_output && !save {
        // Only print raw JSON if we didn't already print a report above
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else if !json_output && !save {
        println!("{}", results);
    }

    Ok(())
}

async fn print_backtest_db_diagnostics(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    use chrono::{DateTime, Utc};

    fn fmt_ts(ts: Option<DateTime<Utc>>) -> String {
        ts.map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    }

    async fn table_exists(pool: &sqlx::PgPool, table: &str) -> Result<bool> {
        let reg: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(format!("public.{table}"))
            .fetch_one(pool)
            .await?;
        Ok(reg.is_some())
    }

    println!("\n=== Backtest DB diagnostics ===");
    println!("symbols: {}", symbols.join(", "));
    println!("from: {}", fmt_ts(from));
    println!("to:   {}", fmt_ts(to));

    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

    // ── sync_records (best: integrated BN+PM view) ───────────
    if !table_exists(pool, "sync_records").await? {
        println!("\n[sync_records] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(timestamp),
              MAX(timestamp),
              COUNT(DISTINCT pm_market_slug)::bigint
            FROM sync_records
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR timestamp >= $2)
              AND ($3::timestamptz IS NULL OR timestamp <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, slugs)) => {
                println!("\n[sync_records]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct pm_market_slug: {slugs}");
            }
            Err(e) => {
                println!("\n[sync_records] query failed: {e}");
            }
        }
    }

    // ── binance_price_ticks (fallback spot) ──────────────────
    if !table_exists(pool, "binance_price_ticks").await? {
        println!("\n[binance_price_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT COUNT(*)::bigint, MIN(trade_time), MAX(trade_time)
            FROM binance_price_ticks
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR trade_time >= $2)
              AND ($3::timestamptz IS NULL OR trade_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts)) => {
                println!("\n[binance_price_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[binance_price_ticks] query failed: {e}");
            }
        }
    }

    // ── binance_klines (supplement spot) ─────────────────────
    if !table_exists(pool, "binance_klines").await? {
        println!("\n[binance_klines] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(open_time),
              MAX(close_time),
              COUNT(DISTINCT interval)::bigint
            FROM binance_klines
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR close_time >= $2)
              AND ($3::timestamptz IS NULL OR open_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, intervals)) => {
                println!("\n[binance_klines]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct intervals: {intervals}");
            }
            Err(e) => {
                println!("\n[binance_klines] query failed: {e}");
            }
        }
    }

    // ── clob_quote_ticks (PM quotes) ─────────────────────────
    if !table_exists(pool, "clob_quote_ticks").await? {
        println!("\n[clob_quote_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_quote_ticks
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_quote_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_quote_ticks] query failed: {e}");
            }
        }
    }

    // ── clob_orderbook_snapshots (PM depth) ──────────────────
    if !table_exists(pool, "clob_orderbook_snapshots").await? {
        println!("\n[clob_orderbook_snapshots] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_orderbook_snapshots
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_orderbook_snapshots]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_orderbook_snapshots] query failed: {e}");
            }
        }
    }

    // ── pm_market_metadata (event windows) ───────────────────
    if !table_exists(pool, "pm_market_metadata").await? {
        println!("\n[pm_market_metadata] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(*) FILTER (WHERE start_time IS NOT NULL AND end_time IS NOT NULL)::bigint,
              COUNT(*) FILTER (WHERE price_to_beat IS NOT NULL AND price_to_beat > 0)::bigint,
              MIN(start_time),
              MAX(end_time)
            FROM pm_market_metadata
            WHERE ($1::timestamptz IS NULL OR end_time >= $1)
              AND ($2::timestamptz IS NULL OR start_time <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, windows, with_s0, min_ts, max_ts)) => {
                println!("\n[pm_market_metadata]");
                println!("rows: {count}, window_rows: {windows}, with price_to_beat>0: {with_s0}");
                println!("ts_range: {} .. {}", fmt_ts(min_ts), fmt_ts(max_ts));
            }
            Err(e) => {
                println!("\n[pm_market_metadata] query failed: {e}");
            }
        }
    }

    // ── pm_token_settlements (token→slug mapping + outcomes) ─
    if !table_exists(pool, "pm_token_settlements").await? {
        println!("\n[pm_token_settlements] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(DISTINCT market_slug)::bigint,
              COUNT(*) FILTER (WHERE resolved = true)::bigint,
              MIN(resolved_at),
              MAX(resolved_at)
            FROM pm_token_settlements
            WHERE ($1::timestamptz IS NULL OR resolved_at >= $1 OR resolved_at IS NULL)
              AND ($2::timestamptz IS NULL OR resolved_at <= $2 OR resolved_at IS NULL)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, slugs, resolved, min_ts, max_ts)) => {
                println!("\n[pm_token_settlements]");
                println!("rows: {count}, distinct market_slug: {slugs}, resolved_rows: {resolved}");
                println!(
                    "resolved_at range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[pm_token_settlements] query failed: {e}");
            }
        }
    }

    // ── deribit_iv_ticks (Deribit IV baseline) ──────────────
    if !table_exists(pool, "deribit_iv_ticks").await? {
        println!("\n[deribit_iv_ticks] MISSING");
    } else {
        let mut printed = false;

        if let Ok((count, min_ts, max_ts, ccy)) =
            sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(timestamp),
                  MAX(timestamp),
                  COUNT(DISTINCT currency)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR timestamp >= $1)
                  AND ($2::timestamptz IS NULL OR timestamp <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
        {
            printed = true;
            println!("\n[deribit_iv_ticks]");
            println!(
                "rows: {count}, ts_range: {} .. {}",
                fmt_ts(min_ts),
                fmt_ts(max_ts)
            );
            println!("distinct currency: {ccy}");
        }

        if !printed {
            match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(ts),
                  MAX(ts),
                  COUNT(DISTINCT symbol)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR ts >= $1)
                  AND ($2::timestamptz IS NULL OR ts <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
            {
                Ok((count, min_ts, max_ts, symbols)) => {
                    println!("\n[deribit_iv_ticks]");
                    println!(
                        "rows: {count}, ts_range: {} .. {}",
                        fmt_ts(min_ts),
                        fmt_ts(max_ts)
                    );
                    println!("distinct symbol: {symbols}");
                }
                Err(e) => {
                    println!("\n[deribit_iv_ticks] query failed: {e}");
                }
            }
        }
    }

    println!("\nHint:");
    println!("- PM 5m backtest needs: clob_quote_ticks + pm_market_metadata (or pm_token_settlements.raw_market) + spot (sync_records or binance_price_ticks/klines).");
    println!("- Deribit IV (optional): populate deribit_iv_ticks (e.g. `ploy deribit-iv-backfill`) to enable IV-aware research/backtests.");

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Gamma verification for backtest trades
// ─────────────────────────────────────────────────────────────

/// Verify backtest trades against Polymarket official settlement via Gamma API.
///
/// 1. Map backtest trades (symbol + entry_time) → token_ids via pm_market_metadata
/// 2. Refresh unresolved tokens via Gamma API → pm_token_settlements
/// 3. Update backtest_trades with gamma_settled_price, gamma_resolved, gamma_match
async fn verify_backtest_trades_gamma(pool: &sqlx::PgPool, run_id: uuid::Uuid) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    // 1. Load trades for this run
    let trade_rows = sqlx::query(
        "SELECT id, symbol, direction, entry_time, exit_time, exit_reason, won
         FROM backtest_trades WHERE run_id = $1 ORDER BY entry_time",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .context("Failed to load backtest trades")?;

    if trade_rows.is_empty() {
        info!("No trades to verify");
        return Ok(());
    }

    // 2. Map trades to token_ids via pm_market_metadata
    //    Each trade's symbol + entry_time falls within a specific market window
    //    pm_market_metadata has: market_slug, symbol, start_time, end_time
    //    pm_token_settlements has: token_id, market_slug, outcome, settled_price
    struct TradeMapping {
        trade_id: i64,
        won: bool,
        direction: String,
        market_slug: String,
    }

    let mut mappings: Vec<TradeMapping> = Vec::new();
    let mut slugs_needed: HashSet<String> = HashSet::new();

    for row in &trade_rows {
        let trade_id: i64 = row.get("id");
        let symbol: String = row.get("symbol");
        let direction: String = row.get("direction");
        let entry_time: DateTime<Utc> = row.get("entry_time");
        let won: bool = row.get("won");

        // Find the market window that contains this trade's entry_time
        let slug_row = sqlx::query_scalar::<_, String>(
            "SELECT market_slug FROM pm_market_metadata
             WHERE symbol = $1 AND start_time <= $2 AND end_time >= $2
             LIMIT 1",
        )
        .bind(&symbol)
        .bind(entry_time)
        .fetch_optional(pool)
        .await?;

        if let Some(slug) = slug_row {
            slugs_needed.insert(slug.clone());
            mappings.push(TradeMapping {
                trade_id,
                won,
                direction,
                market_slug: slug,
            });
        }
    }

    if mappings.is_empty() {
        info!("No trades could be mapped to market slugs");
        return Ok(());
    }

    // 3. Collect token_ids for these slugs from pm_token_settlements
    let slugs_vec: Vec<String> = slugs_needed.into_iter().collect();
    let existing_settlements = sqlx::query(
        "SELECT token_id, market_slug, outcome, resolved, settled_price
         FROM pm_token_settlements WHERE market_slug = ANY($1)",
    )
    .bind(&slugs_vec)
    .fetch_all(pool)
    .await?;

    // Build slug → {outcome → (token_id, resolved, settled_price)}
    struct SettlementInfo {
        token_id: String,
        resolved: bool,
        settled_price: Option<Decimal>,
    }
    let mut slug_settlements: HashMap<String, HashMap<String, SettlementInfo>> = HashMap::new();
    for row in &existing_settlements {
        let slug: String = row.get("market_slug");
        let outcome: Option<String> = row.get("outcome");
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        let settled_price: Option<Decimal> = row.get("settled_price");
        if let Some(outcome) = outcome {
            slug_settlements.entry(slug).or_default().insert(
                outcome,
                SettlementInfo {
                    token_id,
                    resolved,
                    settled_price,
                },
            );
        }
    }

    // 4. Find unresolved token_ids that need Gamma refresh
    let mut unresolved_tokens: Vec<String> = Vec::new();
    for settlements in slug_settlements.values() {
        for info in settlements.values() {
            if !info.resolved {
                unresolved_tokens.push(info.token_id.clone());
            }
        }
    }
    // Also find slugs with NO settlement rows at all
    let mut missing_slugs: Vec<&str> = Vec::new();
    for slug in &slugs_vec {
        if !slug_settlements.contains_key(slug) {
            missing_slugs.push(slug);
        }
    }

    // For missing slugs, try to find token_ids from clob_quote_ticks or pm_market_metadata
    if !missing_slugs.is_empty() {
        // Try to get token_ids from clob_quote_ticks via market_slug join
        let extra_tokens: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT s.token_id FROM pm_token_settlements s
             WHERE s.market_slug = ANY($1) AND s.resolved = false",
        )
        .bind(
            &missing_slugs
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        unresolved_tokens.extend(extra_tokens);
    }

    unresolved_tokens.sort();
    unresolved_tokens.dedup();

    // 5. Refresh via Gamma API
    if !unresolved_tokens.is_empty() {
        const MAX_REFRESH: usize = 500;
        let to_refresh = if unresolved_tokens.len() > MAX_REFRESH {
            &unresolved_tokens[..MAX_REFRESH]
        } else {
            &unresolved_tokens
        };

        println!(
            "\n  Refreshing settlement status for {} token(s) via Gamma...",
            to_refresh.len()
        );

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "gamma fetch failed");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                if !seen_conditions.insert(cond.to_string()) {
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
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            // Store only essential fields, not the full raw_market (avoids "input too long" error)
            let raw_market = serde_json::json!({
                "id": market.id,
                "slug": market.slug,
                "closed": market.closed,
                "condition_id": market.condition_id,
            });

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                let _ = sqlx::query(
                    r#"INSERT INTO pm_token_settlements (
                        token_id, condition_id, market_id, market_slug, outcome,
                        settled_price, resolved, resolved_at, fetched_at, raw_market
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                    ON CONFLICT (token_id) DO UPDATE SET
                        settled_price = EXCLUDED.settled_price,
                        resolved = EXCLUDED.resolved,
                        resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                        fetched_at = NOW(),
                        raw_market = EXCLUDED.raw_market"#,
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
                .await;
            }
            refreshed += 1;
        }

        if refreshed > 0 {
            println!("  Refreshed {} market(s)\n", refreshed);
        }

        // Reload settlements after refresh
        let refreshed_rows = sqlx::query(
            "SELECT token_id, market_slug, outcome, resolved, settled_price
             FROM pm_token_settlements WHERE market_slug = ANY($1)",
        )
        .bind(&slugs_vec)
        .fetch_all(pool)
        .await?;

        slug_settlements.clear();
        for row in &refreshed_rows {
            let slug: String = row.get("market_slug");
            let outcome: Option<String> = row.get("outcome");
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            let settled_price: Option<Decimal> = row.get("settled_price");
            if let Some(outcome) = outcome {
                slug_settlements.entry(slug).or_default().insert(
                    outcome,
                    SettlementInfo {
                        token_id,
                        resolved,
                        settled_price,
                    },
                );
            }
        }
    }

    // 6. Update backtest_trades with gamma verification results
    let mut verified = 0usize;
    let mut matched = 0usize;
    let mut mismatched = 0usize;

    for mapping in &mappings {
        let Some(outcomes) = slug_settlements.get(&mapping.market_slug) else {
            continue;
        };

        // For directional trades: direction "UP" → check "Up" outcome, "DOWN" → check "Down"
        let outcome_key = if mapping.direction == "UP" {
            "Up"
        } else {
            "Down"
        };
        let Some(info) = outcomes.get(outcome_key) else {
            continue;
        };

        if !info.resolved {
            continue;
        }

        let Some(settled_price) = info.settled_price else {
            continue;
        };

        // gamma_match: does the tick-based outcome agree with Gamma settlement?
        // Trade "won" in tick replay ↔ settled_price >= 0.99 for the chosen direction
        let gamma_won = settled_price >= dec!(0.99);
        let gamma_match = mapping.won == gamma_won;

        sqlx::query(
            "UPDATE backtest_trades
             SET gamma_settled_price = $2, gamma_resolved = true, gamma_match = $3
             WHERE id = $1",
        )
        .bind(mapping.trade_id)
        .bind(settled_price)
        .bind(gamma_match)
        .execute(pool)
        .await?;

        verified += 1;
        if gamma_match {
            matched += 1;
        } else {
            mismatched += 1;
        }
    }

    let unverified = mappings.len() - verified;
    println!(
        "  Gamma verification: {} verified ({} matched, {} mismatched), {} unverified\n",
        verified, matched, mismatched, unverified
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backtest list handler
// ─────────────────────────────────────────────────────────────

async fn run_backtest_list(database_url: Option<String>, limit: usize) -> Result<()> {
    use crate::adapters::PostgresStore;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        Vec<String>,
        Option<i32>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT run_id, strategy, mode, symbols, total_trades, win_rate,
                total_pnl, sharpe_ratio, max_drawdown, profit_factor, created_at
         FROM backtest_runs ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        println!("No backtest runs found.");
        return Ok(());
    }

    println!(
        "\n  {:<36} {:<14} {:<10} {:<8} {:<7} {:<10} {:<7} {:<7} {}",
        "RUN_ID", "STRATEGY", "MODE", "SYMBOLS", "TRADES", "PNL", "WIN%", "SHARPE", "CREATED"
    );
    println!("  {}", "-".repeat(110));

    for (run_id, strategy, mode, symbols, trades, win_rate, pnl, sharpe, _dd, _pf, created) in &rows
    {
        let sym_str = if symbols.len() > 2 {
            format!("{}+{}", symbols[0], symbols.len() - 1)
        } else {
            symbols.join(",")
        };
        println!(
            "  {:<36} {:<14} {:<10} {:<8} {:<7} ${:<9.2} {:<6.1}% {:<7.2} {}",
            run_id,
            strategy,
            mode,
            sym_str,
            trades.unwrap_or(0),
            pnl.unwrap_or(rust_decimal::Decimal::ZERO),
            win_rate.unwrap_or(0.0) * 100.0,
            sharpe.unwrap_or(0.0),
            created.format("%Y-%m-%d %H:%M"),
        );
    }
    println!();

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backtest diff handler
// ─────────────────────────────────────────────────────────────

async fn run_backtest_diff(run1: &str, run2: &str, database_url: Option<String>) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let id1: uuid::Uuid = run1.parse().context("Invalid run1 UUID")?;
    let id2: uuid::Uuid = run2.parse().context("Invalid run2 UUID")?;

    let r1 = backtest_report::load_report(store.pool(), id1).await?;
    let r2 = backtest_report::load_report(store.pool(), id2).await?;

    let w = 64;
    let bar = "=".repeat(w);
    let thin = "-".repeat(w);

    println!("\n{}", bar);
    println!("  BACKTEST COMPARISON");
    println!("{}\n", bar);

    println!("  {:<24} {:<20} {:<20}", "METRIC", "RUN A", "RUN B");
    println!("  {}", thin);
    println!(
        "  {:<24} {:<20} {:<20}",
        "Run ID",
        &r1.run.run_id.to_string()[..8],
        &r2.run.run_id.to_string()[..8]
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Strategy", r1.run.strategy, r2.run.strategy
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Trades", r1.run.total_trades, r2.run.total_trades
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Win Rate",
        format!("{:.1}%", r1.run.win_rate * 100.0),
        format!("{:.1}%", r2.run.win_rate * 100.0)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "PnL",
        format!("${:.2}", r1.run.total_pnl),
        format!("${:.2}", r2.run.total_pnl)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Sharpe",
        format!("{:.2}", r1.run.sharpe_ratio),
        format!("{:.2}", r2.run.sharpe_ratio)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Max Drawdown",
        format!(
            "{:.2}%",
            r1.run.max_drawdown * rust_decimal_macros::dec!(100)
        ),
        format!(
            "{:.2}%",
            r2.run.max_drawdown * rust_decimal_macros::dec!(100)
        )
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Profit Factor",
        format!("{:.2}", r1.run.profit_factor),
        format!("{:.2}", r2.run.profit_factor)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Fee Drag",
        format!("{:.1}%", r1.fee_impact.fee_drag_pct),
        format!("{:.1}%", r2.fee_impact.fee_drag_pct)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Calibration Bias",
        format!("{:+.1}%", r1.calibration.overall_bias * 100.0),
        format!("{:+.1}%", r2.calibration.overall_bias * 100.0)
    );
    println!("\n{}\n", bar);

    Ok(())
}

async fn run_live_backtest_compare(
    run_id: &str,
    lookback_hours: u64,
    account_id: Option<String>,
    strategy_id: Option<String>,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use sqlx::Row;
    use std::collections::HashSet;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    crate::coordinator::bootstrap::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;

    let bt_run_id: uuid::Uuid = run_id.parse().context("Invalid run UUID")?;
    let report = backtest_report::load_report(store.pool(), bt_run_id).await?;

    let signal_types = vec![
        "live_order_submit_result".to_string(),
        "live_order_poll_update".to_string(),
        "live_order_rejected".to_string(),
        "live_order_submit_error".to_string(),
    ];

    let rows = sqlx::query(
        r#"
        SELECT
            signal_type,
            side,
            fair_value,
            market_price,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = ANY($2)
          AND ($3::text IS NULL OR account_id = $3)
          AND ($4::text IS NULL OR strategy_id = $4)
        ORDER BY recorded_at DESC
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(&signal_types)
    .bind(account_id.as_deref())
    .bind(strategy_id.as_deref())
    .fetch_all(store.pool())
    .await
    .context("Failed to query live order observations from signal_history")?;

    let mut submitted: HashSet<String> = HashSet::new();
    let mut rejected: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut touched_fill: HashSet<String> = HashSet::new();
    let mut full_fill: HashSet<String> = HashSet::new();
    let mut slippage_bps_weighted_sum = 0.0f64;
    let mut slippage_weight = 0.0f64;

    for row in rows {
        let signal_type: String = row.get("signal_type");
        let side: Option<String> = row.get("side");
        let limit_price: Option<Decimal> = row.get("fair_value");
        let fill_price: Option<Decimal> = row.get("market_price");
        let context: serde_json::Value = row.get("context");

        let order_key = context
            .get("client_order_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| {
                context
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
            });

        let Some(order_key) = order_key else { continue };
        let status = context
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let filled_qty = context
            .get("filled_qty")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match signal_type.as_str() {
            "live_order_submit_result" => {
                submitted.insert(order_key.clone());
            }
            "live_order_rejected" => {
                submitted.insert(order_key.clone());
                rejected.insert(order_key.clone());
            }
            "live_order_submit_error" => {
                submitted.insert(order_key.clone());
                failed.insert(order_key.clone());
            }
            _ => {}
        }

        if filled_qty > 0
            || status.eq_ignore_ascii_case("filled")
            || status.eq_ignore_ascii_case("partiallyfilled")
        {
            touched_fill.insert(order_key.clone());
        }
        if status.eq_ignore_ascii_case("filled") {
            full_fill.insert(order_key.clone());
        }

        if filled_qty > 0 {
            if let (Some(limit_px), Some(fill_px)) = (limit_price, fill_price) {
                if limit_px > Decimal::ZERO {
                    if let (Some(limit_f64), Some(fill_f64)) = (limit_px.to_f64(), fill_px.to_f64())
                    {
                        let side_lower = side.unwrap_or_else(|| "buy".to_string()).to_lowercase();
                        let slip_bps = if side_lower == "sell" {
                            (limit_f64 - fill_f64) / limit_f64 * 10_000.0
                        } else {
                            (fill_f64 - limit_f64) / limit_f64 * 10_000.0
                        };
                        let weight = filled_qty as f64;
                        slippage_bps_weighted_sum += slip_bps * weight;
                        slippage_weight += weight;
                    }
                }
            }
        }
    }

    let submitted_n = submitted.len();
    let rejected_n = rejected.len();
    let failed_n = failed.len();
    let touched_fill_n = touched_fill.len();
    let full_fill_n = full_fill.len();

    let live_fill_rate = if submitted_n > 0 {
        touched_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_full_fill_rate = if submitted_n > 0 {
        full_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_reject_rate = if submitted_n > 0 {
        rejected_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_failed_rate = if submitted_n > 0 {
        failed_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let avg_slippage_bps = if slippage_weight > 0.0 {
        slippage_bps_weighted_sum / slippage_weight
    } else {
        0.0
    };

    let bt_trades = report.run.total_trades.max(0) as usize;
    let live_vs_bt_trade_ratio = if bt_trades > 0 {
        touched_fill_n as f64 / bt_trades as f64
    } else {
        0.0
    };

    println!("\n{}", "=".repeat(78));
    println!("  LIVE VS BACKTEST");
    println!("{}", "=".repeat(78));
    println!(
        "  backtest_run={}  lookback_hours={}  account_id={}  strategy_id={}",
        report.run.run_id,
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        strategy_id.as_deref().unwrap_or("all")
    );
    println!();
    println!("  Backtest:");
    println!(
        "    strategy={} mode={} trades={} win_rate={:.1}% pnl=${:.2} sharpe={:.2}",
        report.run.strategy,
        report.run.mode,
        report.run.total_trades,
        report.run.win_rate * 100.0,
        report.run.total_pnl,
        report.run.sharpe_ratio
    );
    println!("  Live:");
    println!(
        "    submitted={} touched_fill={} full_fill={} rejected={} failed={}",
        submitted_n, touched_fill_n, full_fill_n, rejected_n, failed_n
    );
    println!(
        "    fill_rate={:.1}% full_fill_rate={:.1}% reject_rate={:.1}% failed_rate={:.1}% avg_slippage_bps={:.2}",
        live_fill_rate * 100.0,
        live_full_fill_rate * 100.0,
        live_reject_rate * 100.0,
        live_failed_rate * 100.0,
        avg_slippage_bps
    );
    println!(
        "  Coverage (live_filled_orders / backtest_trades): {:.2}x",
        live_vs_bt_trade_ratio
    );
    println!();

    Ok(())
}

/// Backfill Binance klines into the database for historical backtesting.
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

    crate::coordinator::bootstrap::ensure_pm_token_settlements_table(pool)
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
