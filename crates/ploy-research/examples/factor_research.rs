use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_research::{
    aggregate_factor_metrics, build_event_summaries, build_factor_observations_with_lob,
    factor_metrics, load_research_lob_snapshots_sampled,
};
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::traits::MarketUpdate;
use sqlx::postgres::PgPoolOptions;

#[derive(Debug, Clone, sqlx::FromRow)]
struct ValidWindowRow {
    symbol: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    event_count: i64,
}

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

async fn discover_valid_windows(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    max_windows: i64,
) -> Vec<ValidWindowRow> {
    sqlx::query_as(
        r#"
        SELECT
            m.symbol,
            m.start_time,
            m.end_time,
            COUNT(*)::bigint AS event_count
        FROM pm_market_metadata m
        WHERE m.symbol = ANY($1)
          AND m.start_time >= $2
          AND m.end_time <= $3
          AND EXTRACT(EPOCH FROM (m.end_time - m.start_time)) = 300
          AND EXISTS (
              SELECT 1
              FROM pm_token_settlements s
              WHERE s.market_slug = m.market_slug
                AND s.resolved = true
          )
          AND EXISTS (
              SELECT 1
              FROM binance_lob_ticks l
              WHERE l.symbol = m.symbol
                AND l.event_time >= m.start_time
                AND l.event_time <= m.end_time
          )
        GROUP BY m.symbol, m.start_time, m.end_time
        ORDER BY m.start_time
        LIMIT $4
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(max_windows)
    .fetch_all(pool)
    .await
    .expect("valid window discovery failed")
}

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
            // Mirrors database.rs update_ts: subtract window + 1h buffer so EventDiscovered
            // sorts before all quotes for the same event (quotes can arrive before start_time).
            *end_time
                - chrono::Duration::seconds(*window_secs as i64)
                - chrono::Duration::hours(1)
        }
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

/// Slices a sorted slice to items whose timestamp falls in `[start, end]`.
/// Precondition: `items` must be sorted by `ts_fn` in ascending order.
fn slice_by_time<'a, T>(
    items: &'a [T],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    ts_fn: impl Fn(&T) -> DateTime<Utc>,
) -> &'a [T] {
    let lo = items.partition_point(|x| ts_fn(x) < start);
    let hi = items.partition_point(|x| ts_fn(x) <= end);
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
    let symbols_csv = flag_value(&args, "--symbols").unwrap_or_else(|| "BTCUSDT".to_string());
    let symbols: Vec<String> = symbols_csv
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let discover_windows = args.iter().any(|arg| arg == "--discover-valid-5m-windows");
    let max_windows: i64 = flag_value(&args, "--max-windows")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(8);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    // Downsample LOB snapshots: keep 1 tick per N seconds (default 5).
    // Reduces JSONB transfer ~Nx with minimal research quality loss.
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);

    eprintln!("loading factor research range {start} -> {end} for {:?}", symbols);

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let windows = if discover_windows {
        let discovered = discover_valid_windows(&pool, &symbols, start, end, max_windows).await;
        eprintln!("\n=== Discovered Valid 5m Windows ===");
        for window in &discovered {
            eprintln!(
                "{} {} -> {} events={}",
                window.symbol, window.start_time, window.end_time, window.event_count
            );
        }
        discovered
    } else {
        vec![ValidWindowRow {
            symbol: symbols.first().cloned().unwrap_or_else(|| "BTCUSDT".to_string()),
            start_time: start,
            end_time: end,
            event_count: 0,
        }]
    };

    // Compute global time range covering all windows for bulk load.
    let global_start = windows
        .iter()
        .map(|w| w.start_time)
        .min()
        .unwrap_or(start);
    let global_end = windows
        .iter()
        .map(|w| w.end_time)
        .max()
        .unwrap_or(end);

    // Collect all unique symbols across windows.
    let bulk_symbols: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        windows
            .iter()
            .filter(|w| seen.insert(w.symbol.clone()))
            .map(|w| w.symbol.clone())
            .collect()
    };

    eprintln!(
        "\nbulk loading {} -> {} for {:?}",
        global_start, global_end, bulk_symbols
    );

    let t0 = std::time::Instant::now();
    let all_updates = load_from_database_with_options(
        &pool,
        &bulk_symbols,
        global_start,
        global_end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            ..Default::default()
        },
    )
    .await
    .expect("bulk historical load failed");
    eprintln!("load_from_database_with_options: {:?}", t0.elapsed());

    let t1 = std::time::Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        &pool,
        &bulk_symbols,
        global_start,
        global_end,
        lob_sample_secs,
    )
    .await
    .expect("bulk lob snapshot load failed");
    eprintln!("load_research_lob_snapshots_sampled (sample_secs={}): {:?}", lob_sample_secs, t1.elapsed());

    eprintln!(
        "bulk loaded {} updates, {} lob snapshots",
        all_updates.len(),
        all_lob_snapshots.len()
    );

    let mut all_metrics = Vec::new();
    let mut total_observations = 0usize;
    let mut total_event_rows = 0usize;

    for window in &windows {
        // EventDiscovered sort-ts is end_time - window_secs - 1h (see market_update_ts).
        // Extend the lower bound by that same offset so EventDiscovered items are included.
        let updates_slice_start =
            window.start_time - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
        let updates_slice = slice_by_time(
            &all_updates,
            updates_slice_start,
            window.end_time,
            market_update_ts,
        );

        let lob_slice = slice_by_time(
            &all_lob_snapshots,
            window.start_time,
            window.end_time,
            |s| s.ts,
        );

        eprintln!(
            "\nwindow {} {} -> {} updates={} lob={}",
            window.symbol,
            window.start_time,
            window.end_time,
            updates_slice.len(),
            lob_slice.len(),
        );

        // Multi-symbol data in the slice is safe: the function maintains per-symbol
        // state internally, so updates from other symbols do not affect this window's factors.
        let observations =
            build_factor_observations_with_lob(&updates_slice, &lob_slice, max_quote_age_secs);
        let event_rows = build_event_summaries(&observations);
        let metrics = factor_metrics(&observations, &event_rows);

        total_observations += observations.len();
        total_event_rows += event_rows.len();
        all_metrics.push(metrics);
    }

    let aggregated = aggregate_factor_metrics(&all_metrics);

    eprintln!("\nobservation_rows={}", total_observations);
    eprintln!("event_rows={}", total_event_rows);

    let mut settlement_metrics: Vec<_> = aggregated
        .iter()
        .filter(|metric| metric.label == "settlement_up")
        .collect();
    settlement_metrics.sort_by(|a, b| {
        b.mean_spearman_ic
            .abs()
            .partial_cmp(&a.mean_spearman_ic.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut lag_metrics: Vec<_> = aggregated
        .iter()
        .filter(|metric| metric.label == "future_up_ask_change_30s")
        .collect();
    lag_metrics.sort_by(|a, b| {
        b.mean_spearman_ic
            .abs()
            .partial_cmp(&a.mean_spearman_ic.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    eprintln!("\n=== Settlement Factors (Top 10 by |Mean Spearman IC|) ===");
    for metric in settlement_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} windows={:<4} mean_n={:<8.1} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.windows,
            metric.mean_n,
            metric.mean_pearson_ic,
            metric.mean_spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    eprintln!("\n=== PM Lag Factors (Top 10 by |Mean Spearman IC|) ===");
    for metric in lag_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} windows={:<4} mean_n={:<8.1} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.windows,
            metric.mean_n,
            metric.mean_pearson_ic,
            metric.mean_spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    eprintln!("\n=== Window Count ===");
    eprintln!("{}", windows.len());
}
