//! Hyperparameter optimization for PM5D strategy variants.
//!
//! Directional, reversal, and three_layer use TPE (Bayesian) sampling.
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

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_feed_loaders::{
    HistoricalLoadOptions as DbHistoricalLoadOptions, load_from_database_with_options,
};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    DirectionalStrategy, ExecutionReport, Feed, FullConfig, HistoricalFeed, MarketUpdate, Recorder,
    ReversalStrategy, RuntimeConfig, RuntimeMode, SignalRecord, SimulatedExecutor,
    SimulatedExecutorConfig, StrategyLogic, StrategyRuntime, ThreeLayerProfile, ThreeLayerStrategy,
};
use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent};
use rust_decimal_macros::dec;
use serde::Deserialize;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const DEFAULT_THREE_LAYER_CONFIG: &str = "config/strategies/02-pm5d-threelayer.unified.toml";

#[derive(Debug, Clone)]
struct SnapshotProvenance {
    hash: String,
    generated_at: DateTime<Utc>,
    optimizer_data_dir: String,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn utc_ts(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, day, 0, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    #[test]
    fn validate_preflight_rejects_empty_parquet_manifest() {
        let manifest = PreflightManifest {
            splits: vec![
                SplitPreflight {
                    label: "train",
                    ..SplitPreflight::default()
                },
                SplitPreflight {
                    label: "val",
                    ..SplitPreflight::default()
                },
            ],
        };
        let limits = PreflightLimits {
            max_rows: 15_000_000,
            max_bytes: 80 * 1024 * 1024 * 1024,
            max_symbols: 6,
            max_days: 8,
            allow_large_window: false,
        };
        let symbols = vec!["BTCUSDT".to_string()];

        let error = validate_preflight(&manifest, &symbols, utc_ts(21), utc_ts(27), &limits, true)
            .expect_err("empty parquet manifest should be rejected");

        assert!(error.contains("zero rows"));
        assert!(error.contains("train split has zero rows"));
        assert!(error.contains("val split has zero rows"));
    }

    #[test]
    fn cheap_preflight_rejects_oversized_request_before_manifest_scan() {
        let limits = PreflightLimits {
            max_rows: 15_000_000,
            max_bytes: 80 * 1024 * 1024 * 1024,
            max_symbols: 2,
            max_days: 3,
            allow_large_window: false,
        };
        let symbols = vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "SOLUSDT".to_string(),
        ];

        let error = validate_preflight_request(&symbols, utc_ts(21), utc_ts(27), &limits)
            .expect_err("oversized request should be rejected before parquet scan");

        assert!(error.contains("symbol count 3 exceeds --max-symbols 2"));
        assert!(error.contains("date span 7 days exceeds --max-days 3"));
        assert!(error.contains("before parquet preflight scan"));
    }

    fn outcome(net_pnl: f64, trade_count: usize, sharpe: f64) -> BacktestOutcome {
        BacktestOutcome {
            net_pnl,
            trade_count,
            sharpe,
            updates_processed: 0,
            elapsed_secs: 0.0,
            intents_submitted: 0,
            fills_recorded: 0,
            diagnostics: BacktestDiagnostics::default(),
        }
    }

    #[test]
    fn three_layer_objective_prefers_validation_pnl_over_train_sharpe() {
        let overfit_train = outcome(500.0, 30, 20.0);
        let weak_validation = outcome(-50.0, 20, 5.0);
        let stable_train = outcome(80.0, 20, 4.0);
        let stable_validation = outcome(60.0, 20, 3.0);

        assert!(
            three_layer_objective_score(&stable_train, &stable_validation)
                > three_layer_objective_score(&overfit_train, &weak_validation)
        );
    }

    #[test]
    fn three_layer_objective_rejects_sparse_validation() {
        let train = outcome(100.0, 20, 5.0);
        let sparse_validation = outcome(1000.0, 2, 99.0);

        assert!(three_layer_objective_score(&train, &sparse_validation) <= -999_000.0);
    }

    #[test]
    fn late_hold_ev_margin_mapping_and_toml_hint_are_explicit() {
        assert_eq!(late_hold_ev_margin_from_code(0), None);
        assert_eq!(late_hold_ev_margin_from_code(1), Some(0.0));
        assert_eq!(late_hold_ev_margin_from_code(2), Some(0.02));
        assert_eq!(late_hold_ev_margin_from_code(3), Some(0.05));
        assert_eq!(late_hold_ev_margin_from_code(4), Some(0.08));
        assert_eq!(late_hold_ev_margin_from_code(99), None);

        assert_eq!(
            late_hold_ev_margin_toml_hint(Some(0.02)),
            "three_layer_late_hold_ev_margin = 0.0200"
        );
        assert!(
            late_hold_ev_margin_toml_hint(None).contains("remove three_layer_late_hold_ev_margin")
        );
    }

    #[test]
    fn closed_trade_bucket_diagnostics_capture_directional_ev_metadata() {
        let mut diagnostics = BacktestDiagnostics::default();
        let signal = EntrySignalSnapshot {
            event_id: Some("event-1".to_string()),
            symbol: "BTCUSDT".to_string(),
            direction: "up".to_string(),
            p_hat: 0.64,
            edge: 0.08,
            entry_price: 0.47,
            entry_ts: utc_ts(21),
        };
        let entry_fill = FillRecord {
            fill_id: "entry-fill".to_string(),
            order_id: "entry-order".to_string(),
            token_id: "token-up".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            price: dec!(0.47),
            fee: dec!(0.01),
            timestamp: utc_ts(21),
        };
        let exit_fill = FillRecord {
            fill_id: "exit-fill".to_string(),
            order_id: "tl_sl_token-up_1".to_string(),
            token_id: "token-up".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            price: dec!(0.39),
            fee: dec!(0.01),
            timestamp: utc_ts(21) + chrono::Duration::seconds(75),
        };

        diagnostics.record_closed_trade_buckets(
            Some(&signal),
            &entry_fill,
            &exit_fill,
            "stop_loss",
            -3.5,
        );
        diagnostics.record_closed_trade_buckets(
            Some(&signal),
            &entry_fill,
            &exit_fill,
            "stop_loss",
            1.0,
        );

        let direction = diagnostics
            .trade_buckets
            .get("direction=up")
            .expect("direction bucket recorded");
        assert_eq!(direction.trades, 2);
        assert_eq!(direction.wins, 1);
        assert!((direction.net_pnl + 2.5).abs() < 1e-9);

        assert!(
            diagnostics
                .trade_buckets
                .contains_key("entry_price=0.35-0.50")
        );
        assert!(diagnostics.trade_buckets.contains_key("p_hat=0.60-0.70"));
        assert!(diagnostics.trade_buckets.contains_key("edge=0.05-0.10"));
        assert!(
            diagnostics
                .trade_buckets
                .contains_key("exit_reason=stop_loss")
        );
        assert!(diagnostics.trade_buckets.contains_key("hold_secs=60-120"));
        assert!(
            diagnostics
                .trade_buckets
                .contains_key("exit_price=0.35-0.50")
        );
        assert!(
            diagnostics
                .trade_buckets
                .contains_key("direction_exit=up:stop_loss")
        );
    }

    #[test]
    fn direction_filter_codes_map_to_strategy_config_values() {
        assert!(
            DirectionFilter::from_code(0)
                .allowed_directions()
                .is_empty()
        );
        assert_eq!(
            DirectionFilter::from_code(1).allowed_directions(),
            vec!["UP".to_string()]
        );
        assert_eq!(
            DirectionFilter::from_code(2).allowed_directions(),
            vec!["DOWN".to_string()]
        );
    }
}

fn load_research_snapshot_manifest(path: &str) -> ResearchSnapshotManifestProbe {
    let manifest_path = std::path::Path::new(path).join("manifest.json");
    let file = std::fs::File::open(&manifest_path).unwrap_or_else(|error| {
        eprintln!(
            "ERROR: failed to open research snapshot manifest {}: {error}",
            manifest_path.display()
        );
        std::process::exit(2);
    });
    serde_json::from_reader(file).unwrap_or_else(|error| {
        eprintln!(
            "ERROR: failed to parse research snapshot manifest {}: {error}",
            manifest_path.display()
        );
        std::process::exit(2);
    })
}

fn algorithm_label(_strategy_variant: &str) -> &'static str {
    "TPE"
}

fn load_full_config_or_exit(path: &str) -> FullConfig {
    FullConfig::from_file(path).unwrap_or_else(|error| {
        eprintln!("ERROR: failed to load config {path}: {error}");
        std::process::exit(1);
    })
}

fn legacy_executor_config(require_lob_liquidity: bool) -> SimulatedExecutorConfig {
    SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.08),
        enable_partial_fills: false,
        depth_multiple: dec!(5.0),
        min_fill_pct: dec!(0.5),
        enable_market_impact: true,
        impact_coefficient: dec!(0.1),
        default_depth_shares: 500,
        require_lob_liquidity,
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
struct QuoteLiquidityPreflight {
    quote_rows: usize,
    executable_ask_rows: usize,
    executable_bid_rows: usize,
}

