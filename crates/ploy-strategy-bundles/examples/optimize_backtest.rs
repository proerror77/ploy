//! Hyperparameter optimization for PM5D strategy variants.
//!
//! Directional/reversal use TPE (Bayesian). The `three_layer` branch currently
//! uses random sampling and is labeled that way in runtime output.
//!
//! Usage (PostgreSQL):
//!   cargo run --release -p ploy-strategy-bundles --example optimize_backtest -- \
//!     --db-url postgresql://postgres:postgres@localhost:15432/ploy \
//!     --strategy-variant directional \
//!     --train-start 2026-04-01 \
//!     --train-end   2026-04-03 \
//!     --val-start   2026-04-04 \
//!     --val-end     2026-04-04 \
//!     --trials 200
//!
//!   cargo run --release -p ploy-strategy-bundles --example optimize_backtest -- \
//!     --db-url postgresql://postgres:postgres@localhost:15432/ploy \
//!     --strategy-variant reversal \
//!     --train-start 2026-04-10 \
//!     --train-end   2026-04-10 \
//!     --val-start   2026-04-11 \
//!     --val-end     2026-04-11 \
//!     --symbols BTCUSDT,DOGEUSDT \
//!     --trials 80
//!
//! Usage (Parquet, --data-dir replaces --db-url):
//!   cargo run --release -p ploy-strategy-bundles --features parquet-feed \
//!     --example optimize_backtest -- \
//!     --data-dir /data/parquet \
//!     --strategy-variant three_layer \
//!     --train-start 2026-04-01 \
//!     --train-end   2026-04-10 \
//!     --val-start   2026-04-11 \
//!     --val-end     2026-04-14 \
//!     --trials 200
//!
//! NOTE: out-of-sample validation is always run on the held-out val split.
//! When using --data-dir without explicit --val-start/--val-end, the last 40%
//! of the train range is used as validation (train covers first 60%).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_feed_loaders::{
    load_from_database_with_options, HistoricalLoadOptions as DbHistoricalLoadOptions,
};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    DirectionalStrategy, Feed, HistoricalFeed, MarketUpdate, NullRecorder, ReversalStrategy,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyLogic,
    StrategyRuntime, ThreeLayerStrategy,
};
use ploy_trading::TradeSide;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> usize {
    flag_value(args, flag)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

fn parse_u64_flag(args: &[String], flag: &str, default: u64) -> u64 {
    flag_value(args, flag)
        .and_then(|raw| parse_bytes(&raw))
        .unwrap_or(default)
}

fn parse_bytes(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let split_at = trimmed
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split_at);
    let value: f64 = number.parse().ok()?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * multiplier) as u64)
}

fn display_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else if bytes as f64 >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB)
    } else {
        format!("{bytes} B")
    }
}

fn algorithm_label(strategy_variant: &str) -> &'static str {
    match strategy_variant {
        "three_layer" => "random_sampling",
        _ => "TPE",
    }
}

#[derive(Debug, Clone)]
struct PreflightLimits {
    max_rows: usize,
    max_bytes: u64,
    max_symbols: usize,
    max_days: i64,
    allow_large_window: bool,
}

#[derive(Debug, Default)]
struct SourcePreflight {
    source: &'static str,
    rows: usize,
    bytes: u64,
}

#[derive(Debug, Default)]
struct SplitPreflight {
    label: &'static str,
    sources: Vec<SourcePreflight>,
}

impl SplitPreflight {
    fn total_rows(&self) -> usize {
        self.sources.iter().map(|source| source.rows).sum()
    }

    fn total_bytes(&self) -> u64 {
        self.sources.iter().map(|source| source.bytes).sum()
    }
}

#[derive(Debug, Default)]
struct PreflightManifest {
    splits: Vec<SplitPreflight>,
}

impl PreflightManifest {
    fn total_rows(&self) -> usize {
        self.splits.iter().map(SplitPreflight::total_rows).sum()
    }

    fn max_split_bytes(&self) -> u64 {
        self.splits
            .iter()
            .map(SplitPreflight::total_bytes)
            .max()
            .unwrap_or(0)
    }

    fn print(&self) {
        eprintln!("=== Parquet Preflight Manifest ===");
        for split in &self.splits {
            eprintln!(
                "{}: rows={} bytes={}",
                split.label,
                split.total_rows(),
                display_bytes(split.total_bytes())
            );
            for source in &split.sources {
                eprintln!(
                    "  {:<18} rows={:<12} bytes={}",
                    source.source,
                    source.rows,
                    display_bytes(source.bytes)
                );
            }
        }
        eprintln!(
            "Total estimated rows={} max_split_bytes={}",
            self.total_rows(),
            display_bytes(self.max_split_bytes())
        );
        eprintln!("Preflight preserves raw LOB and aggTrade cadence; it only counts rows.");
        eprintln!();
    }
}

fn validate_preflight(
    manifest: &PreflightManifest,
    symbols: &[String],
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
    limits: &PreflightLimits,
) -> std::result::Result<(), String> {
    if limits.allow_large_window {
        return Ok(());
    }
    let days = (to.date_naive() - from.date_naive()).num_days().abs() + 1;
    let rows = manifest.total_rows();
    let bytes = manifest.max_split_bytes();
    let mut failures = Vec::new();
    if rows > limits.max_rows {
        failures.push(format!(
            "estimated rows {rows} exceed --max-preflight-rows {}",
            limits.max_rows
        ));
    }
    if bytes > limits.max_bytes {
        failures.push(format!(
            "estimated bytes {} exceed --max-preflight-bytes {}",
            display_bytes(bytes),
            display_bytes(limits.max_bytes)
        ));
    }
    if symbols.len() > limits.max_symbols {
        failures.push(format!(
            "symbol count {} exceeds --max-symbols {}",
            symbols.len(),
            limits.max_symbols
        ));
    }
    if days > limits.max_days {
        failures.push(format!(
            "date span {days} days exceeds --max-days {}",
            limits.max_days
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}; rerun with --allow-large-window only after bounded smoke/host-health checks",
            failures.join("; ")
        ))
    }
}

