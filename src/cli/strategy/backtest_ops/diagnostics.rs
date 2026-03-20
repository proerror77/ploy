use super::*;
use alloy::primitives::U256;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::Row;
use std::collections::{BTreeSet, HashMap, HashSet};

const PM5_BUCKET_SECS: i64 = 300;
const PM5_EDGE_SLACK_BUCKETS: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BucketRun {
    start: DateTime<Utc>,
    end_exclusive: DateTime<Utc>,
    buckets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pm5mReplayWindow {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub auto_trim_message: Option<String>,
}

#[derive(Debug, Clone)]
struct Pm5mCoverageSnapshot {
    spot_buckets: BTreeSet<DateTime<Utc>>,
    l2_buckets: BTreeSet<DateTime<Utc>>,
    quote_buckets: BTreeSet<DateTime<Utc>>,
    lob_buckets: BTreeSet<DateTime<Utc>>,
    event_buckets: BTreeSet<DateTime<Utc>>,
}

impl Pm5mCoverageSnapshot {
    fn common_buckets(&self) -> BTreeSet<DateTime<Utc>> {
        let mut common = self
            .spot_buckets
            .intersection(&self.l2_buckets)
            .copied()
            .collect::<BTreeSet<_>>();
        common = common
            .intersection(&self.quote_buckets)
            .copied()
            .collect::<BTreeSet<_>>();
        common = common
            .intersection(&self.lob_buckets)
            .copied()
            .collect::<BTreeSet<_>>();
        common
            .intersection(&self.event_buckets)
            .copied()
            .collect::<BTreeSet<_>>()
    }

    fn longest_common_run(&self) -> Option<BucketRun> {
        longest_contiguous_bucket_run(&self.common_buckets())
    }
}

pub(super) async fn resolve_pm5_replay_window(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    auto_trim: bool,
) -> Result<Pm5mReplayWindow> {
    if from.is_none() || to.is_none() {
        return Ok(Pm5mReplayWindow {
            from,
            to,
            auto_trim_message: None,
        });
    }

    let snapshot = load_pm5_coverage_snapshot(pool, symbols, from, to).await?;
    evaluate_pm5_requested_window(&snapshot, from, to, auto_trim)
}

fn evaluate_pm5_requested_window(
    snapshot: &Pm5mCoverageSnapshot,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    auto_trim: bool,
) -> Result<Pm5mReplayWindow> {
    let from = from.expect("checked by caller");
    let to = to.expect("checked by caller");
    let requested_start = bucket_floor_5m(from);
    let requested_end_exclusive = bucket_floor_5m(to) + Duration::seconds(PM5_BUCKET_SECS);
    let slack = Duration::seconds(PM5_BUCKET_SECS * PM5_EDGE_SLACK_BUCKETS);

    let Some(run) = snapshot.longest_common_run() else {
        anyhow::bail!(
            "pm_5m_directional replay coverage is empty for requested window {} .. {}. \
critical 5m feeds have no common buckets across spot/L2/PM quote/PM LOB/event data. \
Bucket counts: spot={}, l2={}, pm_quote={}, pm_lob={}, event={}. \
Run `--diagnose-db` and choose a shorter contiguous window.",
            from.to_rfc3339(),
            to.to_rfc3339(),
            snapshot.spot_buckets.len(),
            snapshot.l2_buckets.len(),
            snapshot.quote_buckets.len(),
            snapshot.lob_buckets.len(),
            snapshot.event_buckets.len(),
        );
    };

    let start_ok = run.start <= requested_start + slack;
    let end_ok = run.end_exclusive >= requested_end_exclusive - slack;
    if start_ok && end_ok {
        return Ok(Pm5mReplayWindow {
            from: Some(from),
            to: Some(to),
            auto_trim_message: None,
        });
    }

    if auto_trim {
        let effective_from = from.max(run.start);
        let effective_to = to.min(run.end_exclusive);
        if effective_from < effective_to {
            return Ok(Pm5mReplayWindow {
                from: Some(effective_from),
                to: Some(effective_to),
                auto_trim_message: Some(format!(
                    "Auto-trimming pm_5m_directional replay window {} .. {} to {} .. {} inside \
the longest contiguous common-coverage range {} .. {} ({} buckets, {:.2} hours).",
                    from.to_rfc3339(),
                    to.to_rfc3339(),
                    effective_from.to_rfc3339(),
                    effective_to.to_rfc3339(),
                    run.start.to_rfc3339(),
                    run.end_exclusive.to_rfc3339(),
                    run.buckets,
                    run.buckets as f64 * 5.0 / 60.0,
                )),
            });
        }
    }

    anyhow::bail!(
        "pm_5m_directional replay coverage is incomplete for requested window {} .. {}. \
Longest contiguous common 5m coverage across spot/L2/PM quote/PM LOB/event data is {} .. {} \
({} buckets, {:.2} hours). Bucket counts: spot={}, l2={}, pm_quote={}, pm_lob={}, event={}. \
Use a shorter window inside that range or run `--diagnose-db`.",
        from.to_rfc3339(),
        to.to_rfc3339(),
        run.start.to_rfc3339(),
        run.end_exclusive.to_rfc3339(),
        run.buckets,
        run.buckets as f64 * 5.0 / 60.0,
        snapshot.spot_buckets.len(),
        snapshot.l2_buckets.len(),
        snapshot.quote_buckets.len(),
        snapshot.lob_buckets.len(),
        snapshot.event_buckets.len(),
    );
}

async fn load_pm5_coverage_snapshot(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Pm5mCoverageSnapshot> {
    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

    let token_ids = load_pm5_token_ids(pool, &symbol_list, from, to).await?;
    Ok(Pm5mCoverageSnapshot {
        spot_buckets: load_pm5_spot_buckets(pool, &symbol_list, from, to).await?,
        l2_buckets: load_bucket_set(
            pool,
            "binance_lob_ticks",
            "event_time",
            Some("symbol = ANY($1)"),
            symbol_list.clone(),
            from,
            to,
        )
        .await?,
        quote_buckets: load_bucket_set_for_tokens(
            pool,
            "clob_quote_ticks",
            "received_at",
            &token_ids,
            from,
            to,
        )
        .await?,
        lob_buckets: load_bucket_set_for_tokens(
            pool,
            "clob_orderbook_snapshots",
            "received_at",
            &token_ids,
            from,
            to,
        )
        .await?,
        event_buckets: load_bucket_set(
            pool,
            "pm_market_metadata",
            "start_time",
            Some("symbol = ANY($1)"),
            symbol_list,
            from,
            to,
        )
        .await?,
    })
}

async fn load_pm5_token_ids(
    pool: &sqlx::PgPool,
    symbols: &Option<Vec<String>>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Vec<String>> {
    if !table_exists(pool, "pm_market_metadata").await? {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_scalar::<_, String>(
        r#"
        SELECT DISTINCT jsonb_array_elements_text((raw_market->>'clobTokenIds')::jsonb) AS token_id
        FROM pm_market_metadata
        WHERE raw_market IS NOT NULL
          AND raw_market ? 'clobTokenIds'
          AND ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2)
          AND ($3::timestamptz IS NULL OR start_time <= $3)
        "#,
    )
    .bind(symbols.clone())
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut normalized = rows
        .into_iter()
        .filter_map(|token_id| normalize_clob_token_id(&token_id))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

async fn load_pm5_spot_buckets(
    pool: &sqlx::PgPool,
    symbols: &Option<Vec<String>>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<BTreeSet<DateTime<Utc>>> {
    let mut buckets = BTreeSet::new();

    buckets.extend(
        load_bucket_set(
            pool,
            "sync_records",
            "timestamp",
            Some("symbol = ANY($1)"),
            symbols.clone(),
            from,
            to,
        )
        .await?,
    );
    buckets.extend(
        load_bucket_set(
            pool,
            "binance_price_ticks",
            "trade_time",
            Some("symbol = ANY($1)"),
            symbols.clone(),
            from,
            to,
        )
        .await?,
    );
    buckets.extend(
        load_bucket_set(
            pool,
            "binance_klines",
            "close_time",
            Some("symbol = ANY($1)"),
            symbols.clone(),
            from,
            to,
        )
        .await?,
    );

    Ok(buckets)
}

async fn load_bucket_set(
    pool: &sqlx::PgPool,
    table: &str,
    ts_column: &str,
    symbol_predicate: Option<&str>,
    symbols: Option<Vec<String>>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<BTreeSet<DateTime<Utc>>> {
    if !table_exists(pool, table).await? {
        return Ok(BTreeSet::new());
    }

    let symbol_clause = symbol_predicate
        .map(|predicate| format!("AND ($1::text[] IS NULL OR {predicate})"))
        .unwrap_or_default();
    let sql = format!(
        r#"
        SELECT DISTINCT to_timestamp(floor(EXTRACT(EPOCH FROM {ts_column}) / {bucket}) * {bucket}) AS bucket
        FROM {table}
        WHERE 1=1
          {symbol_clause}
          AND ($2::timestamptz IS NULL OR {ts_column} >= $2)
          AND ($3::timestamptz IS NULL OR {ts_column} <= $3)
        ORDER BY bucket
        "#,
        bucket = PM5_BUCKET_SECS,
        table = table,
        ts_column = ts_column,
        symbol_clause = symbol_clause,
    );

    let rows = sqlx::query_scalar::<_, DateTime<Utc>>(&sql)
        .bind(symbols)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    Ok(rows.into_iter().collect())
}

async fn load_bucket_set_for_tokens(
    pool: &sqlx::PgPool,
    table: &str,
    ts_column: &str,
    token_ids: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<BTreeSet<DateTime<Utc>>> {
    if token_ids.is_empty() || !table_exists(pool, table).await? {
        return Ok(BTreeSet::new());
    }

    let sql = format!(
        r#"
        SELECT DISTINCT to_timestamp(floor(EXTRACT(EPOCH FROM {ts_column}) / {bucket}) * {bucket}) AS bucket
        FROM {table}
        WHERE token_id = ANY($1)
          AND ($2::timestamptz IS NULL OR {ts_column} >= $2)
          AND ($3::timestamptz IS NULL OR {ts_column} <= $3)
        ORDER BY bucket
        "#,
        bucket = PM5_BUCKET_SECS,
        table = table,
        ts_column = ts_column,
    );

    let rows = sqlx::query_scalar::<_, DateTime<Utc>>(&sql)
        .bind(token_ids)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    Ok(rows.into_iter().collect())
}

fn longest_contiguous_bucket_run(buckets: &BTreeSet<DateTime<Utc>>) -> Option<BucketRun> {
    let step = Duration::seconds(PM5_BUCKET_SECS);
    let mut iter = buckets.iter().copied();
    let first = iter.next()?;

    let mut best = BucketRun {
        start: first,
        end_exclusive: first + step,
        buckets: 1,
    };
    let mut current = best;
    let mut prev = first;

    for bucket in iter {
        if bucket == prev + step {
            current.end_exclusive = bucket + step;
            current.buckets += 1;
        } else {
            if current.buckets > best.buckets {
                best = current;
            }
            current = BucketRun {
                start: bucket,
                end_exclusive: bucket + step,
                buckets: 1,
            };
        }
        prev = bucket;
    }

    if current.buckets > best.buckets {
        best = current;
    }

    Some(best)
}

fn normalize_clob_token_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return U256::from_str_radix(hex, 16).ok().map(|u| u.to_string());
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Some(s.to_string());
    }
    U256::from_str_radix(s, 16).ok().map(|u| u.to_string())
}

fn bucket_floor_5m(ts: DateTime<Utc>) -> DateTime<Utc> {
    let secs = ts.timestamp();
    let floored = secs - secs.rem_euclid(PM5_BUCKET_SECS);
    DateTime::<Utc>::from_timestamp(floored, 0).unwrap_or(ts)
}

pub(super) async fn print_backtest_db_diagnostics(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    println!("\n=== Backtest DB diagnostics ===");
    println!("symbols: {}", symbols.join(", "));
    println!("from: {}", fmt_ts(from));
    println!("to:   {}", fmt_ts(to));

    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

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

pub(super) async fn verify_backtest_trades_gamma(
    pool: &sqlx::PgPool,
    run_id: uuid::Uuid,
) -> Result<()> {
    use super::super::settlement_ops::is_market_resolved;
    use crate::adapters::PolymarketClient;

    crate::persistence::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

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

    let slugs_vec: Vec<String> = slugs_needed.into_iter().collect();
    let existing_settlements = sqlx::query(
        "SELECT token_id, market_slug, outcome, resolved, settled_price
         FROM pm_token_settlements WHERE market_slug = ANY($1)",
    )
    .bind(&slugs_vec)
    .fetch_all(pool)
    .await?;

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

    let mut unresolved_tokens: Vec<String> = Vec::new();
    for settlements in slug_settlements.values() {
        for info in settlements.values() {
            if !info.resolved {
                unresolved_tokens.push(info.token_id.clone());
            }
        }
    }

    let mut missing_slugs: Vec<&str> = Vec::new();
    for slug in &slugs_vec {
        if !slug_settlements.contains_key(slug) {
            missing_slugs.push(slug);
        }
    }

    if !missing_slugs.is_empty() {
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
            let resolved_at: Option<DateTime<Utc>> = resolved.then(Utc::now);
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

    let mut verified = 0usize;
    let mut matched = 0usize;
    let mut mismatched = 0usize;

    for mapping in &mappings {
        let Some(outcomes) = slug_settlements.get(&mapping.market_slug) else {
            continue;
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::BTreeSet;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn longest_contiguous_bucket_run_prefers_longest_segment() {
        let buckets = [
            ts("2026-03-07T00:00:00Z"),
            ts("2026-03-07T00:05:00Z"),
            ts("2026-03-07T00:20:00Z"),
            ts("2026-03-07T00:25:00Z"),
            ts("2026-03-07T00:30:00Z"),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        let run = longest_contiguous_bucket_run(&buckets).expect("run");
        assert_eq!(run.start, ts("2026-03-07T00:20:00Z"));
        assert_eq!(run.end_exclusive, ts("2026-03-07T00:35:00Z"));
        assert_eq!(run.buckets, 3);
    }

    #[test]
    fn coverage_snapshot_intersects_all_critical_feeds() {
        let snapshot = Pm5mCoverageSnapshot {
            spot_buckets: [
                ts("2026-03-07T00:00:00Z"),
                ts("2026-03-07T00:05:00Z"),
                ts("2026-03-07T00:10:00Z"),
            ]
            .into_iter()
            .collect(),
            l2_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            quote_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            lob_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            event_buckets: [
                ts("2026-03-07T00:00:00Z"),
                ts("2026-03-07T00:05:00Z"),
                ts("2026-03-07T00:10:00Z"),
            ]
            .into_iter()
            .collect(),
        };

        let run = snapshot.longest_common_run().expect("common run");
        assert_eq!(run.start, ts("2026-03-07T00:05:00Z"));
        assert_eq!(run.end_exclusive, ts("2026-03-07T00:15:00Z"));
        assert_eq!(run.buckets, 2);
    }

    #[test]
    fn evaluate_pm5_requested_window_rejects_sparse_range_by_default() {
        let snapshot = Pm5mCoverageSnapshot {
            spot_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            l2_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            quote_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            lob_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            event_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
        };

        let error = evaluate_pm5_requested_window(
            &snapshot,
            Some(ts("2026-03-07T00:00:00Z")),
            Some(ts("2026-03-07T00:20:00Z")),
            false,
        )
        .expect_err("strict mode should reject sparse window");

        assert!(error
            .to_string()
            .contains("pm_5m_directional replay coverage is incomplete"));
    }

    #[test]
    fn evaluate_pm5_requested_window_auto_trims_to_overlap() {
        let snapshot = Pm5mCoverageSnapshot {
            spot_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            l2_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            quote_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            lob_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            event_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
        };

        let resolved = evaluate_pm5_requested_window(
            &snapshot,
            Some(ts("2026-03-07T00:00:00Z")),
            Some(ts("2026-03-07T00:20:00Z")),
            true,
        )
        .expect("auto-trim should recover overlap");

        assert_eq!(resolved.from, Some(ts("2026-03-07T00:05:00Z")));
        assert_eq!(resolved.to, Some(ts("2026-03-07T00:15:00Z")));
        assert!(resolved
            .auto_trim_message
            .as_deref()
            .unwrap_or_default()
            .contains("Auto-trimming pm_5m_directional replay window"));
    }

    #[test]
    fn evaluate_pm5_requested_window_keeps_valid_range_unchanged() {
        let snapshot = Pm5mCoverageSnapshot {
            spot_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            l2_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            quote_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            lob_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
            event_buckets: [ts("2026-03-07T00:05:00Z"), ts("2026-03-07T00:10:00Z")]
                .into_iter()
                .collect(),
        };

        let resolved = evaluate_pm5_requested_window(
            &snapshot,
            Some(ts("2026-03-07T00:05:00Z")),
            Some(ts("2026-03-07T00:14:59Z")),
            true,
        )
        .expect("valid window should pass unchanged");

        assert_eq!(resolved.from, Some(ts("2026-03-07T00:05:00Z")));
        assert_eq!(resolved.to, Some(ts("2026-03-07T00:14:59Z")));
        assert!(resolved.auto_trim_message.is_none());
    }

    #[test]
    fn bucket_floor_5m_rounds_down() {
        let floored = bucket_floor_5m(ts("2026-03-07T00:07:42Z"));
        assert_eq!(floored, Utc.with_ymd_and_hms(2026, 3, 7, 0, 5, 0).unwrap());
    }

    #[test]
    fn normalize_clob_token_id_accepts_hex_with_prefix() {
        assert_eq!(
            normalize_clob_token_id("0x0f"),
            Some(alloy::primitives::U256::from(15u8).to_string())
        );
    }
}