#[derive(Debug, Default)]
struct SplitPreflight {
    label: &'static str,
    sources: Vec<SourcePreflight>,
    quote_liquidity: QuoteLiquidityPreflight,
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

#[derive(Debug, Deserialize)]
struct ResearchSnapshotManifestProbe {
    schema_version: String,
    snapshot_hash: Option<String>,
    generated_at: DateTime<Utc>,
    symbols: Vec<String>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    immutable_input: bool,
    optimizer_data_dir: Option<String>,
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
            eprintln!(
                "  {:<18} quotes={:<10} ask_size_rows={:<10} bid_size_rows={}",
                "pm_quote_liquidity",
                split.quote_liquidity.quote_rows,
                split.quote_liquidity.executable_ask_rows,
                split.quote_liquidity.executable_bid_rows
            );
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
    require_lob_liquidity: bool,
) -> std::result::Result<(), String> {
    let days = (to.date_naive() - from.date_naive()).num_days().abs() + 1;
    let rows = manifest.total_rows();
    let bytes = manifest.max_split_bytes();
    let mut failures = Vec::new();
    if rows == 0 {
        failures.push(
            "preflight found zero rows across train+validation; check parquet sync and date window"
                .to_string(),
        );
    }
    for split in &manifest.splits {
        if split.total_rows() == 0 {
            failures.push(format!(
                "{} split has zero rows; check parquet sync and date window",
                split.label
            ));
        }
    }
    if !limits.allow_large_window {
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
    }
    if require_lob_liquidity {
        for split in &manifest.splits {
            let event_rows = split
                .sources
                .iter()
                .find(|source| source.source == "pm_event")
                .map(|source| source.rows)
                .unwrap_or(0);
            let quote_rows = split
                .sources
                .iter()
                .find(|source| source.source == "pm_quote")
                .map(|source| source.rows)
                .unwrap_or(0);
            if event_rows > 0 && quote_rows > 0 && split.quote_liquidity.executable_ask_rows == 0 {
                failures.push(format!(
                    "{} split has {quote_rows} PM quote rows and {event_rows} PM event rows, \
                     but zero quotes with executable ask_size; --require-lob-liquidity would \
                     reject every buy as non-executable",
                    split.label
                ));
            }
        }
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

fn validate_preflight_request(
    symbols: &[String],
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
    limits: &PreflightLimits,
) -> std::result::Result<(), String> {
    if limits.allow_large_window {
        return Ok(());
    }

    let days = (to.date_naive() - from.date_naive()).num_days().abs() + 1;
    let mut failures = Vec::new();
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
            "{}; rejected before parquet preflight scan; rerun with --allow-large-window only after bounded smoke/host-health checks",
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
    let data_path = std::path::Path::new(data_dir);
    if !data_path.exists() {
        return Err(format!("Parquet data directory does not exist: {data_dir}").into());
    }
    if !data_path.is_dir() {
        return Err(format!("Parquet data path is not a directory: {data_dir}").into());
    }

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
        quote_liquidity: count_quote_liquidity(conn, data_dir, &from_str, &to_str)?,
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
fn count_quote_liquidity(
    conn: &duckdb::Connection,
    data_dir: &str,
    from_str: &str,
    to_str: &str,
) -> std::result::Result<QuoteLiquidityPreflight, Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/clob_quote_ticks");
    if !std::path::Path::new(&dir).exists() {
        return Ok(QuoteLiquidityPreflight::default());
    }
    let glob = format!("{dir}/*.parquet");
    let sql = format!(
        "SELECT \
           count(*)::BIGINT, \
           coalesce(sum(CASE WHEN best_ask IS NOT NULL AND ask_size IS NOT NULL AND ask_size > 0 THEN 1 ELSE 0 END), 0)::BIGINT, \
           coalesce(sum(CASE WHEN best_bid IS NOT NULL AND bid_size IS NOT NULL AND bid_size > 0 THEN 1 ELSE 0 END), 0)::BIGINT \
         FROM read_parquet('{glob}') \
         WHERE received_at >= TIMESTAMPTZ '{from_str}' \
           AND received_at <= TIMESTAMPTZ '{to_str}' \
           AND source IN ('polymarket_ws', 'polymarket_ws_collector', 'ploy_runner_live') \
           AND best_bid IS NOT NULL AND best_ask IS NOT NULL \
           AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99)"
    );
    let (quote_rows, executable_ask_rows, executable_bid_rows): (i64, i64, i64) =
        conn.query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(QuoteLiquidityPreflight {
        quote_rows: quote_rows.max(0) as usize,
        executable_ask_rows: executable_ask_rows.max(0) as usize,
        executable_bid_rows: executable_bid_rows.max(0) as usize,
    })
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

fn validate_snapshot_optimizer_scope(
    manifest: &ResearchSnapshotManifestProbe,
    symbols: &[String],
    train_from: DateTime<Utc>,
    train_to: DateTime<Utc>,
    val_from: DateTime<Utc>,
    val_to: DateTime<Utc>,
) -> std::result::Result<(), String> {
    let mut requested_symbols = symbols.to_vec();
    requested_symbols.sort();
    let mut snapshot_symbols = manifest.symbols.clone();
    snapshot_symbols.sort();
    if snapshot_symbols != requested_symbols {
        return Err(format!(
            "snapshot symbols {:?} do not match requested symbols {:?}",
            snapshot_symbols, requested_symbols
        ));
    }
    let requested_start = train_from.min(val_from);
    let requested_end = train_to.max(val_to);
    if manifest.start > requested_start || manifest.end < requested_end {
        return Err(format!(
            "snapshot window {} -> {} does not cover optimizer window {} -> {}",
            manifest.start, manifest.end, requested_start, requested_end
        ));
    }
    Ok(())
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
    intents_submitted: u64,
    fills_recorded: u64,
    diagnostics: BacktestDiagnostics,
}

fn three_layer_objective_score(train: &BacktestOutcome, validation: &BacktestOutcome) -> f64 {
    if validation.trade_count < 5 {
        return -1_000_000.0 + validation.net_pnl;
    }
    let train_val_gap = (train.net_pnl - validation.net_pnl).abs();
    validation.net_pnl - 0.25 * train_val_gap + 0.10 * validation.sharpe.max(0.0)
}

#[derive(Debug, Clone, Default)]
struct BacktestDiagnostics {
    signals_recorded: u64,
    orders_recorded: u64,
    orders_rejected: u64,
    fills_recorded_by_recorder: u64,
    rejection_reasons: BTreeMap<String, u64>,
    strategy: Vec<(String, u64)>,
    entry_signals_by_token: BTreeMap<String, Vec<EntrySignalSnapshot>>,
    exit_reasons_by_fill_id: BTreeMap<String, String>,
    trade_buckets: BTreeMap<String, TradeBucketStats>,
}

impl BacktestDiagnostics {
    fn summary(&self) -> String {
        let mut parts = vec![
            format!("signals={}", self.signals_recorded),
            format!("orders={}", self.orders_recorded),
            format!("order_rejects={}", self.orders_rejected),
            format!("recorder_fills={}", self.fills_recorded_by_recorder),
        ];

        let strategy = top_counts(&self.strategy, 8);
        if !strategy.is_empty() {
            parts.push(format!("strategy=[{}]", strategy));
        }

        let rejection_reasons = self
            .rejection_reasons
            .iter()
            .map(|(key, value)| (key.clone(), *value))
            .collect::<Vec<_>>();
        let rejections = top_counts(&rejection_reasons, 4);
        if !rejections.is_empty() {
            parts.push(format!("rejects=[{}]", rejections));
        }

        parts.join(" ")
    }

    fn record_closed_trade_buckets(
        &mut self,
        signal: Option<&EntrySignalSnapshot>,
        entry: &FillRecord,
        exit: &FillRecord,
        exit_reason: &str,
        pnl: f64,
    ) {
        let hold_secs = (exit.timestamp - entry.timestamp).num_seconds().max(0);
        let exit_price = decimal_to_f64(exit.price);
        let entry_price = decimal_to_f64(entry.price);

        self.record_trade_bucket(&format!("exit_reason={exit_reason}"), pnl);
        self.record_trade_bucket(&format!("hold_secs={}", hold_secs_bucket(hold_secs)), pnl);
        self.record_trade_bucket(&format!("exit_price={}", price_bucket(exit_price)), pnl);
        self.record_trade_bucket(
            &format!(
                "roundtrip_price_move={}",
                price_move_bucket(exit_price - entry_price)
            ),
            pnl,
        );

        let Some(signal) = signal else {
            self.record_trade_bucket("signal=missing", pnl);
            return;
        };

        if signal.event_id.is_none() {
            self.record_trade_bucket("event_id=missing", pnl);
        }
        let signal_to_fill_secs = (entry.timestamp - signal.entry_ts).num_seconds().max(0);
        self.record_trade_bucket(
            &format!(
                "signal_to_fill_secs={}",
                hold_secs_bucket(signal_to_fill_secs)
            ),
            pnl,
        );
        self.record_trade_bucket(&format!("symbol={}", signal.symbol), pnl);
        self.record_trade_bucket(&format!("direction={}", signal.direction), pnl);
        self.record_trade_bucket(
            &format!("direction_exit={}:{}", signal.direction, exit_reason),
            pnl,
        );
        self.record_trade_bucket(
            &format!("symbol_exit={}:{}", signal.symbol, exit_reason),
            pnl,
        );
        self.record_trade_bucket(
            &format!("entry_price={}", price_bucket(signal.entry_price)),
            pnl,
        );
        self.record_trade_bucket(&format!("p_hat={}", probability_bucket(signal.p_hat)), pnl);
        self.record_trade_bucket(&format!("edge={}", edge_bucket(signal.edge)), pnl);
    }

    fn record_trade_bucket(&mut self, key: &str, pnl: f64) {
        self.trade_buckets
            .entry(key.to_string())
            .or_default()
            .record(pnl);
    }

    fn top_trade_buckets(&self, limit: usize) -> Vec<(&String, &TradeBucketStats)> {
        let mut buckets = self
            .trade_buckets
            .iter()
            .filter(|(_, stats)| stats.trades > 0)
            .collect::<Vec<_>>();
        buckets.sort_by(|a, b| {
            a.1.net_pnl
                .partial_cmp(&b.1.net_pnl)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.trades.cmp(&a.1.trades))
                .then_with(|| a.0.cmp(b.0))
        });
        buckets.into_iter().take(limit).collect()
    }

    fn trade_buckets_json(&self, limit: usize) -> Vec<serde_json::Value> {
        self.top_trade_buckets(limit)
            .into_iter()
            .map(|(key, stats)| {
                json!({
                    "bucket": key,
                    "trades": stats.trades,
                    "net_pnl": round_secs(stats.net_pnl),
                    "avg_pnl": round_secs(stats.avg_pnl()),
                    "win_rate": round_secs(stats.win_rate()),
                })
            })
            .collect()
    }

    fn negative_trade_bucket_summary(&self, limit: usize) -> String {
        self.top_trade_buckets(limit)
            .into_iter()
            .filter(|(_, stats)| stats.net_pnl < 0.0)
            .map(|(key, stats)| {
                format!("{}:${:.2}/{}", key, round_secs(stats.net_pnl), stats.trades)
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Debug, Clone)]
struct EntrySignalSnapshot {
    event_id: Option<String>,
    symbol: String,
    direction: String,
    p_hat: f64,
    edge: f64,
    entry_price: f64,
    entry_ts: DateTime<Utc>,
}

impl EntrySignalSnapshot {
    fn from_signal(signal: &SignalRecord) -> Self {
        Self {
            event_id: signal.event_id.clone(),
            symbol: signal.symbol.clone(),
            direction: signal.direction.clone(),
            p_hat: signal.p_hat,
            edge: signal.edge,
            entry_price: decimal_to_f64(signal.entry_price),
            entry_ts: signal.ts,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TradeBucketStats {
    trades: u64,
    wins: u64,
    net_pnl: f64,
}

impl TradeBucketStats {
    fn record(&mut self, pnl: f64) {
        self.trades += 1;
        if pnl > 0.0 {
            self.wins += 1;
        }
        self.net_pnl += pnl;
    }

    fn avg_pnl(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.net_pnl / self.trades as f64
        }
    }

    fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64
        }
    }
}

fn decimal_to_f64(value: rust_decimal::Decimal) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

fn price_bucket(value: f64) -> &'static str {
    if value < 0.35 {
        "<0.35"
    } else if value < 0.50 {
        "0.35-0.50"
    } else if value < 0.65 {
        "0.50-0.65"
    } else {
        ">=0.65"
    }
}

fn probability_bucket(value: f64) -> &'static str {
    if value < 0.60 {
        "<0.60"
    } else if value < 0.70 {
        "0.60-0.70"
    } else {
        ">=0.70"
    }
}

fn edge_bucket(value: f64) -> &'static str {
    if value < 0.05 {
        "<0.05"
    } else if value < 0.10 {
        "0.05-0.10"
    } else {
        ">=0.10"
    }
}

fn hold_secs_bucket(value: i64) -> &'static str {
    if value < 30 {
        "<30"
    } else if value < 60 {
        "30-60"
    } else if value < 120 {
        "60-120"
    } else if value < 240 {
        "120-240"
    } else {
        ">=240"
    }
}

fn price_move_bucket(value: f64) -> &'static str {
    if value < -0.20 {
        "<-0.20"
    } else if value < -0.10 {
        "-0.20--0.10"
    } else if value < 0.0 {
        "-0.10-0"
    } else if value < 0.10 {
        "0-0.10"
    } else if value < 0.20 {
        "0.10-0.20"
    } else {
        ">=0.20"
    }
}