fn parse_date_start(raw: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {raw}"))
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    )
}

fn parse_date_end(raw: &str) -> chrono::DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {raw}"))
            .and_hms_opt(23, 59, 59)
            .unwrap(),
    )
}

fn parse_timestamp(raw: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("Invalid timestamp: {raw}"))
        .with_timezone(&Utc)
}

#[cfg(feature = "parquet-feed")]
fn parquet_preflight_manifest(
    data_dir: &str,
    symbols: &[String],
    train_from: chrono::DateTime<Utc>,
    train_to: chrono::DateTime<Utc>,
    val_from: chrono::DateTime<Utc>,
    val_to: chrono::DateTime<Utc>,
) -> std::result::Result<PreflightManifest, Box<dyn std::error::Error>> {
    let conn = open_duckdb_for_preflight()?;
    Ok(PreflightManifest {
        splits: vec![
            split_preflight(&conn, "train", data_dir, symbols, train_from, train_to)?,
            split_preflight(&conn, "val", data_dir, symbols, val_from, val_to)?,
        ],
    })
}

#[cfg(not(feature = "parquet-feed"))]
fn parquet_preflight_manifest(
    _data_dir: &str,
    _symbols: &[String],
    _train_from: chrono::DateTime<Utc>,
    _train_to: chrono::DateTime<Utc>,
    _val_from: chrono::DateTime<Utc>,
    _val_to: chrono::DateTime<Utc>,
) -> std::result::Result<PreflightManifest, Box<dyn std::error::Error>> {
    Err("Parquet preflight requires --features parquet-feed".into())
}

#[cfg(feature = "parquet-feed")]
fn open_duckdb_for_preflight() -> std::result::Result<duckdb::Connection, Box<dyn std::error::Error>>
{
    let memory_limit =
        std::env::var("PLOY_DUCKDB_MEMORY_LIMIT").unwrap_or_else(|_| "6GB".to_string());
    let temp_dir =
        std::env::var("PLOY_DUCKDB_TEMP_DIR").unwrap_or_else(|_| "/tmp/duckdb_spill".to_string());
    std::fs::create_dir_all(&temp_dir)?;
    let conn = duckdb::Connection::open_in_memory()?;
    conn.execute_batch(&format!(
        "SET memory_limit='{}'; SET temp_directory='{}';",
        memory_limit.replace('\'', "''"),
        temp_dir.replace('\'', "''")
    ))?;
    Ok(conn)
}

#[cfg(feature = "parquet-feed")]
fn split_preflight(
    conn: &duckdb::Connection,
    label: &'static str,
    data_dir: &str,
    symbols: &[String],
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
) -> std::result::Result<SplitPreflight, Box<dyn std::error::Error>> {
    use chrono::Duration;

    const WARMUP_MINUTES: i64 = 30;
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();
    let spot_from_str = (from - Duration::minutes(WARMUP_MINUTES)).to_rfc3339();
    let sym_filter = parquet_symbol_filter_sql(symbols);

    Ok(SplitPreflight {
        label,
        sources: vec![
            SourcePreflight {
                source: "spot",
                rows: count_parquet_rows(
                    conn,
                    data_dir,
                    "binance_price_ticks",
                    "trade_time",
                    &spot_from_str,
                    &to_str,
                    &sym_filter,
                )?,
                bytes: parquet_dir_bytes(data_dir, "binance_price_ticks")?,
            },
            SourcePreflight {
                source: "agg_trade",
                rows: count_parquet_rows(
                    conn,
                    data_dir,
                    "binance_agg_trade_ticks",
                    "trade_time",
                    &from_str,
                    &to_str,
                    &sym_filter,
                )?,
                bytes: parquet_dir_bytes(data_dir, "binance_agg_trade_ticks")?,
            },
            SourcePreflight {
                source: "lob",
                rows: count_parquet_rows(
                    conn,
                    data_dir,
                    "binance_lob_ticks",
                    "event_time",
                    &from_str,
                    &to_str,
                    &sym_filter,
                )?,
                bytes: parquet_dir_bytes(data_dir, "binance_lob_ticks")?,
            },
            SourcePreflight {
                source: "pm_quote",
                rows: count_quote_rows(conn, data_dir, &from_str, &to_str)?,
                bytes: parquet_dir_bytes(data_dir, "clob_quote_ticks")?,
            },
            SourcePreflight {
                source: "pm_event",
                rows: count_event_rows(conn, data_dir, symbols, &from_str, &to_str)?,
                bytes: parquet_dir_bytes(data_dir, "pm_market_metadata")?,
            },
        ],
    })
}

#[cfg(feature = "parquet-feed")]
fn count_parquet_rows(
    conn: &duckdb::Connection,
    data_dir: &str,
    source_dir: &str,
    ts_col: &str,
    from_str: &str,
    to_str: &str,
    sym_filter: &str,
) -> std::result::Result<usize, Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/{source_dir}");
    if !std::path::Path::new(&dir).exists() {
        return Ok(0);
    }
    let glob = format!("{dir}/*.parquet");
    let sql = format!(
        "SELECT count(*)::BIGINT FROM read_parquet('{glob}') \
         WHERE {ts_col} >= TIMESTAMPTZ '{from_str}' \
           AND {ts_col} <= TIMESTAMPTZ '{to_str}' \
           {sym_filter}"
    );
    let rows: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(rows.max(0) as usize)
}

#[cfg(feature = "parquet-feed")]
fn count_quote_rows(
    conn: &duckdb::Connection,
    data_dir: &str,
    from_str: &str,
    to_str: &str,
) -> std::result::Result<usize, Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/clob_quote_ticks");
    if !std::path::Path::new(&dir).exists() {
        return Ok(0);
    }
    let glob = format!("{dir}/*.parquet");
    let sql = format!(
        "SELECT count(*)::BIGINT FROM read_parquet('{glob}') \
         WHERE received_at >= TIMESTAMPTZ '{from_str}' \
           AND received_at <= TIMESTAMPTZ '{to_str}' \
           AND source IN ('polymarket_ws', 'polymarket_ws_collector', 'ploy_runner_live') \
           AND best_bid IS NOT NULL AND best_ask IS NOT NULL \
           AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99)"
    );
    let rows: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(rows.max(0) as usize)
}

