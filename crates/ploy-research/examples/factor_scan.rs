//! factor_scan — regime-aware factor IC scanner
//!
//! Loads historical data from Postgres, builds FactorObservation rows,
//! runs scan_into_registry across all regime × label combinations, and
//! prints the top factors per regime.
//!
//! Usage:
//!   cargo run -p ploy-research --example factor_scan -- \
//!     --db-url postgres://... \
//!     --symbols BTCUSDT,ETHUSDT \
//!     --start-date 2026-04-01 \
//!     --end-date 2026-04-15 \
//!     [--lob-sample-secs 5] \
//!     [--max-quote-age-secs 30] \
//!     [--top-n 5]

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::{
    build_factor_observations_with_lob, load_research_lob_snapshots_sampled,
    scan_into_registry, FactorObservation, FactorRegistry, Regime,
};
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::traits::MarketUpdate;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

fn market_update_ts(u: &MarketUpdate) -> DateTime<Utc> {
    match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::AggTrade { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::L2Depth { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered { end_time, window_secs, .. } => {
            *end_time
                - chrono::Duration::seconds(*window_secs as i64)
                - chrono::Duration::hours(1)
        }
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) <= end);
    &items[lo..hi]
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let db_url = flag_value(&args, "--db-url").expect("--db-url required");

    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });

    let symbols: Vec<String> = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);

    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);

    let top_n: usize = flag_value(&args, "--top-n")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);

    eprintln!("factor_scan: {} -> {} for {:?}", start, end, symbols);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await
        .expect("database connection failed");

    // Bulk load all market updates and LOB snapshots for the full range.
    let t0 = std::time::Instant::now();
    let all_updates = load_from_database_with_options(
        &pool,
        &symbols,
        start,
        end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            ..Default::default()
        },
    )
    .await
    .expect("bulk historical load failed");
    eprintln!("load_from_database_with_options: {:?} ({} rows)", t0.elapsed(), all_updates.len());

    let t1 = std::time::Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        &pool,
        &symbols,
        start,
        end,
        lob_sample_secs,
    )
    .await
    .expect("bulk lob snapshot load failed");
    eprintln!(
        "load_research_lob_snapshots_sampled (sample_secs={}): {:?} ({} rows)",
        lob_sample_secs,
        t1.elapsed(),
        all_lob_snapshots.len()
    );

    // Slice to the requested window and build FactorObservation rows.
    let updates_slice_start = start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let updates_slice = slice_by_time(&all_updates, updates_slice_start, end, market_update_ts);
    let lob_slice = slice_by_time(&all_lob_snapshots, start, end, |s| s.ts);

    eprintln!(
        "sliced: {} updates, {} lob snapshots",
        updates_slice.len(),
        lob_slice.len()
    );

    let t2 = std::time::Instant::now();
    let observations: Vec<FactorObservation> =
        build_factor_observations_with_lob(updates_slice, lob_slice, max_quote_age_secs);
    eprintln!(
        "build_factor_observations_with_lob: {:?} ({} observations)",
        t2.elapsed(),
        observations.len()
    );

    if observations.is_empty() {
        eprintln!("no observations — check date range and symbols");
        return;
    }

    // Run regime-aware IC scan.
    let t3 = std::time::Instant::now();
    let mut registry = FactorRegistry::new();
    scan_into_registry(&observations, &mut registry);
    eprintln!("scan_into_registry: {:?} ({} factors registered)", t3.elapsed(), registry.all().len());

    // Print top-N factors per regime × label.
    let regimes = [Regime::Early, Regime::Middle, Regime::Expiry];
    let labels = ["settlement_up", "future_up_ask_change_30s"];

    for regime in &regimes {
        for label in &labels {
            let top = registry.top_n(*regime, label, top_n);
            if top.is_empty() {
                continue;
            }
            println!("\n=== {:?} / {} (top {}) ===", regime, label, top_n);
            println!("{:<30} {:>10} {:>8} {:>10}", "factor", "ic", "dir", "stability");
            println!("{}", "-".repeat(62));
            for meta in &top {
                println!(
                    "{:<30} {:>10.4} {:>8} {:>10.4}",
                    meta.name,
                    meta.ic,
                    if meta.direction > 0 { "+" } else { "-" },
                    meta.stability
                );
            }
        }
    }

    println!("\nDone. Total registered factor entries: {}", registry.all().len());
}