fn exit_reason_bucket(order_id: &str) -> &'static str {
    if order_id.starts_with("tl_tp_") || order_id.contains("_take_profit_") {
        "take_profit"
    } else if order_id.starts_with("tl_late_ev_") {
        "late_hold_ev"
    } else if order_id.starts_with("tl_pre_settle_") {
        "pre_settlement"
    } else if order_id.starts_with("tl_sl_") || order_id.contains("_stop_loss_") {
        "stop_loss"
    } else if order_id.starts_with("tl_settle_") || order_id.contains("_settle_") {
        "settlement"
    } else if order_id.contains("_exit_") || order_id.contains("exit") {
        "strategy_exit"
    } else {
        "unknown"
    }
}

fn top_counts(counts: &[(String, u64)], limit: usize) -> String {
    let mut ordered = counts
        .iter()
        .filter(|(_, value)| *value > 0)
        .cloned()
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ordered
        .into_iter()
        .take(limit)
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn print_backtest_diagnostics(label: &str, outcome: &BacktestOutcome) {
    let negative_buckets = outcome.diagnostics.negative_trade_bucket_summary(6);
    if outcome.trade_count > 0
        && outcome.diagnostics.orders_rejected == 0
        && negative_buckets.is_empty()
    {
        return;
    }

    eprintln!(
        "      diag {label}: runtime_intents={} runtime_fills={} {}",
        outcome.intents_submitted,
        outcome.fills_recorded,
        outcome.diagnostics.summary(),
    );
    if !negative_buckets.is_empty() {
        eprintln!("      worst_buckets {label}: {negative_buckets}");
    }
}

struct DiagnosticRecorder {
    diagnostics: Arc<Mutex<BacktestDiagnostics>>,
}

#[async_trait]
impl Recorder for DiagnosticRecorder {
    async fn record_signal(&mut self, _signal: &SignalRecord) {
        self.diagnostics.lock().unwrap().signals_recorded += 1;
    }

    async fn record_order(
        &mut self,
        _strategy: &str,
        _intent: &TradingIntent,
        _signal: Option<&SignalRecord>,
        report: &ExecutionReport,
        _order_id: &str,
    ) {
        let mut diagnostics = self.diagnostics.lock().unwrap();
        diagnostics.orders_recorded += 1;
        if report.rejected {
            diagnostics.orders_rejected += 1;
            let reason = report
                .rejection_reason
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            *diagnostics.rejection_reasons.entry(reason).or_insert(0) += 1;
        }
    }

    async fn record_fill(
        &mut self,
        _strategy: &str,
        intent: &TradingIntent,
        signal: Option<&SignalRecord>,
        fill: &FillRecord,
        _report: &ExecutionReport,
    ) {
        let mut diagnostics = self.diagnostics.lock().unwrap();
        diagnostics.fills_recorded_by_recorder += 1;
        if intent.purpose == IntentPurpose::Entry && fill.side == TradeSide::Buy {
            if let Some(signal) = signal {
                diagnostics
                    .entry_signals_by_token
                    .entry(fill.token_id.clone())
                    .or_default()
                    .push(EntrySignalSnapshot::from_signal(signal));
            }
        } else if intent.purpose == IntentPurpose::Exit && fill.side == TradeSide::Sell {
            diagnostics.exit_reasons_by_fill_id.insert(
                fill.fill_id.clone(),
                exit_reason_bucket(&intent.intent_id).to_string(),
            );
        }
    }

    async fn flush(&mut self) {}
}

fn source_hint(source: &ReplaySource) -> String {
    source
        .update_hint()
        .map(|updates| updates.to_string())
        .unwrap_or_else(|| "streaming".to_string())
}

fn round_secs(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn throughput(updates: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        0.0
    } else {
        updates as f64 / seconds
    }
}

fn outcome_timing_json(
    split: &str,
    source: &ReplaySource,
    outcome: &BacktestOutcome,
    wall_secs: f64,
    score: Option<f64>,
) -> serde_json::Value {
    json!({
        "split": split,
        "source_label": source.label(),
        "source_kind": source.kind(),
        "score": score,
        "sharpe": outcome.sharpe,
        "net_pnl": round_secs(outcome.net_pnl),
        "trade_count": outcome.trade_count,
        "updates_processed": outcome.updates_processed,
        "runtime_elapsed_secs": round_secs(outcome.elapsed_secs),
        "wall_secs": round_secs(wall_secs),
        "updates_per_sec": round_secs(throughput(outcome.updates_processed, wall_secs)),
        "intents_submitted": outcome.intents_submitted,
        "fills_recorded": outcome.fills_recorded,
        "orders_rejected": outcome.diagnostics.orders_rejected,
        "trade_buckets": outcome.diagnostics.trade_buckets_json(32),
    })
}

fn write_timing_json(path: Option<&str>, payload: serde_json::Value) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = std::path::Path::new(path).parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!(
                "warning: failed to create timing dir {}: {error}",
                parent.display()
            );
            return;
        }
    }
    match serde_json::to_string_pretty(&payload) {
        Ok(body) => {
            if let Err(error) = fs::write(path, format!("{body}\n")) {
                eprintln!("warning: failed to write timing json {path}: {error}");
            }
        }
        Err(error) => eprintln!("warning: failed to encode timing json {path}: {error}"),
    }
}

