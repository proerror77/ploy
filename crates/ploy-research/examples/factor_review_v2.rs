//! factor_review_v2 — execution-aware PM5D factor review
//!
//! Loads official-settlement historical data, builds the legacy
//! `FactorObservation` rows, derives side-aware `FactorObservationV2` rows, and
//! prints data-health plus single-factor execution metrics.
//!
//! Usage:
//!   cargo run -p ploy-research --features db --example factor_review_v2 -- \
//!     --db-url postgres://... \
//!     --symbols BTCUSDT,ETHUSDT,SOLUSDT \
//!     --start-date 2026-04-24 \
//!     --end-date 2026-04-24 \
//!     --stake-usd 15 \
//!     [--lob-sample-secs 5] \
//!     [--observation-sample-secs 30] \
//!     [--max-quote-age-secs 30] \
//!     [--top-n 20]

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_market_contracts::MarketUpdate;
use ploy_research::{
    DeribitFeatureSnapshot, FactorObservation, FactorReviewOptions,
    build_factor_observations_with_lob_sampled, format_factor_review_v2_report,
    load_research_lob_snapshots_sampled, review_factors_v2_with_deribit,
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
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let top_n: usize = flag_value(&args, "--top-n")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(20);
    let options = FactorReviewOptions {
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

    eprintln!(
        "factor_review_v2: {} -> {} for {:?}, stake_usd={:.2}, observation_sample_secs={}",
        start, end, symbols, options.stake_usd, observation_sample_secs
    );

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let all_updates = load_from_database_with_options(
        &pool,
        &symbols,
        start,
        end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            include_l2: false,
            ..Default::default()
        },
    )
    .await
    .expect("bulk historical load failed");
    eprintln!("updates: {}", all_updates.len());

    let all_lob_snapshots =
        load_research_lob_snapshots_sampled(&pool, &symbols, start, end, lob_sample_secs)
            .await
            .expect("bulk lob snapshot load failed");
    eprintln!("lob_snapshots: {}", all_lob_snapshots.len());

    let deribit_snapshots =
        load_deribit_feature_snapshots(&pool, &symbols, start, end, observation_sample_secs).await;
    eprintln!("deribit_snapshots: {}", deribit_snapshots.len());

    let updates_slice_start = start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let updates_slice = slice_by_time(
        &all_updates,
        updates_slice_start,
        end,
        MarketUpdate::sort_ts,
    );
    let lob_slice = slice_by_time(&all_lob_snapshots, start, end, |snapshot| snapshot.ts);

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

    let report = review_factors_v2_with_deribit(&observations, &deribit_snapshots, options);
    println!("{}", format_factor_review_v2_report(&report, top_n));
}

