use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::{
    build_event_summaries, build_factor_observations, factor_metrics, observations_to_frame,
};
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use polars::prelude::*;
use sqlx::postgres::PgPoolOptions;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start timestamp"))
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59).expect("valid end timestamp"))
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
        .with_timezone(&Utc)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required")));
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required")));
    let symbols_csv = flag_value(&args, "--symbols").unwrap_or_else(|| "BTCUSDT".to_string());
    let symbols: Vec<String> = symbols_csv
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    eprintln!("loading factor research window {start} -> {end} for {:?}", symbols);

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let updates = load_from_database_with_options(
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
    .expect("historical load failed");

    eprintln!("loaded {} updates", updates.len());

    let observations = build_factor_observations(&updates);
    let event_rows = build_event_summaries(&observations);
    let metrics = factor_metrics(&observations, &event_rows);
    let frame = observations_to_frame(&observations).expect("observation frame");

    eprintln!("observation_rows={}", observations.len());
    eprintln!("event_rows={}", event_rows.len());
    eprintln!("frame_shape={:?}", frame.shape());

    let mut settlement_metrics: Vec<_> = metrics
        .iter()
        .filter(|metric| metric.label == "settlement_up")
        .collect();
    settlement_metrics.sort_by(|a, b| {
        b.spearman_ic
            .abs()
            .partial_cmp(&a.spearman_ic.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut lag_metrics: Vec<_> = metrics
        .iter()
        .filter(|metric| metric.label == "future_up_ask_change_30s")
        .collect();
    lag_metrics.sort_by(|a, b| {
        b.spearman_ic
            .abs()
            .partial_cmp(&a.spearman_ic.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    eprintln!("\n=== Settlement Factors (Top 10 by |Spearman IC|) ===");
    for metric in settlement_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} n={:<6} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.n,
            metric.pearson_ic,
            metric.spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    eprintln!("\n=== PM Lag Factors (Top 10 by |Spearman IC|) ===");
    for metric in lag_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} n={:<6} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.n,
            metric.pearson_ic,
            metric.spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    let grouped = frame
        .clone()
        .lazy()
        .group_by([col("symbol")])
        .agg([
            len().alias("rows"),
            col("settlement_up").mean().alias("settlement_up_mean"),
            col("future_up_ask_change_30s")
                .mean()
                .alias("future_up_ask_change_30s_mean"),
        ])
        .collect()
        .expect("grouped summary");

    eprintln!("\n=== Symbol Summary ===");
    eprintln!("{grouped}");
}