/// Run a single backtest and return compact analyzer-style metrics.
fn run_backtest(
    strategy_variant: &str,
    config: DirectionalConfig,
    source: &ReplaySource,
    executor_config: &SimulatedExecutorConfig,
    max_updates: Option<u64>,
) -> std::result::Result<BacktestOutcome, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create tokio runtime: {error}"))?;

    let strategy = build_strategy(strategy_variant, config);
    let feed = source.open()?;
    let executor = SimulatedExecutor::new(executor_config.clone());
    let diagnostics = Arc::new(Mutex::new(BacktestDiagnostics::default()));
    let recorder = Box::new(DiagnosticRecorder {
        diagnostics: Arc::clone(&diagnostics),
    });
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates,
        skip_settlement_exits: false,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = rt.block_on(runtime.run());
    let mut diagnostics = diagnostics.lock().unwrap().clone();
    diagnostics.strategy = result.strategy_diagnostics.clone();

    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    let cashflow = snapshot.fill_cashflow_summary();
    let net_pnl = cashflow.net_pnl().to_string().parse::<f64>().unwrap_or(0.0);

    let fills = &snapshot.fills;
    let mut by_token: HashMap<&str, Vec<&ploy_trading::FillRecord>> = HashMap::new();
    for fill in fills {
        by_token
            .entry(fill.token_id.as_str())
            .or_default()
            .push(fill);
    }
    let mut entry_signals_by_token = diagnostics
        .entry_signals_by_token
        .clone()
        .into_iter()
        .map(|(token, signals)| (token, VecDeque::from(signals)))
        .collect::<HashMap<_, _>>();
    let mut per_trade_pnl = Vec::new();
    for (token_id, token_fills) in by_token.iter_mut() {
        token_fills.sort_by_key(|fill| fill.timestamp);
        let mut i = 0;
        while i + 1 < token_fills.len() {
            let entry = token_fills[i];
            let exit = token_fills[i + 1];
            if entry.side == TradeSide::Buy && exit.side == TradeSide::Sell {
                let pnl = (exit.price - entry.price) * entry.quantity - entry.fee - exit.fee;
                let pnl = decimal_to_f64(pnl);
                let signal = entry_signals_by_token
                    .get_mut(*token_id)
                    .and_then(|signals| signals.pop_front());
                let exit_reason = diagnostics
                    .exit_reasons_by_fill_id
                    .get(&exit.fill_id)
                    .cloned()
                    .unwrap_or_else(|| exit_reason_bucket(&exit.order_id).to_string());
                diagnostics.record_closed_trade_buckets(
                    signal.as_ref(),
                    entry,
                    exit,
                    &exit_reason,
                    pnl,
                );
                per_trade_pnl.push(pnl);
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    let trade_count = per_trade_pnl.len();

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
        intents_submitted: result.intents_submitted,
        fills_recorded: result.fills_recorded,
        diagnostics,
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
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: min_time as u64,
        max_time_remaining_secs: max_time as u64,
        cooldown_secs: cooldown_secs as u64,
        stake_usd: dec!(25),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300, 900],
        three_layer_min_direction_prob: 0.52,
        three_layer_allowed_directions: Vec::new(),
        three_layer_min_distance_over_sigma: 0.15,
        three_layer_min_confirmation_score: 0.03,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.015,
        three_layer_min_reward_risk: 0.9,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_market_prior_weight: 0.35,
        three_layer_confirmation_logit_weight: 1.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_pre_settlement_exit_secs: 0,
        three_layer_pre_settlement_min_exit_bid: 0.01,
        three_layer_late_hold_ev_margin: None,
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
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: params.min_time_remaining_secs as u64,
        max_time_remaining_secs: params.max_time_remaining_secs as u64,
        cooldown_secs: params.cooldown_secs as u64,
        stake_usd: dec!(10),
        max_positions: 30,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![300],
        three_layer_min_direction_prob: 0.52,
        three_layer_allowed_directions: Vec::new(),
        three_layer_min_distance_over_sigma: 0.15,
        three_layer_min_confirmation_score: 0.03,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.015,
        three_layer_min_reward_risk: 0.9,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_market_prior_weight: 0.35,
        three_layer_confirmation_logit_weight: 1.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_pre_settlement_exit_secs: 0,
        three_layer_pre_settlement_min_exit_bid: 0.01,
        three_layer_late_hold_ev_margin: None,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
    }
}

struct ThreeLayerSearchParams {
    min_entry_score: f64,
    min_entry_price: f64,
    min_direction_prob: f64,
    direction_filter: DirectionFilter,
    min_distance_over_sigma: f64,
    min_confirmation_score: f64,
    min_drift_confirmation: f64,
    min_edge: f64,
    min_reward_risk: f64,
    market_prior_weight: f64,
    confirmation_logit_weight: f64,
    take_profit_ask: f64,
    stop_distance_pct: f64,
    pre_settlement_exit_secs: i64,
    pre_settlement_min_exit_bid: f64,
    late_hold_ev_margin: Option<f64>,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

#[derive(Debug, Clone, Copy)]
enum DirectionFilter {
    Both,
    UpOnly,
    DownOnly,
}

impl DirectionFilter {
    fn from_code(code: i64) -> Self {
        match code {
            1 => Self::UpOnly,
            2 => Self::DownOnly,
            _ => Self::Both,
        }
    }

    fn as_label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::UpOnly => "up_only",
            Self::DownOnly => "down_only",
        }
    }

    fn allowed_directions(self) -> Vec<String> {
        match self {
            Self::Both => Vec::new(),
            Self::UpOnly => vec!["UP".to_string()],
            Self::DownOnly => vec!["DOWN".to_string()],
        }
    }
}

fn make_three_layer_config(
    symbols: &[String],
    p: &ThreeLayerSearchParams,
    base: &DirectionalConfig,
) -> DirectionalConfig {
    let mut config = base.clone();
    config.symbols = symbols.to_vec();
    config.min_edge = p.min_edge;
    config.min_time_remaining_secs = p.min_time_remaining_secs as u64;
    config.max_time_remaining_secs = p.max_time_remaining_secs as u64;
    config.cooldown_secs = p.cooldown_secs as u64;
    config.min_entry_price = p.min_entry_price;
    config.three_layer_min_entry_score = p.min_entry_score;
    config.three_layer_min_direction_prob = p.min_direction_prob;
    config.three_layer_allowed_directions = p.direction_filter.allowed_directions();
    config.three_layer_min_distance_over_sigma = p.min_distance_over_sigma;
    config.three_layer_min_confirmation_score = p.min_confirmation_score;
    config.three_layer_min_drift_confirmation = p.min_drift_confirmation;
    config.three_layer_min_edge = p.min_edge;
    config.three_layer_min_reward_risk = p.min_reward_risk;
    config.three_layer_market_prior_weight = p.market_prior_weight;
    config.three_layer_confirmation_logit_weight = p.confirmation_logit_weight;
    config.three_layer_take_profit_ask = p.take_profit_ask;
    config.three_layer_stop_distance_pct = p.stop_distance_pct;
    config.three_layer_pre_settlement_exit_secs = p.pre_settlement_exit_secs as u64;
    config.three_layer_pre_settlement_min_exit_bid = p.pre_settlement_min_exit_bid;
    config.three_layer_late_hold_ev_margin = p.late_hold_ev_margin;
    config
}

fn late_hold_ev_margin_from_code(code: i64) -> Option<f64> {
    match code {
        1 => Some(0.0),
        2 => Some(0.02),
        3 => Some(0.05),
        4 => Some(0.08),
        _ => None,
    }
}