#[cfg(feature = "parquet-feed")]
fn count_event_rows(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from_str: &str,
    to_str: &str,
) -> std::result::Result<usize, Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/pm_market_metadata");
    if !std::path::Path::new(&dir).exists() {
        return Ok(0);
    }
    let glob = format!("{dir}/*.parquet");
    let sym_filter = parquet_symbol_filter_sql(symbols);
    let sql = format!(
        "SELECT count(*)::BIGINT * 2 FROM read_parquet('{glob}') \
         WHERE end_time >= TIMESTAMPTZ '{from_str}' \
           AND start_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
           AND raw_market IS NOT NULL"
    );
    let rows: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(rows.max(0) as usize)
}

#[cfg(feature = "parquet-feed")]
fn parquet_dir_bytes(
    data_dir: &str,
    source_dir: &str,
) -> std::result::Result<u64, Box<dyn std::error::Error>> {
    let dir = std::path::Path::new(data_dir).join(source_dir);
    if !dir.exists() {
        return Ok(0);
    }
    let mut bytes = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "parquet")
        {
            bytes += entry.metadata()?.len();
        }
    }
    Ok(bytes)
}

#[cfg(feature = "parquet-feed")]
fn parquet_symbol_filter_sql(symbols: &[String]) -> String {
    if symbols.is_empty() {
        return String::new();
    }
    let list = symbols
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("AND symbol IN ({list})")
}

fn canonical_strategy_variant(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "directional" | "v1" | "v2" | "v3" | "pm5d_v1" | "pm5d_v2" | "pm5d_v3" => {
            "directional".to_string()
        }
        "reversal" | "pm5d_reversal" | "pm-5m-reversal" => "reversal".to_string(),
        "three_layer" | "3layer" | "pm5d_three_layer" | "pm-5m-three-layer" => {
            "three_layer".to_string()
        }
        other => other.to_string(),
    }
}

fn build_strategy(strategy_variant: &str, config: DirectionalConfig) -> Box<dyn StrategyLogic> {
    match strategy_variant {
        "directional" => Box::new(DirectionalStrategy::new(config)),
        "reversal" => Box::new(ReversalStrategy::new(config.into())),
        "three_layer" => Box::new(ThreeLayerStrategy::new(config.into())),
        other => panic!("unsupported strategy_variant: {other}"),
    }
}

#[derive(Clone)]
enum ReplaySource {
    DbEager {
        label: String,
        updates: Arc<[MarketUpdate]>,
    },
    ParquetStream {
        label: String,
        data_dir: String,
        symbols: Vec<String>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
}

impl ReplaySource {
    fn db_eager(label: &str, updates: Vec<MarketUpdate>) -> Self {
        Self::DbEager {
            label: label.to_string(),
            updates: Arc::from(updates.into_boxed_slice()),
        }
    }

    fn parquet_stream(
        label: &str,
        data_dir: &str,
        symbols: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Self {
        Self::ParquetStream {
            label: label.to_string(),
            data_dir: data_dir.to_string(),
            symbols: symbols.to_vec(),
            from,
            to,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::DbEager { label, .. } | Self::ParquetStream { label, .. } => label,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::DbEager { .. } => "db-eager",
            Self::ParquetStream { .. } => "parquet-stream",
        }
    }

    fn update_hint(&self) -> Option<usize> {
        match self {
            Self::DbEager { updates, .. } => Some(updates.len()),
            Self::ParquetStream { .. } => None,
        }
    }

    fn validate(&self) -> std::result::Result<(), String> {
        match self {
            Self::DbEager { .. } => Ok(()),
            Self::ParquetStream { data_dir, .. } => {
                #[cfg(not(feature = "parquet-feed"))]
                {
                    let _ = data_dir;
                    Err("--data-dir requires the `parquet-feed` feature".to_string())
                }

                #[cfg(feature = "parquet-feed")]
                {
                    if std::path::Path::new(data_dir).exists() {
                        Ok(())
                    } else {
                        Err(format!("Parquet data directory does not exist: {data_dir}"))
                    }
                }
            }
        }
    }

    fn open(&self) -> std::result::Result<Box<dyn Feed>, String> {
        match self {
            Self::DbEager { updates, .. } => {
                Ok(Box::new(HistoricalFeed::shared(Arc::clone(updates))))
            }
            Self::ParquetStream {
                data_dir,
                symbols,
                from,
                to,
                ..
            } => {
                #[cfg(not(feature = "parquet-feed"))]
                {
                    let _ = (data_dir, symbols, from, to);
                    Err("--data-dir requires the `parquet-feed` feature".to_string())
                }

                #[cfg(feature = "parquet-feed")]
                {
                    use ploy_strategy_bundles::feed::parquet_stream::StreamingParquetFeed;

                    self.validate()?;
                    let mut options = ploy_strategy_bundles::feed::HistoricalLoadOptions::default();
                    // Optimizer replay is tick-preserving. Do not sample away LOB
                    // or aggTrade rows; the strategy consumes both at live cadence.
                    options.lob_sample_secs = 1;
                    Ok(Box::new(StreamingParquetFeed::new(
                        data_dir, symbols, *from, *to, &options,
                    )))
                }
            }
        }
    }
}

struct BacktestOutcome {
    net_pnl: f64,
    trade_count: usize,
    sharpe: f64,
    updates_processed: u64,
    elapsed_secs: f64,
}

fn source_hint(source: &ReplaySource) -> String {
    source
        .update_hint()
        .map(|updates| updates.to_string())
        .unwrap_or_else(|| "streaming".to_string())
}

/// Run a single backtest and return compact analyzer-style metrics.
fn run_backtest(
    strategy_variant: &str,
    config: DirectionalConfig,
    source: &ReplaySource,
    max_updates: Option<u64>,
) -> std::result::Result<BacktestOutcome, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create tokio runtime: {error}"))?;