async fn load_deribit_feature_snapshots(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i64,
) -> Vec<DeribitFeatureSnapshot> {
    let currencies: Vec<String> = symbols
        .iter()
        .filter_map(|symbol| symbol_to_deribit_currency(symbol))
        .collect();
    if currencies.is_empty() {
        return Vec::new();
    }

    let sample_secs = sample_secs.clamp(1, 300) as i32;
    let mut snapshots = Vec::new();
    let current_iv_rows: Vec<(
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        DateTime<Utc>,
    )> = match sqlx::query_as(
        r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.bid_iv::double precision,
            d.ask_iv::double precision,
            d.underlying_price::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, bid_iv, ask_iv, underlying_price
            FROM deribit_iv_ticks d
            WHERE d.currency = c.currency
              AND d.creation_ts <= b.bucket_ts
              AND d.creation_ts > b.bucket_ts - interval '5 minutes'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.creation_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#,
    )
    .bind(&currencies)
    .bind(start)
    .bind(end)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("deribit iv load failed: {err}");
            Vec::new()
        }
    };

    for (currency, mark_iv, bid_iv, ask_iv, underlying_price, ts) in current_iv_rows {
        snapshots.push(DeribitFeatureSnapshot {
            symbol: deribit_currency_to_symbol(&currency),
            ts,
            mark_iv: mark_iv.unwrap_or(f64::NAN),
            bid_iv: bid_iv.unwrap_or(f64::NAN),
            ask_iv: ask_iv.unwrap_or(f64::NAN),
            underlying_price: underlying_price.unwrap_or(f64::NAN),
            delta: f64::NAN,
            gamma: f64::NAN,
            vega: f64::NAN,
            theta: f64::NAN,
        });
    }

    if snapshots.is_empty() {
        let legacy_iv_rows: Vec<(String, Option<f64>, Option<f64>, DateTime<Utc>)> =
            match sqlx::query_as(
                r#"
                SELECT
                    currency,
                    COALESCE(atm_iv, iv_close, iv_open)::double precision,
                    iv_close::double precision,
                    timestamp
                FROM deribit_iv_ticks
                WHERE currency = ANY($1)
                  AND timestamp >= $2
                  AND timestamp <= $3
                ORDER BY timestamp
                "#,
            )
            .bind(&currencies)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    eprintln!("legacy deribit iv load skipped: {err}");
                    Vec::new()
                }
            };
        for (currency, atm_iv, iv_close, ts) in legacy_iv_rows {
            let mark_iv = atm_iv.or(iv_close).unwrap_or(f64::NAN);
            snapshots.push(DeribitFeatureSnapshot {
                symbol: deribit_currency_to_symbol(&currency),
                ts,
                mark_iv,
                bid_iv: f64::NAN,
                ask_iv: f64::NAN,
                underlying_price: f64::NAN,
                delta: f64::NAN,
                gamma: f64::NAN,
                vega: f64::NAN,
                theta: f64::NAN,
            });
        }
    }

    let greeks_rows: Vec<(
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        DateTime<Utc>,
    )> = match sqlx::query_as(
        r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.delta::double precision,
            d.gamma::double precision,
            d.vega::double precision,
            d.theta::double precision,
            d.underlying_price::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, delta, gamma, vega, theta, underlying_price
            FROM deribit_atm_greeks_ticks d
            WHERE d.currency = c.currency
              AND d.source_ts <= b.bucket_ts
              AND d.source_ts > b.bucket_ts - interval '5 minutes'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.source_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#,
    )
    .bind(&currencies)
    .bind(start)
    .bind(end)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("deribit greeks load failed: {err}");
            Vec::new()
        }
    };

    for (currency, mark_iv, delta, gamma, vega, theta, underlying_price, ts) in greeks_rows {
        snapshots.push(DeribitFeatureSnapshot {
            symbol: deribit_currency_to_symbol(&currency),
            ts,
            mark_iv: mark_iv.unwrap_or(f64::NAN),
            bid_iv: f64::NAN,
            ask_iv: f64::NAN,
            underlying_price: underlying_price.unwrap_or(f64::NAN),
            delta: delta.unwrap_or(f64::NAN),
            gamma: gamma.unwrap_or(f64::NAN),
            vega: vega.unwrap_or(f64::NAN),
            theta: theta.unwrap_or(f64::NAN),
        });
    }

    snapshots.sort_by_key(|snapshot| (snapshot.symbol.clone(), snapshot.ts));
    snapshots
}

fn symbol_to_deribit_currency(symbol: &str) -> Option<String> {
    let upper = symbol.trim().to_ascii_uppercase();
    if upper.starts_with("BTC") {
        Some("BTC".to_string())
    } else if upper.starts_with("ETH") {
        Some("ETH".to_string())
    } else if upper.starts_with("SOL") {
        Some("SOL".to_string())
    } else {
        None
    }
}

fn deribit_currency_to_symbol(currency: &str) -> String {
    match currency.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTCUSDT".to_string(),
        "ETH" => "ETHUSDT".to_string(),
        "SOL" => "SOLUSDT".to_string(),
        other => format!("{other}USDT"),
    }
}
