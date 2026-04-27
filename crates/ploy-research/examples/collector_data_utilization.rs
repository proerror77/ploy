//! collector_data_utilization — audit collected data vs Factor V2 usage.
//!
//! This report answers two separate questions:
//! 1. Is each collector writing fresh rows for the requested PM5D window?
//! 2. Does the current Factor V2 pipeline actually use the resulting fields?
//!
//! Usage:
//!   cargo run -p ploy-research --features db --example collector_data_utilization -- \
//!     --db-url postgres://... \
//!     --symbols BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
//!     --start-date 2026-04-26 \
//!     --end-date 2026-04-26

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_market_contracts::MarketUpdate;
use ploy_research::{
    FactorObservation, FactorReviewOptions, build_data_health_report,
    build_factor_observations_v2_with_deribit_and_pm_books,
    build_factor_observations_with_lob_sampled, factor_v2_descriptors,
    load_deribit_feature_snapshots, load_research_lob_snapshots_sampled,
    load_research_pm_book_snapshots_sampled,
};
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Debug, Clone)]
struct TableUsage {
    name: &'static str,
    rows: i64,
    latest: Option<DateTime<Utc>>,
    used_by_factor_v2: &'static str,
    note: &'static str,
}

#[derive(Debug, Clone)]
struct SourceUsage {
    source: String,
    rows: i64,
    latest: Option<DateTime<Utc>>,
}

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

fn deribit_currencies(symbols: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for symbol in symbols {
        let upper = symbol.trim().to_ascii_uppercase();
        if upper.starts_with("BTC") {
            out.insert("BTC".to_string());
        } else if upper.starts_with("ETH") {
            out.insert("ETH".to_string());
        } else if upper.starts_with("SOL") {
            out.insert("SOL".to_string());
        }
    }
    out.into_iter().collect()
}

async fn table_usage(
    pool: &sqlx::PgPool,
    name: &'static str,
    query: &str,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    used_by_factor_v2: &'static str,
    note: &'static str,
) -> TableUsage {
    let result: Result<(Option<DateTime<Utc>>, i64), sqlx::Error> = sqlx::query_as(query)
        .bind(symbols)
        .bind(start)
        .bind(end)
        .fetch_one(pool)
        .await;
    match result {
        Ok((latest, rows)) => TableUsage {
            name,
            rows,
            latest,
            used_by_factor_v2,
            note,
        },
        Err(err) => {
            eprintln!("{name} utilization query failed: {err}");
            TableUsage {
                name,
                rows: 0,
                latest: None,
                used_by_factor_v2,
                note,
            }
        }
    }
}

async fn deribit_usage(
    pool: &sqlx::PgPool,
    name: &'static str,
    query: &str,
    currencies: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    used_by_factor_v2: &'static str,
    note: &'static str,
) -> TableUsage {
    if currencies.is_empty() {
        return TableUsage {
            name,
            rows: 0,
            latest: None,
            used_by_factor_v2,
            note,
        };
    }
    let result: Result<(Option<DateTime<Utc>>, i64), sqlx::Error> = sqlx::query_as(query)
        .bind(currencies)
        .bind(start)
        .bind(end)
        .fetch_one(pool)
        .await;
    match result {
        Ok((latest, rows)) => TableUsage {
            name,
            rows,
            latest,
            used_by_factor_v2,
            note,
        },
        Err(err) => {
            eprintln!("{name} utilization query failed: {err}");
            TableUsage {
                name,
                rows: 0,
                latest: None,
                used_by_factor_v2,
                note,
            }
        }
    }
}

