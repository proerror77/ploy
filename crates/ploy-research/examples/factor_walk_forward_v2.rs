//! factor_walk_forward_v2 — executable PM5D factor walk-forward review
//!
//! The training window fits each factor's direction and selected-quantile
//! threshold. The following test window only applies that trained threshold and
//! scores executable PnL after PM CLOB fillability.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_market_contracts::MarketUpdate;
use ploy_research::{
    FactorObservation, FactorReviewOptions, FactorWalkForwardOptions,
    build_factor_observations_with_lob_sampled, format_factor_walk_forward_v2_report,
    load_deribit_feature_snapshots, load_research_lob_snapshots_sampled,
    walk_forward_factors_v2_with_deribit,
};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
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
    let hi = items.partition_point(|item| ts_fn(item) < end);
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
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let review = FactorReviewOptions {
        stake_usd: flag_value(&args, "--stake-usd")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(15.0),
        min_observations: flag_value(&args, "--min-observations")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        top_quantile: flag_value(&args, "--top-quantile")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0.2),
    };
    let options = FactorWalkForwardOptions {
        review,
        train_window_days: flag_value(&args, "--train-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(2),
        test_window_days: flag_value(&args, "--test-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        step_days: flag_value(&args, "--step-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        top_n: flag_value(&args, "--top-n")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
    };

    eprintln!(
        "factor_walk_forward_v2: {} -> {} for {:?}, stake_usd={:.2}, train_days={}, test_days={}, observation_sample_secs={}",
        start,
        end,
        symbols,
        options.review.stake_usd,
        options.train_window_days,
        options.test_window_days,
        observation_sample_secs
    );

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let history_start = start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(lob_sample_secs.max(1)).unwrap_or(1);
    let all_updates = load_from_database_with_options(
        &pool,
        &symbols,
        history_start,
        end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            include_l2: false,
            spot_sample_secs: historical_sample_secs,
            lob_sample_secs: historical_sample_secs,
            ..Default::default()
        },
    )
    .await
    .expect("bulk historical load failed");
    eprintln!("updates: {}", all_updates.len());

    let all_lob_snapshots =
        load_research_lob_snapshots_sampled(&pool, &symbols, history_start, end, lob_sample_secs)
            .await
            .expect("bulk lob snapshot load failed");
    eprintln!("lob_snapshots: {}", all_lob_snapshots.len());

    let deribit_snapshots =
        load_deribit_feature_snapshots(&pool, &symbols, start, end, observation_sample_secs).await;
    eprintln!("deribit_snapshots: {}", deribit_snapshots.len());

    let updates_slice = slice_by_time(&all_updates, history_start, end, MarketUpdate::sort_ts);
    let lob_slice = slice_by_time(&all_lob_snapshots, history_start, end, |snapshot| {
        snapshot.ts
    });

    let observations: Vec<FactorObservation> = build_factor_observations_with_lob_sampled(
        updates_slice,
        lob_slice,
        max_quote_age_secs,
        observation_sample_secs,
    );
    eprintln!("factor_observations: {}", observations.len());

    if observations.is_empty() {
        eprintln!("no observations — check date range, symbols, quote coverage, and settlements");
        return;
    }

    let report = walk_forward_factors_v2_with_deribit(
        &observations,
        &deribit_snapshots,
        start,
        end,
        options,
    );
    println!("{}", format_factor_walk_forward_v2_report(&report));
}