    let strategy = build_strategy(strategy_variant, config);
    let feed = source.open()?;
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.08),
        enable_partial_fills: false,
        depth_multiple: dec!(5.0),
        min_fill_pct: dec!(0.5),
        enable_market_impact: true,
        impact_coefficient: dec!(0.1),
        default_depth_shares: 500,
    });
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates,
        skip_settlement_exits: false,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = rt.block_on(runtime.run());

    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    let cashflow = snapshot.fill_cashflow_summary();
    let net_pnl = cashflow.net_pnl().to_string().parse::<f64>().unwrap_or(0.0);
    let trade_count = result.fills_recorded as usize / 2;

    let fills = &snapshot.fills;
    let mut by_token: HashMap<&str, Vec<&ploy_trading::FillRecord>> = HashMap::new();
    for fill in fills {
        by_token
            .entry(fill.token_id.as_str())
            .or_default()
            .push(fill);
    }
    let mut per_trade_pnl = Vec::new();
    for token_fills in by_token.values() {
        let mut i = 0;
        while i + 1 < token_fills.len() {
            let entry = token_fills[i];
            let exit = token_fills[i + 1];
            if entry.side == TradeSide::Buy && exit.side == TradeSide::Sell {
                let pnl = (exit.price - entry.price) * entry.quantity - entry.fee - exit.fee;
                per_trade_pnl.push(pnl.to_string().parse::<f64>().unwrap_or(0.0));
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    let sharpe = if per_trade_pnl.len() < 5 {
        -999.0
    } else {
        let n = per_trade_pnl.len() as f64;
        let mean = per_trade_pnl.iter().sum::<f64>() / n;
        let variance = per_trade_pnl
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / n;
        let std_dev = variance.sqrt();
        if std_dev < 1e-9 {
            0.0
        } else {
            mean / std_dev * (87.0_f64 * 365.0).sqrt()
        }
    };

    Ok(BacktestOutcome {
        net_pnl,
        trade_count,
        sharpe,
        updates_processed: result.updates_processed,
        elapsed_secs: result.elapsed_secs,
    })
}

fn make_directional_config(
    symbols: &[String],
    min_probability: f64,
    min_edge: f64,
    max_entry_price: f64,
    cooldown_secs: i64,
    min_time: i64,
    max_time: i64,
) -> DirectionalConfig {
    DirectionalConfig {
        symbols: symbols.to_vec(),
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability,
        min_z_score: 0.20,
        min_entry_price: 0.10,
        max_entry_price,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        min_time_remaining_secs: min_time as u64,
        max_time_remaining_secs: max_time as u64,
        cooldown_secs: cooldown_secs as u64,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300, 900],
        three_layer_min_direction_prob: 0.52,
        three_layer_min_distance_over_sigma: 0.15,
        three_layer_min_confirmation_score: 0.03,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.015,
        three_layer_min_reward_risk: 0.9,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
    }
}

struct ReversalSearchParams {
    max_distance_pct: f64,
    max_drift_flip_age_secs: i64,
    min_post_flip_drift: f64,
    min_lob_depth_ratio: f64,
    max_ask_for_reversal: f64,
    max_pm_lag_secs: i64,
    min_edge: f64,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

fn make_reversal_config(symbols: &[String], params: &ReversalSearchParams) -> DirectionalConfig {
    DirectionalConfig {
        symbols: symbols.to_vec(),
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.20,
        min_entry_price: 0.10,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: params.min_edge,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: params.max_distance_pct,
        reversal_max_drift_flip_age_secs: params.max_drift_flip_age_secs as u64,
        reversal_min_post_flip_drift: params.min_post_flip_drift,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: params.min_lob_depth_ratio,
        reversal_max_ask_for_reversal: params.max_ask_for_reversal,
        reversal_max_pm_lag_secs: params.max_pm_lag_secs as u64,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        min_time_remaining_secs: params.min_time_remaining_secs as u64,
        max_time_remaining_secs: params.max_time_remaining_secs as u64,
        cooldown_secs: params.cooldown_secs as u64,
        stake_usd: dec!(10),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300],
        three_layer_min_direction_prob: 0.52,
        three_layer_min_distance_over_sigma: 0.15,
        three_layer_min_confirmation_score: 0.03,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.015,
        three_layer_min_reward_risk: 0.9,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
    }
}

struct ThreeLayerSearchParams {
    min_direction_prob: f64,
    min_distance_over_sigma: f64,
    min_confirmation_score: f64,
    min_drift_confirmation: f64,
    min_edge: f64,
    min_reward_risk: f64,
    take_profit_ask: f64,
    stop_distance_pct: f64,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

fn make_three_layer_config(symbols: &[String], p: &ThreeLayerSearchParams) -> DirectionalConfig {
    DirectionalConfig {
        symbols: symbols.to_vec(),
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.20,
        min_entry_price: 0.10,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: p.min_edge,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        min_time_remaining_secs: p.min_time_remaining_secs as u64,
        max_time_remaining_secs: p.max_time_remaining_secs as u64,
        cooldown_secs: p.cooldown_secs as u64,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300, 900],
        three_layer_min_direction_prob: p.min_direction_prob,
        three_layer_min_distance_over_sigma: p.min_distance_over_sigma,
        three_layer_min_confirmation_score: p.min_confirmation_score,
        three_layer_min_drift_confirmation: p.min_drift_confirmation,
        three_layer_min_edge: p.min_edge,
        three_layer_min_reward_risk: p.min_reward_risk,
        three_layer_take_profit_ask: p.take_profit_ask,
        three_layer_stop_distance_pct: p.stop_distance_pct,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let db_url = flag_value(&args, "--db-url");
    let data_dir = flag_value(&args, "--data-dir");

    if db_url.is_none() && data_dir.is_none() {
        eprintln!("ERROR: either --db-url or --data-dir is required");
        std::process::exit(1);
    }

    let strategy_variant = canonical_strategy_variant(
        &flag_value(&args, "--strategy-variant").unwrap_or_else(|| "directional".into()),
    );
    let train_start = flag_value(&args, "--train-start").unwrap_or_else(|| "2026-04-01".into());
    let train_end = flag_value(&args, "--train-end").unwrap_or_else(|| "2026-04-03".into());
    let val_start = flag_value(&args, "--val-start").unwrap_or_else(|| "2026-04-04".into());
    let val_end = flag_value(&args, "--val-end").unwrap_or_else(|| "2026-04-04".into());
    let train_start_ts = flag_value(&args, "--train-start-ts");
    let train_end_ts = flag_value(&args, "--train-end-ts");
    let val_start_ts = flag_value(&args, "--val-start-ts");
    let val_end_ts = flag_value(&args, "--val-end-ts");
    let symbols_arg = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT".into());
    let require_official_settlement = args
        .iter()
        .any(|arg| arg == "--require-official-settlement");
    let n_trials: usize = flag_value(&args, "--trials")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(200);
    let preflight_only =
        flag_present(&args, "--preflight-only") || flag_present(&args, "--preflight");
    let preflight_limits = PreflightLimits {
        max_rows: parse_usize_flag(&args, "--max-preflight-rows", 15_000_000),
        max_bytes: parse_u64_flag(&args, "--max-preflight-bytes", 80 * 1024 * 1024 * 1024),
        max_symbols: parse_usize_flag(&args, "--max-symbols", 6),
        max_days: parse_usize_flag(&args, "--max-days", 8) as i64,
        allow_large_window: flag_present(&args, "--allow-large-window"),
    };
    if let Some(memory_limit) = flag_value(&args, "--duckdb-memory-limit") {
        std::env::set_var("PLOY_DUCKDB_MEMORY_LIMIT", memory_limit);
    }
    if let Some(temp_dir) = flag_value(&args, "--duckdb-temp-dir") {
        std::env::set_var("PLOY_DUCKDB_TEMP_DIR", temp_dir);
    }
    let max_updates: Option<u64> = flag_value(&args, "--max-updates").map(|raw| {
        raw.parse()
            .unwrap_or_else(|_| panic!("Invalid --max-updates: {raw}"))
    });
    let symbols: Vec<String> = symbols_arg
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    eprintln!("=== PM5D Hyperparameter Optimization ===");
    eprintln!("Variant: {strategy_variant}");
    eprintln!(
        "Train: {} → {}",
        train_start_ts.as_deref().unwrap_or(&train_start),
        train_end_ts.as_deref().unwrap_or(&train_end)
    );
    eprintln!(
        "Val:   {} → {}",
        val_start_ts.as_deref().unwrap_or(&val_start),
        val_end_ts.as_deref().unwrap_or(&val_end)
    );
    eprintln!("Symbols: {:?}", symbols);
    eprintln!("Official-only settlement: {}", require_official_settlement);
    eprintln!(
        "Trials: {n_trials}  Algorithm: {}",
        algorithm_label(&strategy_variant)
    );
    if let Some(max_updates) = max_updates {
        eprintln!(
            "Smoke bound: max_updates={max_updates} (truncated replay; non-canonical result)"
        );
    }
    eprintln!();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let train_from = train_start_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_start(&train_start));
    let train_to = train_end_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_end(&train_end));
    let val_from = val_start_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_start(&val_start));
    let val_to = val_end_ts
        .as_deref()
        .map(parse_timestamp)
        .unwrap_or_else(|| parse_date_end(&val_end));

    let (train_source, val_source) = if let Some(ref dir) = data_dir {
        let manifest =
            parquet_preflight_manifest(dir, &symbols, train_from, train_to, val_from, val_to)
                .expect("Failed to build Parquet preflight manifest");
        manifest.print();
        if let Err(error) =
            validate_preflight(&manifest, &symbols, train_from, val_to, &preflight_limits)
        {
            eprintln!("ERROR: optimize preflight rejected this request: {error}");
            std::process::exit(2);
        }
        if preflight_only {
            eprintln!("Preflight-only mode complete; exiting before replay/optimization.");
            return;
        }

        (
            ReplaySource::parquet_stream("train", dir, &symbols, train_from, train_to),
            ReplaySource::parquet_stream("validation", dir, &symbols, val_from, val_to),
        )
    } else {
        let db_url = db_url.as_deref().unwrap();
        let pool = rt
            .block_on(PgPoolOptions::new().max_connections(3).connect(db_url))
            .expect("DB connection failed");

        eprintln!(
            "Loading training data into db-eager replay ({} → {})...",
            train_start, train_end
        );
        let train = rt
            .block_on(load_from_database_with_options(
                &pool,
                &symbols,
                train_from,
                train_to,
                &DbHistoricalLoadOptions {
                    require_official_settlement,
                    ..DbHistoricalLoadOptions::default()
                },
            ))
            .expect("Failed to load training data");
        eprintln!("  {} updates loaded", train.len());

        eprintln!(
            "Loading validation data into db-eager replay ({} → {})...",
            val_start, val_end
        );
        let val = rt
            .block_on(load_from_database_with_options(
                &pool,
                &symbols,
                val_from,
                val_to,
                &DbHistoricalLoadOptions {
                    require_official_settlement,
                    ..DbHistoricalLoadOptions::default()
                },
            ))
            .expect("Failed to load validation data");
        eprintln!("  {} updates loaded\n", val.len());
        (
            ReplaySource::db_eager("train", train),
            ReplaySource::db_eager("validation", val),
        )
    };

    eprintln!(
        "Replay source train: {} [{}] updates={}",
        train_source.label(),
        train_source.kind(),
        source_hint(&train_source)
    );
    eprintln!(
        "Replay source validation: {} [{}] updates={}",
        val_source.label(),
        val_source.kind(),
        source_hint(&val_source)
    );

    let symbols_ref = Arc::new(symbols.clone());
    let study: Study<f64> = Study::maximize(TpeSampler::new());

    if strategy_variant == "reversal" {
        let p_max_distance = FloatParam::new(0.015, 0.05).name("reversal_max_distance_pct");
        let p_max_ask = FloatParam::new(0.45, 0.85).name("reversal_max_ask_for_reversal");
        let p_pm_lag = IntParam::new(20, 120).name("reversal_max_pm_lag_secs");
        let p_min_edge = FloatParam::new(-0.05, 0.01).name("min_edge");

        let train_ref = train_source.clone();
        let symbols_ref_c = Arc::clone(&symbols_ref);

        let p_max_distance_c = p_max_distance.clone();
        let p_max_ask_c = p_max_ask.clone();
        let p_pm_lag_c = p_pm_lag.clone();
        let p_min_edge_c = p_min_edge.clone();

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let params = ReversalSearchParams {
                    max_distance_pct: p_max_distance_c.suggest(trial)?,
                    max_drift_flip_age_secs: 60,
                    min_post_flip_drift: 0.0,
                    min_lob_depth_ratio: 0.0,
                    max_ask_for_reversal: p_max_ask_c.suggest(trial)?,
                    max_pm_lag_secs: p_pm_lag_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    cooldown_secs: 0,
                    min_time_remaining_secs: 15,
                    max_time_remaining_secs: 300,
                };

                let config = make_reversal_config(symbols_ref_c.as_slice(), &params);
                let outcome = match run_backtest("reversal", config, &train_ref, max_updates) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        eprintln!(
                            "  Trial {:>3}: source={} error={error}",
                            trial.id(),
                            train_ref.kind()
                        );
                        return Ok::<f64, Error>(-1_000_000.0);
                    }
                };
                let score = if outcome.trade_count == 0 {
                    -100.0
                } else if outcome.trade_count < 5 {
                    outcome.net_pnl
                } else {
                    outcome.sharpe
                };

                eprintln!(
                    "  Trial {:>3}: source={} score={:>7.3} sharpe={:>7.3} pnl=${:>8.2} trades={:>4} updates={} elapsed={:.1}s dist={:.4} flip={} drift={:.5} lob={:.2} ask={:.3} lag={}",
                    trial.id(),
                    train_ref.kind(),
                    score,
                    outcome.sharpe,
                    outcome.net_pnl,
                    outcome.trade_count,
                    outcome.updates_processed,
                    outcome.elapsed_secs,
                    params.max_distance_pct,
                    params.max_drift_flip_age_secs,
                    params.min_post_flip_drift,
                    params.min_lob_depth_ratio,
                    params.max_ask_for_reversal,
                    params.max_pm_lag_secs,
                );

                Ok::<f64, Error>(score)
            })
            .expect("Optimization failed");

        let best = study.best_trial().expect("No completed trials");
        let best_params = ReversalSearchParams {
            max_distance_pct: best.get(&p_max_distance).unwrap_or(0.015),
            max_drift_flip_age_secs: 60,
            min_post_flip_drift: 0.0,
            min_lob_depth_ratio: 0.0,
            max_ask_for_reversal: best.get(&p_max_ask).unwrap_or(0.25),
            max_pm_lag_secs: best.get(&p_pm_lag).unwrap_or(30),
            min_edge: best.get(&p_min_edge).unwrap_or(0.02),
            cooldown_secs: 0,
            min_time_remaining_secs: 15,
            max_time_remaining_secs: 300,
        };

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Objective score:                 {:.3}", best.value);
        eprintln!(
            "reversal_max_distance_pct:     {:.4}",
            best_params.max_distance_pct
        );
        eprintln!(
            "reversal_max_drift_flip_age:   {}",
            best_params.max_drift_flip_age_secs
        );
        eprintln!(
            "reversal_min_post_flip_drift:  {:.5}",
            best_params.min_post_flip_drift
        );
        eprintln!(
            "reversal_min_lob_depth_ratio:  {:.3}",
            best_params.min_lob_depth_ratio
        );
        eprintln!(
            "reversal_max_ask_for_reversal: {:.4}",
            best_params.max_ask_for_reversal
        );
        eprintln!(
            "reversal_max_pm_lag_secs:      {}",
            best_params.max_pm_lag_secs
        );
        eprintln!("min_edge:                      {:.4}", best_params.min_edge);
        eprintln!(
            "cooldown_secs:                 {}",
            best_params.cooldown_secs
        );
        eprintln!(
            "min_time_remaining_secs:       {}",
            best_params.min_time_remaining_secs
        );
        eprintln!(
            "max_time_remaining_secs:       {}",
            best_params.max_time_remaining_secs
        );

        eprintln!("\n=== Validation (held-out) ===");
        let val_config = make_reversal_config(symbols_ref.as_slice(), &best_params);
        let val_outcome = run_backtest("reversal", val_config, &val_source, max_updates)
            .expect("Validation backtest failed");
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);

        eprintln!("\n=== Config Snippet ===");
        eprintln!(
            "reversal_max_distance_pct = {:.4}",
            best_params.max_distance_pct
        );
        eprintln!(
            "reversal_max_drift_flip_age_secs = {}",
            best_params.max_drift_flip_age_secs
        );
        eprintln!(
            "reversal_min_post_flip_drift = {:.5}",
            best_params.min_post_flip_drift
        );
        eprintln!(
            "reversal_min_lob_depth_ratio = {:.3}",
            best_params.min_lob_depth_ratio
        );
        eprintln!(
            "reversal_max_ask_for_reversal = {:.4}",
            best_params.max_ask_for_reversal
        );
        eprintln!("reversal_max_pm_lag_secs = {}", best_params.max_pm_lag_secs);
        eprintln!("min_edge = {:.4}", best_params.min_edge);
        eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
        eprintln!(
            "min_time_remaining_secs = {}",
            best_params.min_time_remaining_secs
        );
        eprintln!(
            "max_time_remaining_secs = {}",
            best_params.max_time_remaining_secs
        );

        let mut all_trials = study.trials();
        all_trials.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        eprintln!("\n=== Top 10 Trials ===");
        eprintln!(
            "{:<6} {:<8} {:<8} {:<8} {:<8} {:<8} {:<8}",
            "Trial", "Score", "dist", "flip", "lob", "ask", "edge"
        );
        for trial in all_trials.iter().take(10) {
            eprintln!(
                "{:<6} {:<8.3} {:<8.4} {:<8} {:<8.2} {:<8.3} {:<8.4}",
                trial.id,
                trial.value,
                trial.get(&p_max_distance).unwrap_or(0.0),
                60,
                0.0,
                trial.get(&p_max_ask).unwrap_or(0.0),
                trial.get(&p_min_edge).unwrap_or(0.0),
            );
        }
    } else if strategy_variant == "three_layer" {
        // ── three_layer parameter search ──────────────────────────────────────
        // Simple LCG random number generator (no external rand crate needed).
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        let mut lcg_state = seed ^ 0x9e3779b97f4a7c15;

        let mut lcg_next = move || -> f64 {
            // Xorshift64 for better quality than plain LCG
            lcg_state ^= lcg_state << 13;
            lcg_state ^= lcg_state >> 7;
            lcg_state ^= lcg_state << 17;
            (lcg_state as f64) / (u64::MAX as f64)
        };

        let sample =
            |rng: &mut dyn FnMut() -> f64, lo: f64, hi: f64| -> f64 { lo + rng() * (hi - lo) };

        let train_ref = train_source.clone();
        let symbols_ref_c = Arc::clone(&symbols_ref);
        let mut best_score = f64::NEG_INFINITY;
        let mut best_params_opt: Option<ThreeLayerSearchParams> = None;
        let mut best_pnl = 0.0f64;
        let mut best_trades = 0usize;

        for iter in 0..n_trials {
            let min_direction_prob = sample(&mut lcg_next, 0.52, 0.70);
            let min_distance_over_sigma = sample(&mut lcg_next, 0.10, 0.60);
            let min_confirmation_score = sample(&mut lcg_next, 0.05, 0.30);
            let min_drift_confirmation = sample(&mut lcg_next, 0.0001, 0.001);
            let min_edge = sample(&mut lcg_next, 0.02, 0.06);
            let min_reward_risk = sample(&mut lcg_next, 0.8, 2.0);
            let take_profit_ask = sample(&mut lcg_next, 0.60, 0.85);
            let stop_distance_pct = sample(&mut lcg_next, 0.010, 0.040);
            let cooldown_secs = sample(&mut lcg_next, 30.0, 120.0) as i64;
            let min_time_remaining_secs = sample(&mut lcg_next, 60.0, 150.0) as i64;
            let max_time_remaining_secs =
                min_time_remaining_secs + sample(&mut lcg_next, 30.0, 120.0) as i64;

            let params = ThreeLayerSearchParams {
                min_direction_prob,
                min_distance_over_sigma,
                min_confirmation_score,
                min_drift_confirmation,
                min_edge,
                min_reward_risk,
                take_profit_ask,
                stop_distance_pct,
                cooldown_secs,
                min_time_remaining_secs,
                max_time_remaining_secs,
            };

            let config = make_three_layer_config(symbols_ref_c.as_slice(), &params);
            let outcome = match run_backtest("three_layer", config, &train_ref, max_updates) {
                Ok(outcome) => outcome,
                Err(error) => {
                    eprintln!(
                        "iter {:>3}/{}: source={} error={error}",
                        iter + 1,
                        n_trials,
                        train_ref.kind()
                    );
                    continue;
                }
            };

            // Sharpe = mean(pnl_per_trade) / std(pnl_per_trade) * sqrt(trades_per_year)
            // Penalty for < 5 trades already applied inside run_backtest (-999.0)
            let score = outcome.sharpe;

            eprintln!(
                "iter {:>3}/{}: source={} sharpe={:>7.3} trades={:>4} pnl=${:>8.2} updates={} elapsed={:.1}s | params: dir_prob={:.3} dist_sigma={:.3} conf={:.3} drift={:.5} edge={:.3} rr={:.2} tp={:.3} stop={:.4} cd={}s",
                iter + 1,
                n_trials,
                train_ref.kind(),
                outcome.sharpe,
                outcome.trade_count,
                outcome.net_pnl,
                outcome.updates_processed,
                outcome.elapsed_secs,
                min_direction_prob,
                min_distance_over_sigma,
                min_confirmation_score,
                min_drift_confirmation,
                min_edge,
                min_reward_risk,
                take_profit_ask,
                stop_distance_pct,
                cooldown_secs,
            );

            if score > best_score {
                best_score = score;
                best_pnl = outcome.net_pnl;
                best_trades = outcome.trade_count;
                best_params_opt = Some(ThreeLayerSearchParams {
                    min_direction_prob,
                    min_distance_over_sigma,
                    min_confirmation_score,
                    min_drift_confirmation,
                    min_edge,
                    min_reward_risk,
                    take_profit_ask,
                    stop_distance_pct,
                    cooldown_secs,
                    min_time_remaining_secs,
                    max_time_remaining_secs,
                });
            }
        }

        let best_params = best_params_opt.expect("No completed trials");

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Sharpe:                      {best_score:.3}");
        eprintln!("PnL:                         ${best_pnl:.2}");
        eprintln!("Trades:                      {best_trades}");

        eprintln!("\n=== Validation (held-out, out-of-sample) ===");
        let val_config = make_three_layer_config(symbols_ref.as_slice(), &best_params);
        let val_outcome = run_backtest("three_layer", val_config, &val_source, max_updates)
            .expect("Validation backtest failed");
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);

        eprintln!("\n=== Best Config (TOML) ===");
        eprintln!("# Paste into [strategy] section of your config file");
        eprintln!(
            "three_layer_min_direction_prob = {:.4}",
            best_params.min_direction_prob
        );
        eprintln!(
            "three_layer_min_distance_over_sigma = {:.4}",
            best_params.min_distance_over_sigma
        );
        eprintln!(
            "three_layer_min_confirmation_score = {:.4}",
            best_params.min_confirmation_score
        );
        eprintln!(
            "three_layer_min_drift_confirmation = {:.6}",
            best_params.min_drift_confirmation
        );
        eprintln!("three_layer_min_edge = {:.4}", best_params.min_edge);
        eprintln!(
            "three_layer_min_reward_risk = {:.4}",
            best_params.min_reward_risk
        );
        eprintln!(
            "three_layer_take_profit_ask = {:.4}",
            best_params.take_profit_ask
        );
        eprintln!(
            "three_layer_stop_distance_pct = {:.4}",
            best_params.stop_distance_pct
        );
        eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
        eprintln!(
            "min_time_remaining_secs = {}",
            best_params.min_time_remaining_secs
        );
        eprintln!(
            "max_time_remaining_secs = {}",
            best_params.max_time_remaining_secs
        );
    } else {
        let p_min_prob = FloatParam::new(0.50, 0.72).name("min_probability");
        let p_min_edge = FloatParam::new(0.005, 0.06).name("min_edge");
        let p_max_entry = FloatParam::new(0.35, 0.85).name("max_entry_price");
        let p_cooldown = IntParam::new(5, 90).name("cooldown_secs");
        let p_min_time = IntParam::new(20, 120).name("min_time_remaining_secs");
        let p_max_time = IntParam::new(180, 300).name("max_time_remaining_secs");

        let train_ref = train_source.clone();
        let symbols_ref_c = Arc::clone(&symbols_ref);
        let p_min_prob_c = p_min_prob.clone();
        let p_min_edge_c = p_min_edge.clone();
        let p_max_entry_c = p_max_entry.clone();
        let p_cooldown_c = p_cooldown.clone();
        let p_min_time_c = p_min_time.clone();
        let p_max_time_c = p_max_time.clone();

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let min_prob = p_min_prob_c.suggest(trial)?;
                let min_edge = p_min_edge_c.suggest(trial)?;
                let max_entry = p_max_entry_c.suggest(trial)?;
                let cooldown = p_cooldown_c.suggest(trial)?;
                let min_time = p_min_time_c.suggest(trial)?;
                let max_time = p_max_time_c.suggest(trial)?;

                if max_time <= min_time || max_entry <= 0.45 {
                    return Ok::<f64, Error>(-10.0);
                }

                let config = make_directional_config(
                    symbols_ref_c.as_slice(),
                    min_prob,
                    min_edge,
                    max_entry,
                    cooldown,
                    min_time,
                    max_time,
                );
                let outcome = match run_backtest("directional", config, &train_ref, max_updates) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        eprintln!(
                            "  Trial {:>3}: source={} error={error}",
                            trial.id(),
                            train_ref.kind()
                        );
                        return Ok::<f64, Error>(-1_000_000.0);
                    }
                };

                eprintln!(
                    "  Trial {:>3}: source={} sharpe={:>7.3}  pnl=${:>8.2}  trades={:>4}  updates={} elapsed={:.1}s  p={:.3}  edge={:.4}  max={:.2}  cd={}s",
                    trial.id(),
                    train_ref.kind(),
                    outcome.sharpe,
                    outcome.net_pnl,
                    outcome.trade_count,
                    outcome.updates_processed,
                    outcome.elapsed_secs,
                    min_prob,
                    min_edge,
                    max_entry,
                    cooldown
                );

                Ok(outcome.sharpe)
            })
            .expect("Optimization failed");

        let best = study.best_trial().expect("No completed trials");
        let best_min_prob = best.get(&p_min_prob).unwrap_or(0.55);
        let best_min_edge = best.get(&p_min_edge).unwrap_or(0.02);
        let best_max_entry = best.get(&p_max_entry).unwrap_or(0.85);
        let best_cooldown = best.get(&p_cooldown).unwrap_or(15);
        let best_min_time = best.get(&p_min_time).unwrap_or(60);
        let best_max_time = best.get(&p_max_time).unwrap_or(300);

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Sharpe:                {:.3}", best.value);
        eprintln!("min_probability:       {best_min_prob:.4}");
        eprintln!("min_edge:              {best_min_edge:.4}");
        eprintln!("max_entry_price:       {best_max_entry:.4}");
        eprintln!("cooldown_secs:         {best_cooldown}");
        eprintln!("min_time_remaining:    {best_min_time}");
        eprintln!("max_time_remaining:    {best_max_time}");

        eprintln!("\n=== Validation (held-out) ===");
        let val_config = make_directional_config(
            symbols_ref.as_slice(),
            best_min_prob,
            best_min_edge,
            best_max_entry,
            best_cooldown,
            best_min_time,
            best_max_time,
        );
        let val_outcome = run_backtest("directional", val_config, &val_source, max_updates)
            .expect("Validation backtest failed");
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);

        eprintln!("\n=== Config Snippet ===");
        eprintln!("min_probability = {best_min_prob:.4}");
        eprintln!("min_edge = {best_min_edge:.4}");
        eprintln!("max_entry_price = {best_max_entry:.4}");
        eprintln!("cooldown_secs = {best_cooldown}");
        eprintln!("min_time_remaining_secs = {best_min_time}");
        eprintln!("max_time_remaining_secs = {best_max_time}");

        let mut all_trials = study.trials();
        all_trials.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        eprintln!("\n=== Top 10 Trials ===");
        eprintln!(
            "{:<6} {:<8} {:<8} {:<8} {:<8} {:<10} {:<10}",
            "Trial", "Sharpe", "p_min", "edge", "max_px", "cooldown", "min_time"
        );
        for trial in all_trials.iter().take(10) {
            eprintln!(
                "{:<6} {:<8.3} {:<8.3} {:<8.4} {:<8.3} {:<10} {:<10}",
                trial.id,
                trial.value,
                trial.get(&p_min_prob).unwrap_or(0.0),
                trial.get(&p_min_edge).unwrap_or(0.0),
                trial.get(&p_max_entry).unwrap_or(0.0),
                trial.get(&p_cooldown).unwrap_or(0),
                trial.get(&p_min_time).unwrap_or(0),
            );
        }
    }
}