async fn pm_source_usage(
    pool: &sqlx::PgPool,
    query: &str,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<SourceUsage> {
    let result: Result<Vec<(String, i64, Option<DateTime<Utc>>)>, sqlx::Error> =
        sqlx::query_as(query)
            .bind(symbols)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await;
    match result {
        Ok(rows) => rows
            .into_iter()
            .map(|(source, rows, latest)| SourceUsage {
                source,
                rows,
                latest,
            })
            .collect(),
        Err(err) => {
            eprintln!("source utilization query failed: {err}");
            Vec::new()
        }
    }
}

fn latest_str(ts: Option<DateTime<Utc>>) -> String {
    ts.map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "-".to_string())
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
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT".to_string())
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
    let stake_usd: f64 = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let currencies = deribit_currencies(&symbols);
    let table_rows = vec![
        table_usage(
            &pool,
            "binance_price_ticks",
            "SELECT max(trade_time), count(*) FROM binance_price_ticks WHERE symbol = ANY($1) AND trade_time >= $2 AND trade_time <= $3",
            &symbols,
            start,
            end,
            "yes",
            "spot drift, volatility, continuation price flow",
        )
        .await,
        table_usage(
            &pool,
            "binance_agg_trade_ticks",
            "SELECT max(trade_time), count(*) FROM binance_agg_trade_ticks WHERE symbol = ANY($1) AND trade_time >= $2 AND trade_time <= $3",
            &symbols,
            start,
            end,
            "yes",
            "aggressor flow, signed volume, continuation candles",
        )
        .await,
        table_usage(
            &pool,
            "binance_lob_ticks",
            "SELECT max(event_time), count(*) FROM binance_lob_ticks WHERE symbol = ANY($1) AND event_time >= $2 AND event_time <= $3",
            &symbols,
            start,
            end,
            "yes",
            "CEX OBI/depth/microprice factors through sampled LOB snapshots",
        )
        .await,
        table_usage(
            &pool,
            "pm_market_metadata",
            "SELECT max(updated_at), count(*) FROM pm_market_metadata WHERE symbol = ANY($1) AND end_time >= $2 AND start_time <= $3",
            &symbols,
            start,
            end,
            "yes",
            "event windows, token mapping, price_to_beat",
        )
        .await,
        table_usage(
            &pool,
            "pm_token_settlements",
            r#"
            WITH token_map AS (
                SELECT DISTINCT trim(both '"' from token::text) AS token_id
                FROM pm_market_metadata m
                CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
                WHERE m.symbol = ANY($1)
                  AND m.end_time >= $2
                  AND m.start_time <= $3
                  AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            )
            SELECT max(s.resolved_at), count(*)
            FROM pm_token_settlements s
            JOIN token_map t ON t.token_id = s.token_id
            WHERE s.resolved = true
            "#,
            &symbols,
            start,
            end,
            "yes",
            "official settlement labels only",
        )
        .await,
        table_usage(
            &pool,
            "clob_quote_ticks",
            r#"
            WITH token_map AS (
                SELECT DISTINCT trim(both '"' from token::text) AS token_id
                FROM pm_market_metadata m
                CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
                WHERE m.symbol = ANY($1)
                  AND m.end_time >= $2
                  AND m.start_time <= $3
                  AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            )
            SELECT max(q.received_at), count(*)
            FROM clob_quote_ticks q
            JOIN token_map t ON t.token_id = q.token_id
            WHERE q.received_at >= $2
              AND q.received_at <= $3
            "#,
            &symbols,
            start,
            end,
            "yes",
            "PM executable top-of-book and size",
        )
        .await,
        table_usage(
            &pool,
            "clob_orderbook_snapshots",
            r#"
            WITH token_map AS (
                SELECT DISTINCT trim(both '"' from token::text) AS token_id
                FROM pm_market_metadata m
                CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
                WHERE m.symbol = ANY($1)
                  AND m.end_time >= $2
                  AND m.start_time <= $3
                  AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            )
            SELECT max(o.received_at), count(*)
            FROM clob_orderbook_snapshots o
            JOIN token_map t ON t.token_id = o.token_id
            WHERE o.received_at >= $2
              AND o.received_at <= $3
            "#,
            &symbols,
            start,
            end,
            "partial",
            "fallback source for top-of-book; full PM depth factors not yet wired",
        )
        .await,
        table_usage(
            &pool,
            "clob_trade_ticks",
            r#"
            WITH token_map AS (
                SELECT DISTINCT trim(both '"' from token::text) AS token_id
                FROM pm_market_metadata m
                CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
                WHERE m.symbol = ANY($1)
                  AND m.end_time >= $2
                  AND m.start_time <= $3
                  AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            )
            SELECT max(t.trade_ts), count(*)
            FROM clob_trade_ticks t
            JOIN token_map tm ON tm.token_id = t.token_id
            WHERE t.trade_ts >= $2
              AND t.trade_ts <= $3
            "#,
            &symbols,
            start,
            end,
            "no",
            "PM trade prints are not yet Factor V2 inputs",
        )
        .await,
        deribit_usage(
            &pool,
            "deribit_iv_ticks",
            "SELECT max(creation_ts), count(*) FROM deribit_iv_ticks WHERE currency = ANY($1) AND creation_ts >= $2 AND creation_ts <= $3",
            &currencies,
            start,
            end,
            "yes",
            "Deribit IV level/spread/change factors",
        )
        .await,
        deribit_usage(
            &pool,
            "deribit_atm_greeks_ticks",
            "SELECT max(source_ts), count(*) FROM deribit_atm_greeks_ticks WHERE currency = ANY($1) AND source_ts >= $2 AND source_ts <= $3",
            &currencies,
            start,
            end,
            "conditional",
            "Greeks are only usable when source_ts is fresh inside the window",
        )
        .await,
        table_usage(
            &pool,
            "binance_klines",
            "SELECT max(close_time), count(*) FROM binance_klines WHERE symbol = ANY($1) AND close_time >= $2 AND close_time <= $3",
            &symbols,
            start,
            end,
            "no",
            "continuation currently rebuilt from tick/aggTrade flow, not this table",
        )
        .await,
        table_usage(
            &pool,
            "reference_price_ticks",
            "SELECT max(price_time), count(*) FROM reference_price_ticks WHERE lower(symbol) = ANY($1) AND price_time >= $2 AND price_time <= $3",
            &symbols.iter().map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>(),
            start,
            end,
            "no",
            "not part of PM5D crypto Factor V2 review path",
        )
        .await,
    ];

    let quote_sources = pm_source_usage(
        &pool,
        r#"
        WITH token_map AS (
            SELECT DISTINCT trim(both '"' from token::text) AS token_id
            FROM pm_market_metadata m
            CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
            WHERE m.symbol = ANY($1)
              AND m.end_time >= $2
              AND m.start_time <= $3
              AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        )
        SELECT q.source, count(*), max(q.received_at)
        FROM clob_quote_ticks q
        JOIN token_map t ON t.token_id = q.token_id
        WHERE q.received_at >= $2
          AND q.received_at <= $3
        GROUP BY q.source
        ORDER BY count(*) DESC
        "#,
        &symbols,
        start,
        end,
    )
    .await;

    let snapshot_sources = pm_source_usage(
        &pool,
        r#"
        WITH token_map AS (
            SELECT DISTINCT trim(both '"' from token::text) AS token_id
            FROM pm_market_metadata m
            CROSS JOIN LATERAL jsonb_array_elements((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb) token
            WHERE m.symbol = ANY($1)
              AND m.end_time >= $2
              AND m.start_time <= $3
              AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        )
        SELECT o.source, count(*), max(o.received_at)
        FROM clob_orderbook_snapshots o
        JOIN token_map t ON t.token_id = o.token_id
        WHERE o.received_at >= $2
          AND o.received_at <= $3
        GROUP BY o.source
        ORDER BY count(*) DESC
        "#,
        &symbols,
        start,
        end,
    )
    .await;

    let history_start = start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(lob_sample_secs.max(1)).unwrap_or(1);
    let updates = load_from_database_with_options(
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
    .expect("historical load failed");
    let lob_snapshots =
        load_research_lob_snapshots_sampled(&pool, &symbols, history_start, end, lob_sample_secs)
            .await
            .expect("lob snapshot load failed");
    let pm_book_snapshots = load_research_pm_book_snapshots_sampled(
        &pool,
        &symbols,
        history_start,
        end,
        lob_sample_secs,
    )
    .await
    .expect("PM book snapshot load failed");
    let deribit_snapshots =
        load_deribit_feature_snapshots(&pool, &symbols, start, end, observation_sample_secs).await;
    let updates_slice = slice_by_time(&updates, history_start, end, MarketUpdate::sort_ts);
    let lob_slice = slice_by_time(&lob_snapshots, history_start, end, |snapshot| snapshot.ts);
    let observations: Vec<FactorObservation> = build_factor_observations_with_lob_sampled(
        updates_slice,
        lob_slice,
        max_quote_age_secs,
        observation_sample_secs,
    );
    let review_options = FactorReviewOptions {
        stake_usd,
        ..Default::default()
    };
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &pm_book_snapshots,
        &review_options,
    );
    let health = build_data_health_report(&observations, &v2_rows);

    println!("=== Collector Data Utilization ===");
    println!(
        "window_start={} window_end={} symbols={} deribit_currencies={}",
        start.to_rfc3339(),
        end.to_rfc3339(),
        symbols.join("|"),
        currencies.join("|")
    );
    println!(
        "updates={} lob_snapshots={} deribit_snapshots={} factor_observations={} v2_rows={}",
        updates.len(),
        lob_snapshots.len(),
        deribit_snapshots.len(),
        observations.len(),
        v2_rows.len()
    );

    println!("\n=== Collector Tables ===");
    println!("table,window_rows,latest_ts,used_by_factor_v2,note");
    for row in &table_rows {
        println!(
            "{},{},{},{},{}",
            row.name,
            row.rows,
            latest_str(row.latest),
            row.used_by_factor_v2,
            row.note
        );
    }

    println!("\n=== PM Quote Sources ===");
    println!("source,window_rows,latest_ts");
    for row in &quote_sources {
        println!("{},{},{}", row.source, row.rows, latest_str(row.latest));
    }

    println!("\n=== PM Snapshot Sources ===");
    println!("source,window_rows,latest_ts");
    for row in &snapshot_sources {
        println!("{},{},{}", row.source, row.rows, latest_str(row.latest));
    }

    println!("\n=== Factor V2 Data Health ===");
    println!(
        "source_obs={},v2_rows={},settlement_labels={},entry_quote_rows={},entry_size_rows={},entry_fill_rate={:.4},exit_fill_rate={:.4},full_depth_entry_fill_rate={:.4},full_depth_exit_fill_rate={:.4},executable_pnl_rows={},full_depth_executable_pnl_rows={},deribit_rows={},avg_pm_lag_secs={:.2},avg_entry_sweep_slip_bps={:.2},avg_exit_sweep_slip_bps={:.2}",
        health.source_observations,
        health.v2_rows,
        health.settlement_label_rows,
        health.entry_quote_rows,
        health.entry_size_rows,
        health.entry_fill_rate(),
        health.exit_fill_rate(),
        health.full_depth_entry_fill_rate(),
        health.full_depth_exit_fill_rate(),
        health.executable_pnl_rows,
        health.full_depth_executable_pnl_rows,
        health.deribit_rows,
        health.avg_pm_lag_secs,
        health.avg_entry_sweep_slippage_bps,
        health.avg_exit_sweep_slippage_bps
    );

    println!("\n=== Factor Descriptor Coverage ===");
    println!("factor,family,finite_rows,coverage,nan_rate");
    for descriptor in factor_v2_descriptors() {
        let finite = v2_rows
            .iter()
            .filter(|row| (descriptor.accessor)(row).is_finite())
            .count();
        let coverage = if v2_rows.is_empty() {
            0.0
        } else {
            finite as f64 / v2_rows.len() as f64
        };
        println!(
            "{},{},{},{:.4},{:.4}",
            descriptor.name,
            descriptor.family.as_str(),
            finite,
            coverage,
            1.0 - coverage
        );
    }

    println!("\n=== Known Utilization Gaps ===");
    println!("gap,status,next_action");
    println!(
        "pm_full_depth,used_for_sweep_labels,compare top-of-book live parity labels against full-depth sweep labels before changing live order aggressiveness"
    );
    println!(
        "pm_trade_prints,collector_added_factor_pending,deploy collect-pm-trades and then wire trade imbalance/burst factors into Factor V2"
    );
    println!(
        "deribit_greeks,conditional,only promote delta/gamma/vega/theta when deribit_atm_greeks_ticks has fresh source_ts in-window"
    );
    println!(
        "reference_prices,not_used,keep out of PM5D crypto factor review unless adding Chainlink/Pyth cross-source lag features"
    );
    println!(
        "binance_klines,not_used,current continuation features are rebuilt point-in-time from spot/aggTrade streams"
    );
}