fn format_late_hold_ev_margin(margin: Option<f64>) -> String {
    margin
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "off".to_string())
}

fn late_hold_ev_margin_toml_hint(margin: Option<f64>) -> String {
    margin
        .map(|value| format!("three_layer_late_hold_ev_margin = {value:.4}"))
        .unwrap_or_else(|| {
            "# remove three_layer_late_hold_ev_margin to disable late-hold EV exit".to_string()
        })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let total_started = Instant::now();

    let db_url = flag_value(&args, "--db-url");
    let mut data_dir = flag_value(&args, "--data-dir");
    let snapshot_dir = flag_value(&args, "--snapshot-dir");
    let timing_json = flag_value(&args, "--timing-json");
    let mut phase_timings = Vec::new();
    let allow_live_parquet_debug = flag_present(&args, "--allow-live-parquet-debug");
    let allow_direct_db_debug = flag_present(&args, "--allow-direct-db-debug");
    let mut snapshot_manifest = None;

    if let Some(ref snapshot_dir) = snapshot_dir {
        if db_url.is_some() {
            eprintln!("ERROR: --snapshot-dir cannot be combined with --db-url");
            std::process::exit(2);
        }
        let manifest = load_research_snapshot_manifest(snapshot_dir);
        if !manifest.immutable_input {
            eprintln!("ERROR: research snapshot manifest is not marked immutable_input=true");
            std::process::exit(2);
        }
        if manifest.schema_version != "research_snapshot_v1" {
            eprintln!(
                "ERROR: unsupported research snapshot schema {}; expected research_snapshot_v1",
                manifest.schema_version
            );
            std::process::exit(2);
        }
        if manifest
            .snapshot_hash
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            eprintln!("ERROR: research snapshot manifest is missing snapshot_hash");
            std::process::exit(2);
        }
        match (&data_dir, &manifest.optimizer_data_dir) {
            (None, Some(snapshot_data_dir)) => {
                data_dir = Some(snapshot_data_dir.clone());
            }
            (Some(actual), Some(expected)) if actual != expected => {
                eprintln!(
                    "ERROR: --data-dir {actual} does not match snapshot optimizer_data_dir {expected}"
                );
                std::process::exit(2);
            }
            (Some(_), None) => {
                eprintln!(
                    "ERROR: research snapshot manifest does not declare optimizer_data_dir; \
                     refusing to optimize against an unpinned parquet source"
                );
                std::process::exit(2);
            }
            (None, None) => {}
            _ => {}
        }
        if data_dir.is_none() {
            eprintln!(
                "ERROR: research snapshot manifest must declare optimizer_data_dir for canonical optimization"
            );
            std::process::exit(2);
        }
        snapshot_manifest = Some(manifest);
    } else {
        if db_url.is_some() && !allow_direct_db_debug {
            eprintln!(
                "ERROR: direct DB optimizer replay is non-canonical; pass --snapshot-dir or explicit --allow-direct-db-debug"
            );
            std::process::exit(2);
        }
        if data_dir.is_some() && !allow_live_parquet_debug {
            eprintln!(
                "ERROR: live Parquet optimizer replay is non-canonical; pass --snapshot-dir or explicit --allow-live-parquet-debug"
            );
            std::process::exit(2);
        }
        if data_dir.is_none() && db_url.is_none() {
            eprintln!(
                "ERROR: optimizer canonical mode requires --snapshot-dir; debug modes require --data-dir/--allow-live-parquet-debug or --db-url/--allow-direct-db-debug"
            );
            std::process::exit(2);
        }
    }

    if db_url.is_none() && data_dir.is_none() {
        eprintln!("ERROR: either --db-url or --data-dir is required");
        std::process::exit(1);
    }

    let strategy_variant = canonical_strategy_variant(
        &flag_value(&args, "--strategy-variant").unwrap_or_else(|| "directional".into()),
    );
    let three_layer_full_config = if strategy_variant == "three_layer" {
        let config_path =
            flag_value(&args, "--config").unwrap_or_else(|| DEFAULT_THREE_LAYER_CONFIG.into());
        Some((config_path.clone(), load_full_config_or_exit(&config_path)))
    } else {
        None
    };
    let train_start = flag_value(&args, "--train-start").unwrap_or_else(|| "2026-04-01".into());
    let train_end = flag_value(&args, "--train-end").unwrap_or_else(|| "2026-04-03".into());
    let val_start = flag_value(&args, "--val-start").unwrap_or_else(|| "2026-04-04".into());
    let val_end = flag_value(&args, "--val-end").unwrap_or_else(|| "2026-04-04".into());
    let train_start_ts = flag_value(&args, "--train-start-ts");
    let train_end_ts = flag_value(&args, "--train-end-ts");
    let val_start_ts = flag_value(&args, "--val-start-ts");
    let val_end_ts = flag_value(&args, "--val-end-ts");
    let symbols_arg = flag_value(&args, "--symbols")
        .or_else(|| {
            three_layer_full_config
                .as_ref()
                .map(|(_, config)| config.strategy.symbols.join(","))
        })
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT".into());
    let require_official_settlement = flag_present(&args, "--require-official-settlement")
        || three_layer_full_config
            .as_ref()
            .map(|(_, config)| config.backtest_data.require_official_settlement)
            .unwrap_or(false);
    let require_lob_liquidity = if flag_present(&args, "--allow-synthetic-liquidity") {
        false
    } else {
        flag_present(&args, "--require-lob-liquidity")
            || three_layer_full_config
                .as_ref()
                .map(|(_, config)| config.execution.require_lob_liquidity)
                .unwrap_or(false)
    };
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
    let three_layer_base_config = three_layer_full_config.as_ref().map(|(_, config)| {
        let mut strategy = config.strategy.clone();
        strategy.symbols = symbols.clone();
        strategy
    });
    let mut executor_config = three_layer_full_config
        .as_ref()
        .map(|(_, config)| config.sim_executor_config())
        .unwrap_or_else(|| legacy_executor_config(require_lob_liquidity));
    executor_config.require_lob_liquidity = require_lob_liquidity;

    eprintln!("=== PM5D Hyperparameter Optimization ===");
    eprintln!("Variant: {strategy_variant}");
    if let Some((path, config)) = &three_layer_full_config {
        eprintln!("Config baseline: {path}");
        eprintln!("Baseline stake_usd: {}", config.strategy.stake_usd);
        eprintln!(
            "Baseline allowed_window_secs: {:?}",
            config.strategy.allowed_window_secs
        );
    }
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
        "Executable LOB liquidity required: {}",
        executor_config.require_lob_liquidity
    );
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

    let _snapshot_provenance = snapshot_manifest.as_ref().map(|manifest| {
        if let Err(error) = validate_snapshot_optimizer_scope(
            manifest, &symbols, train_from, train_to, val_from, val_to,
        ) {
            eprintln!("ERROR: research snapshot does not match optimizer request: {error}");
            std::process::exit(2);
        }
        let provenance = SnapshotProvenance {
            hash: manifest.snapshot_hash.clone().unwrap_or_default(),
            generated_at: manifest.generated_at,
            optimizer_data_dir: manifest.optimizer_data_dir.clone().unwrap_or_default(),
        };
        let line = format!(
            "Research snapshot: schema={} hash={} generated_at={} optimizer_data_dir={}",
            manifest.schema_version,
            provenance.hash,
            provenance.generated_at,
            provenance.optimizer_data_dir
        );
        eprintln!("{line}");
        println!("{line}");
        provenance
    });

    let (train_source, val_source) = if let Some(ref dir) = data_dir {
        let request_guardrail_started = Instant::now();
        if let Err(error) =
            validate_preflight_request(&symbols, train_from, val_to, &preflight_limits)
        {
            eprintln!("ERROR: optimize request rejected: {error}");
            phase_timings.push(json!({
                "phase": "request_guardrail",
                "source": "parquet-stream",
                "wall_secs": round_secs(request_guardrail_started.elapsed().as_secs_f64()),
                "result": "rejected",
                "error": &error,
            }));
            write_timing_json(
                timing_json.as_deref(),
                json!({
                    "command": "optimize_backtest",
                    "mode": if preflight_only { "preflight-only" } else { "optimize" },
                    "strategy_variant": &strategy_variant,
                    "symbols": &symbols,
                    "train_start": train_start_ts.as_deref().unwrap_or(&train_start),
                    "train_end": train_end_ts.as_deref().unwrap_or(&train_end),
                    "val_start": val_start_ts.as_deref().unwrap_or(&val_start),
                    "val_end": val_end_ts.as_deref().unwrap_or(&val_end),
                    "trials_requested": n_trials,
                    "source": "parquet-stream",
                    "phase_timings": phase_timings,
                    "error": error,
                    "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
                }),
            );
            std::process::exit(2);
        }
        phase_timings.push(json!({
            "phase": "request_guardrail",
            "source": "parquet-stream",
            "wall_secs": round_secs(request_guardrail_started.elapsed().as_secs_f64()),
            "result": "passed",
        }));
        let preflight_started = Instant::now();
        let manifest =
            parquet_preflight_manifest(dir, &symbols, train_from, train_to, val_from, val_to)
                .expect("Failed to build Parquet preflight manifest");
        phase_timings.push(json!({
            "phase": "parquet_preflight",
            "source": "parquet-stream",
            "wall_secs": round_secs(preflight_started.elapsed().as_secs_f64()),
            "data_dir": dir,
        }));
        manifest.print();
        if let Err(error) = validate_preflight(
            &manifest,
            &symbols,
            train_from,
            val_to,
            &preflight_limits,
            executor_config.require_lob_liquidity,
        ) {
            eprintln!("ERROR: optimize preflight rejected this request: {error}");
            std::process::exit(2);
        }
        if preflight_only {
            eprintln!("Preflight-only mode complete; exiting before replay/optimization.");
            write_timing_json(
                timing_json.as_deref(),
                json!({
                    "command": "optimize_backtest",
                    "mode": "preflight-only",
                    "strategy_variant": &strategy_variant,
                    "symbols": &symbols,
                    "train_start": train_start_ts.as_deref().unwrap_or(&train_start),
                    "train_end": train_end_ts.as_deref().unwrap_or(&train_end),
                    "val_start": val_start_ts.as_deref().unwrap_or(&val_start),
                    "val_end": val_end_ts.as_deref().unwrap_or(&val_end),
                    "trials_requested": n_trials,
                    "source": "parquet-stream",
                    "phase_timings": phase_timings,
                    "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
                }),
            );
            return;
        }

        (
            ReplaySource::parquet_stream("train", dir, &symbols, train_from, train_to),
            ReplaySource::parquet_stream("validation", dir, &symbols, val_from, val_to),
        )
    } else {
        let db_url = db_url.as_deref().unwrap();
        let db_connect_started = Instant::now();
        let pool = rt
            .block_on(PgPoolOptions::new().max_connections(3).connect(db_url))
            .expect("DB connection failed");
        phase_timings.push(json!({
            "phase": "db_connect",
            "source": "db-eager",
            "wall_secs": round_secs(db_connect_started.elapsed().as_secs_f64()),
        }));

        eprintln!(
            "Loading training data into db-eager replay ({} → {})...",
            train_start, train_end
        );
        let train_load_started = Instant::now();
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
        phase_timings.push(json!({
            "phase": "db_load_train",
            "source": "db-eager",
            "wall_secs": round_secs(train_load_started.elapsed().as_secs_f64()),
            "updates_loaded": train.len(),
        }));

        eprintln!(
            "Loading validation data into db-eager replay ({} → {})...",
            val_start, val_end
        );
        let val_load_started = Instant::now();
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
        phase_timings.push(json!({
            "phase": "db_load_validation",
            "source": "db-eager",
            "wall_secs": round_secs(val_load_started.elapsed().as_secs_f64()),
            "updates_loaded": val.len(),
        }));
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

    let trial_timings: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let mut validation_timings = Vec::new();
    let symbols_ref = Arc::new(symbols.clone());
    let executor_config = Arc::new(executor_config);
    let three_layer_base_config = three_layer_base_config.map(Arc::new);
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
        let executor_config_c = Arc::clone(&executor_config);
        let trial_timings_c = Arc::clone(&trial_timings);

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let trial_id = trial.id();
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
                let trial_started = Instant::now();
                let outcome = match run_backtest(
                    "reversal",
                    config,
                    &train_ref,
                    executor_config_c.as_ref(),
                    max_updates,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        trial_timings_c.lock().unwrap().push(json!({
                            "trial_id": trial_id,
                            "strategy_variant": "reversal",
                            "split": "train",
                            "source_label": train_ref.label(),
                            "source_kind": train_ref.kind(),
                            "wall_secs": round_secs(trial_started.elapsed().as_secs_f64()),
                            "error": &error,
                        }));
                        eprintln!(
                            "  Trial {:>3}: source={} error={error}",
                            trial_id,
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
                let trial_wall_secs = trial_started.elapsed().as_secs_f64();
                let mut record =
                    outcome_timing_json("train", &train_ref, &outcome, trial_wall_secs, Some(score));
                record["trial_id"] = json!(trial_id);
                record["strategy_variant"] = json!("reversal");
                trial_timings_c.lock().unwrap().push(record);

                eprintln!(
                    "  Trial {:>3}: source={} score={:>7.3} sharpe={:>7.3} pnl=${:>8.2} trades={:>4} updates={} elapsed={:.1}s dist={:.4} flip={} drift={:.5} lob={:.2} ask={:.3} lag={}",
                    trial_id,
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
                print_backtest_diagnostics(&format!("trial {trial_id}"), &outcome);

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
        let validation_started = Instant::now();
        let val_outcome = run_backtest(
            "reversal",
            val_config,
            &val_source,
            executor_config.as_ref(),
            max_updates,
        )
        .expect("Validation backtest failed");
        validation_timings.push(outcome_timing_json(
            "validation",
            &val_source,
            &val_outcome,
            validation_started.elapsed().as_secs_f64(),
            Some(val_outcome.sharpe),
        ));
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);
        print_backtest_diagnostics("validation", &val_outcome);

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
        let trial_timing_snapshot = trial_timings.lock().unwrap().clone();
        write_timing_json(
            timing_json.as_deref(),
            json!({
                "command": "optimize_backtest",
                "strategy_variant": "reversal",
                "algorithm": algorithm_label("reversal"),
                "trials_requested": n_trials,
                "trials_recorded": trial_timing_snapshot.len(),
                "train_source": {"label": train_source.label(), "kind": train_source.kind(), "updates": source_hint(&train_source)},
                "validation_source": {"label": val_source.label(), "kind": val_source.kind(), "updates": source_hint(&val_source)},
                "symbols": &symbols,
                "phase_timings": phase_timings,
                "trial_timings": trial_timing_snapshot,
                "validation_timings": validation_timings,
                "best_training_score": best.value,
                "validation_sharpe": val_outcome.sharpe,
                "validation_net_pnl": round_secs(val_outcome.net_pnl),
                "validation_trades": val_outcome.trade_count,
                "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
            }),
        );
    } else if strategy_variant == "three_layer" {
        let base_config = three_layer_base_config
            .as_ref()
            .expect("three_layer optimizer requires a config baseline");
        // ── three_layer parameter search ──────────────────────────────────────
        let train_ref = train_source.clone();
        let val_ref = val_source.clone();
        let symbols_ref_c = Arc::clone(&symbols_ref);
        let executor_config_c = Arc::clone(&executor_config);
        let base_config_c = Arc::clone(base_config);
        let trial_timings_c = Arc::clone(&trial_timings);

        let p_min_entry_score = FloatParam::new(0.30, 0.85).name("three_layer_min_entry_score");
        let p_min_entry_price = FloatParam::new(0.25, 0.65).name("three_layer_min_entry_price");
        let p_min_direction_prob =
            FloatParam::new(0.56, 0.85).name("three_layer_min_direction_prob");
        let p_direction_filter = IntParam::new(0, 2).name("three_layer_direction_filter");
        let p_min_distance_over_sigma =
            FloatParam::new(0.20, 0.90).name("three_layer_min_distance_over_sigma");
        let p_min_confirmation_score =
            FloatParam::new(0.10, 0.50).name("three_layer_min_confirmation_score");
        let p_min_drift_confirmation =
            FloatParam::new(0.0001, 0.001).name("three_layer_min_drift_confirmation");
        let p_min_edge = FloatParam::new(0.03, 0.18).name("three_layer_min_edge");
        let p_min_reward_risk = FloatParam::new(1.0, 3.0).name("three_layer_min_reward_risk");
        let p_market_prior_weight =
            FloatParam::new(0.10, 0.75).name("three_layer_market_prior_weight");
        let p_confirmation_logit_weight =
            FloatParam::new(0.0, 2.5).name("three_layer_confirmation_logit_weight");
        let p_take_profit_ask = FloatParam::new(0.65, 0.95).name("three_layer_take_profit_ask");
        let p_stop_distance_pct =
            FloatParam::new(0.006, 0.030).name("three_layer_stop_distance_pct");
        let p_pre_settlement_exit_secs =
            IntParam::new(0, 180).name("three_layer_pre_settlement_exit_secs");
        let p_pre_settlement_min_exit_bid =
            FloatParam::new(0.01, 0.35).name("three_layer_pre_settlement_min_exit_bid");
        let p_late_hold_ev_margin_code =
            IntParam::new(0, 4).name("three_layer_late_hold_ev_margin_code");
        let p_cooldown_secs = IntParam::new(60, 300).name("cooldown_secs");
        let p_min_time_remaining_secs = IntParam::new(60, 150).name("min_time_remaining_secs");
        let p_max_time_span_secs = IntParam::new(30, 120).name("three_layer_time_span_secs");

        let p_min_entry_score_c = p_min_entry_score.clone();
        let p_min_entry_price_c = p_min_entry_price.clone();
        let p_min_direction_prob_c = p_min_direction_prob.clone();
        let p_direction_filter_c = p_direction_filter.clone();
        let p_min_distance_over_sigma_c = p_min_distance_over_sigma.clone();
        let p_min_confirmation_score_c = p_min_confirmation_score.clone();
        let p_min_drift_confirmation_c = p_min_drift_confirmation.clone();
        let p_min_edge_c = p_min_edge.clone();
        let p_min_reward_risk_c = p_min_reward_risk.clone();
        let p_market_prior_weight_c = p_market_prior_weight.clone();
        let p_confirmation_logit_weight_c = p_confirmation_logit_weight.clone();
        let p_take_profit_ask_c = p_take_profit_ask.clone();
        let p_stop_distance_pct_c = p_stop_distance_pct.clone();
        let p_pre_settlement_exit_secs_c = p_pre_settlement_exit_secs.clone();
        let p_pre_settlement_min_exit_bid_c = p_pre_settlement_min_exit_bid.clone();
        let p_late_hold_ev_margin_code_c = p_late_hold_ev_margin_code.clone();
        let p_cooldown_secs_c = p_cooldown_secs.clone();
        let p_min_time_remaining_secs_c = p_min_time_remaining_secs.clone();
        let p_max_time_span_secs_c = p_max_time_span_secs.clone();

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let trial_id = trial.id();
                let min_time_remaining_secs = p_min_time_remaining_secs_c.suggest(trial)?;
                let params = ThreeLayerSearchParams {
                    min_entry_score: p_min_entry_score_c.suggest(trial)?,
                    min_entry_price: p_min_entry_price_c.suggest(trial)?,
                    min_direction_prob: p_min_direction_prob_c.suggest(trial)?,
                    direction_filter: DirectionFilter::from_code(p_direction_filter_c.suggest(trial)?),
                    min_distance_over_sigma: p_min_distance_over_sigma_c.suggest(trial)?,
                    min_confirmation_score: p_min_confirmation_score_c.suggest(trial)?,
                    min_drift_confirmation: p_min_drift_confirmation_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    min_reward_risk: p_min_reward_risk_c.suggest(trial)?,
                    market_prior_weight: p_market_prior_weight_c.suggest(trial)?,
                    confirmation_logit_weight: p_confirmation_logit_weight_c.suggest(trial)?,
                    take_profit_ask: p_take_profit_ask_c.suggest(trial)?,
                    stop_distance_pct: p_stop_distance_pct_c.suggest(trial)?,
                    pre_settlement_exit_secs: p_pre_settlement_exit_secs_c.suggest(trial)?,
                    pre_settlement_min_exit_bid: p_pre_settlement_min_exit_bid_c.suggest(trial)?,
                    late_hold_ev_margin: late_hold_ev_margin_from_code(
                        p_late_hold_ev_margin_code_c.suggest(trial)?,
                    ),
                    cooldown_secs: p_cooldown_secs_c.suggest(trial)?,
                    min_time_remaining_secs,
                    max_time_remaining_secs: min_time_remaining_secs
                        + p_max_time_span_secs_c.suggest(trial)?,
                };

                let config = make_three_layer_config(
                    symbols_ref_c.as_slice(),
                    &params,
                    base_config_c.as_ref(),
                );
                let trial_started = Instant::now();
                let outcome = match run_backtest(
                    "three_layer",
                    config.clone(),
                    &train_ref,
                    executor_config_c.as_ref(),
                    max_updates,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        trial_timings_c.lock().unwrap().push(json!({
                            "trial_id": trial_id,
                            "strategy_variant": "three_layer",
                            "split": "train",
                            "source_label": train_ref.label(),
                            "source_kind": train_ref.kind(),
                            "wall_secs": round_secs(trial_started.elapsed().as_secs_f64()),
                            "error": &error,
                        }));
                        eprintln!(
                            "  Trial {:>3}: source={} error={error}",
                            trial_id,
                            train_ref.kind()
                        );
                        return Ok::<f64, Error>(-1_000_000.0);
                    }
                };

                let validation_started = Instant::now();
                let validation = match run_backtest(
                    "three_layer",
                    config,
                    &val_ref,
                    executor_config_c.as_ref(),
                    max_updates,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        trial_timings_c.lock().unwrap().push(json!({
                            "trial_id": trial_id,
                            "strategy_variant": "three_layer",
                            "split": "validation",
                            "source_label": val_ref.label(),
                            "source_kind": val_ref.kind(),
                            "wall_secs": round_secs(validation_started.elapsed().as_secs_f64()),
                            "error": &error,
                        }));
                        eprintln!(
                            "  Trial {:>3}: validation source={} error={error}",
                            trial_id,
                            val_ref.kind()
                        );
                        return Ok::<f64, Error>(-1_000_000.0);
                    }
                };

                let score = three_layer_objective_score(&outcome, &validation);
                let trial_wall_secs = trial_started.elapsed().as_secs_f64();
                let mut record = outcome_timing_json(
                    "train",
                    &train_ref,
                    &outcome,
                    trial_wall_secs,
                    Some(score),
                );
                record["trial_id"] = json!(trial_id);
                record["strategy_variant"] = json!("three_layer");
                record["direction_filter"] = json!(params.direction_filter.as_label());
                record["min_entry_price"] = json!(params.min_entry_price);
                let mut validation_record = outcome_timing_json(
                    "validation",
                    &val_ref,
                    &validation,
                    validation_started.elapsed().as_secs_f64(),
                    Some(score),
                );
                validation_record["trial_id"] = json!(trial_id);
                validation_record["strategy_variant"] = json!("three_layer");
                validation_record["direction_filter"] = json!(params.direction_filter.as_label());
                validation_record["min_entry_price"] = json!(params.min_entry_price);
                record["late_hold_ev_margin"] = json!(params.late_hold_ev_margin);
                validation_record["late_hold_ev_margin"] = json!(params.late_hold_ev_margin);
                {
                    let mut timings = trial_timings_c.lock().unwrap();
                    timings.push(record);
                    timings.push(validation_record);
                }

                eprintln!(
                    "  Trial {:>3}: source={} score={:>8.2} train_pnl=${:>8.2} val_pnl=${:>8.2} val_sharpe={:>7.3} val_trades={:>4} updates={} elapsed={:.1}s | params: entry={:.3} min_px={:.3} dir_prob={:.3} side={} dist_sigma={:.3} conf={:.3} drift={:.5} edge={:.3} rr={:.2} prior_w={:.2} conf_w={:.2} tp={:.3} stop={:.4} pre_settle={}s min_exit_bid={:.3} late_ev={} cd={}s",
                    trial_id,
                    train_ref.kind(),
                    score,
                    outcome.net_pnl,
                    validation.net_pnl,
                    validation.sharpe,
                    validation.trade_count,
                    outcome.updates_processed,
                    outcome.elapsed_secs,
                    params.min_entry_score,
                    params.min_entry_price,
                    params.min_direction_prob,
                    params.direction_filter.as_label(),
                    params.min_distance_over_sigma,
                    params.min_confirmation_score,
                    params.min_drift_confirmation,
                    params.min_edge,
                    params.min_reward_risk,
                    params.market_prior_weight,
                    params.confirmation_logit_weight,
                    params.take_profit_ask,
                    params.stop_distance_pct,
                    params.pre_settlement_exit_secs,
                    params.pre_settlement_min_exit_bid,
                    format_late_hold_ev_margin(params.late_hold_ev_margin),
                    params.cooldown_secs,
                );
                print_backtest_diagnostics(&format!("trial {trial_id}"), &outcome);

                Ok::<f64, Error>(score)
            })
            .expect("Optimization failed");

        let best = study.best_trial().expect("No completed trials");
        let best_min_time_remaining_secs = best.get(&p_min_time_remaining_secs).unwrap_or(60);
        let best_params = ThreeLayerSearchParams {
            min_entry_score: best.get(&p_min_entry_score).unwrap_or(0.30),
            min_entry_price: best.get(&p_min_entry_price).unwrap_or(0.25),
            min_direction_prob: best.get(&p_min_direction_prob).unwrap_or(0.52),
            direction_filter: DirectionFilter::from_code(
                best.get(&p_direction_filter).unwrap_or(0),
            ),
            min_distance_over_sigma: best.get(&p_min_distance_over_sigma).unwrap_or(0.15),
            min_confirmation_score: best.get(&p_min_confirmation_score).unwrap_or(0.03),
            min_drift_confirmation: best.get(&p_min_drift_confirmation).unwrap_or(0.0002),
            min_edge: best.get(&p_min_edge).unwrap_or(0.02),
            min_reward_risk: best.get(&p_min_reward_risk).unwrap_or(0.9),
            market_prior_weight: best.get(&p_market_prior_weight).unwrap_or(0.35),
            confirmation_logit_weight: best.get(&p_confirmation_logit_weight).unwrap_or(1.0),
            take_profit_ask: best.get(&p_take_profit_ask).unwrap_or(0.70),
            stop_distance_pct: best.get(&p_stop_distance_pct).unwrap_or(0.020),
            pre_settlement_exit_secs: best.get(&p_pre_settlement_exit_secs).unwrap_or(0),
            pre_settlement_min_exit_bid: best.get(&p_pre_settlement_min_exit_bid).unwrap_or(0.01),
            late_hold_ev_margin: late_hold_ev_margin_from_code(
                best.get(&p_late_hold_ev_margin_code).unwrap_or(0),
            ),
            cooldown_secs: best.get(&p_cooldown_secs).unwrap_or(30),
            min_time_remaining_secs: best_min_time_remaining_secs,
            max_time_remaining_secs: best_min_time_remaining_secs
                + best.get(&p_max_time_span_secs).unwrap_or(60),
        };

        eprintln!("\n=== Best Parameters (Training) ===");
        eprintln!("Objective:                   {:.3}", best.value);

        eprintln!("\n=== Validation (held-out, out-of-sample) ===");
        let val_config =
            make_three_layer_config(symbols_ref.as_slice(), &best_params, base_config.as_ref());
        let validation_started = Instant::now();
        let val_outcome = run_backtest(
            "three_layer",
            val_config,
            &val_source,
            executor_config.as_ref(),
            max_updates,
        )
        .expect("Validation backtest failed");
        let mut validation_record = outcome_timing_json(
            "validation",
            &val_source,
            &val_outcome,
            validation_started.elapsed().as_secs_f64(),
            Some(val_outcome.sharpe),
        );
        validation_record["direction_filter"] = json!(best_params.direction_filter.as_label());
        validation_record["min_entry_price"] = json!(best_params.min_entry_price);
        validation_record["late_hold_ev_margin"] = json!(best_params.late_hold_ev_margin);
        validation_timings.push(validation_record);
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);
        print_backtest_diagnostics("validation", &val_outcome);

        eprintln!("\n=== Best Config (TOML) ===");
        eprintln!("# Paste into [strategy] section of your config file");
        eprintln!(
            "three_layer_min_entry_score = {:.4}",
            best_params.min_entry_score
        );
        eprintln!("min_entry_price = {:.4}", best_params.min_entry_price);
        eprintln!(
            "three_layer_min_direction_prob = {:.4}",
            best_params.min_direction_prob
        );
        let allowed_directions = best_params.direction_filter.allowed_directions();
        if !allowed_directions.is_empty() {
            eprintln!("three_layer_allowed_directions = {:?}", allowed_directions);
        }
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
            "three_layer_market_prior_weight = {:.4}",
            best_params.market_prior_weight
        );
        eprintln!(
            "three_layer_confirmation_logit_weight = {:.4}",
            best_params.confirmation_logit_weight
        );
        eprintln!(
            "three_layer_take_profit_ask = {:.4}",
            best_params.take_profit_ask
        );
        eprintln!(
            "three_layer_stop_distance_pct = {:.4}",
            best_params.stop_distance_pct
        );
        eprintln!(
            "three_layer_pre_settlement_exit_secs = {}",
            best_params.pre_settlement_exit_secs
        );
        eprintln!(
            "three_layer_pre_settlement_min_exit_bid = {:.4}",
            best_params.pre_settlement_min_exit_bid
        );
        eprintln!(
            "{}",
            late_hold_ev_margin_toml_hint(best_params.late_hold_ev_margin)
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
        let trial_timing_snapshot = trial_timings.lock().unwrap().clone();
        write_timing_json(
            timing_json.as_deref(),
            json!({
                "command": "optimize_backtest",
                "strategy_variant": "three_layer",
                "algorithm": algorithm_label("three_layer"),
                "trials_requested": n_trials,
                "trials_recorded": trial_timing_snapshot.len(),
                "train_source": {"label": train_source.label(), "kind": train_source.kind(), "updates": source_hint(&train_source)},
                "validation_source": {"label": val_source.label(), "kind": val_source.kind(), "updates": source_hint(&val_source)},
                "symbols": &symbols,
                "phase_timings": phase_timings,
                "trial_timings": trial_timing_snapshot,
                "validation_timings": validation_timings,
                "best_training_score": best.value,
                "validation_sharpe": val_outcome.sharpe,
                "validation_net_pnl": round_secs(val_outcome.net_pnl),
                "validation_trades": val_outcome.trade_count,
                "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
            }),
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
        let executor_config_c = Arc::clone(&executor_config);
        let trial_timings_c = Arc::clone(&trial_timings);

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let trial_id = trial.id();
                let min_prob = p_min_prob_c.suggest(trial)?;
                let min_edge = p_min_edge_c.suggest(trial)?;
                let max_entry = p_max_entry_c.suggest(trial)?;
                let cooldown = p_cooldown_c.suggest(trial)?;
                let min_time = p_min_time_c.suggest(trial)?;
                let max_time = p_max_time_c.suggest(trial)?;

                if max_time <= min_time || max_entry <= 0.45 {
                    trial_timings_c.lock().unwrap().push(json!({
                        "trial_id": trial_id,
                        "strategy_variant": "directional",
                        "split": "train",
                        "source_label": train_ref.label(),
                        "source_kind": train_ref.kind(),
                        "score": -10.0,
                        "skipped": "invalid_parameter_combo",
                    }));
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
                let trial_started = Instant::now();
                let outcome = match run_backtest(
                    "directional",
                    config,
                    &train_ref,
                    executor_config_c.as_ref(),
                    max_updates,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        trial_timings_c.lock().unwrap().push(json!({
                            "trial_id": trial_id,
                            "strategy_variant": "directional",
                            "split": "train",
                            "source_label": train_ref.label(),
                            "source_kind": train_ref.kind(),
                            "wall_secs": round_secs(trial_started.elapsed().as_secs_f64()),
                            "error": &error,
                        }));
                        eprintln!(
                            "  Trial {:>3}: source={} error={error}",
                            trial_id,
                            train_ref.kind()
                        );
                        return Ok::<f64, Error>(-1_000_000.0);
                    }
                };
                let score = outcome.sharpe;
                let trial_wall_secs = trial_started.elapsed().as_secs_f64();
                let mut record =
                    outcome_timing_json("train", &train_ref, &outcome, trial_wall_secs, Some(score));
                record["trial_id"] = json!(trial_id);
                record["strategy_variant"] = json!("directional");
                trial_timings_c.lock().unwrap().push(record);

                eprintln!(
                    "  Trial {:>3}: source={} sharpe={:>7.3}  pnl=${:>8.2}  trades={:>4}  updates={} elapsed={:.1}s  p={:.3}  edge={:.4}  max={:.2}  cd={}s",
                    trial_id,
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
                print_backtest_diagnostics(&format!("trial {trial_id}"), &outcome);

                Ok(score)
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
        let validation_started = Instant::now();
        let val_outcome = run_backtest(
            "directional",
            val_config,
            &val_source,
            executor_config.as_ref(),
            max_updates,
        )
        .expect("Validation backtest failed");
        validation_timings.push(outcome_timing_json(
            "validation",
            &val_source,
            &val_outcome,
            validation_started.elapsed().as_secs_f64(),
            Some(val_outcome.sharpe),
        ));
        eprintln!("Val Source:  {}", val_source.kind());
        eprintln!("Val Sharpe:  {:.3}", val_outcome.sharpe);
        eprintln!("Val PnL:     ${:.2}", val_outcome.net_pnl);
        eprintln!("Val Trades:  {}", val_outcome.trade_count);
        eprintln!("Val Updates: {}", val_outcome.updates_processed);
        eprintln!("Val Elapsed: {:.1}s", val_outcome.elapsed_secs);
        print_backtest_diagnostics("validation", &val_outcome);

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
        let trial_timing_snapshot = trial_timings.lock().unwrap().clone();
        write_timing_json(
            timing_json.as_deref(),
            json!({
                "command": "optimize_backtest",
                "strategy_variant": "directional",
                "algorithm": algorithm_label("directional"),
                "trials_requested": n_trials,
                "trials_recorded": trial_timing_snapshot.len(),
                "train_source": {"label": train_source.label(), "kind": train_source.kind(), "updates": source_hint(&train_source)},
                "validation_source": {"label": val_source.label(), "kind": val_source.kind(), "updates": source_hint(&val_source)},
                "symbols": &symbols,
                "phase_timings": phase_timings,
                "trial_timings": trial_timing_snapshot,
                "validation_timings": validation_timings,
                "best_training_score": best.value,
                "validation_sharpe": val_outcome.sharpe,
                "validation_net_pnl": round_secs(val_outcome.net_pnl),
                "validation_trades": val_outcome.trade_count,
                "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
            }),
        );
    }
}
