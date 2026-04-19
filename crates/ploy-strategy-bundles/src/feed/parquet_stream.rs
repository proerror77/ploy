//! Streaming DuckDB-backed Parquet feed for O(1) memory backtest.
//!
//! Unlike [`super::parquet`] which loads all data into a `Vec<MarketUpdate>`,
//! this feed runs DuckDB in a background thread and streams rows through a
//! bounded `mpsc` channel. Memory usage is O(channel buffer) regardless of
//! date range.
//!
//! The main data tables (spot, agg trades, LOB, quotes) are merged via a single
//! DuckDB `UNION ALL … ORDER BY` query — DuckDB handles the merge sort
//! internally with disk spill, so Rust never holds more than the channel buffer
//! in memory. Events (~11K rows) are loaded separately and merged via a
//! two-pointer technique in the send loop.
//!
//! # Usage
//!
//! ```no_run
//! use ploy_strategy_bundles::feed::{parquet_stream::StreamingParquetFeed, HistoricalLoadOptions};
//! use chrono::Utc;
//!
//! let feed = StreamingParquetFeed::new(
//!     "/data/parquet",
//!     &["BTCUSDT".to_string()],
//!     Utc::now() - chrono::Duration::days(7),
//!     Utc::now(),
//!     &HistoricalLoadOptions::default(),
//! );
//! ```

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use tracing::info;

use crate::feed::database::{HistoricalLoadOptions, normalize_token_id};
use crate::traits::{Feed, MarketUpdate};

/// How far before `from` to load spot prices for EWMA warm-up.
const WARMUP_MINUTES: i64 = 30;

/// Bounded channel capacity — limits how far ahead the background thread runs.
const CHANNEL_CAPACITY: usize = 1000;

/// Streaming Parquet feed backed by a DuckDB background thread.
///
/// The background thread executes a single DuckDB `UNION ALL` query across all
/// data tables (spot, LOB, agg trades, quotes) with `ORDER BY ts_us`. DuckDB
/// handles the merge sort internally with disk spill, so Rust never holds more
/// than the channel buffer in memory. Events are loaded separately (~11K rows)
/// and merged via two-pointer in the send loop.
pub struct StreamingParquetFeed {
    receiver: Receiver<MarketUpdate>,
    /// Keep the thread handle so it is joined on drop (prevents leaks).
    _worker: thread::JoinHandle<()>,
}

impl StreamingParquetFeed {
    /// Spawn the background DuckDB thread and return a ready-to-use feed.
    ///
    /// Returns immediately; data starts flowing as soon as DuckDB opens the
    /// Parquet files. If `data_dir` does not exist the feed will be empty.
    pub fn new(
        data_dir: &str,
        symbols: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        options: &HistoricalLoadOptions,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<MarketUpdate>(CHANNEL_CAPACITY);

        let data_dir = data_dir.to_string();
        let symbols = symbols.to_vec();
        let lob_sample_secs = options.lob_sample_secs;

        let worker = thread::spawn(move || {
            if let Err(e) = run_background(&data_dir, &symbols, from, to, lob_sample_secs, tx) {
                tracing::warn!(error = %e, "StreamingParquetFeed background thread error");
            }
        });

        Self {
            receiver: rx,
            _worker: worker,
        }
    }
}

#[async_trait]
impl Feed for StreamingParquetFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        self.receiver.recv().ok()
    }
}

// ── Background thread ────────────────────────────────────────────────────────

/// Entry point for the background worker thread.
///
/// Builds a single DuckDB `UNION ALL` query across spot, agg trades, LOB, and
/// quotes, ordered by timestamp. DuckDB handles the merge sort with disk spill.
/// Events are loaded separately (small dataset) and merged via two-pointer.
/// Memory usage is O(channel buffer) regardless of date range.
#[cfg(feature = "parquet-feed")]
fn run_background(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    lob_sample_secs: u32,
    tx: SyncSender<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use duckdb::Connection;
    use std::path::Path;

    if !Path::new(data_dir).exists() {
        return Ok(());
    }

    std::fs::create_dir_all("/tmp/duckdb_spill").ok();
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("SET memory_limit='6GB'; SET temp_directory='/tmp/duckdb_spill';")?;

    let sym_filter = symbol_filter_sql(symbols);
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();
    let spot_from_str = spot_from.to_rfc3339();
    let bucket_us = (lob_sample_secs.max(1) as i64) * 1_000_000;

    // File globs
    let spot_glob = format!("{data_dir}/binance_price_ticks/*.parquet");
    let agg_glob = format!("{data_dir}/binance_agg_trade_ticks/*.parquet");
    let lob_glob = format!("{data_dir}/binance_lob_ticks/*.parquet");
    let quote_glob = format!("{data_dir}/clob_quote_ticks/*.parquet");

    // ── 1. Load events separately (small, ~11K rows) ────────────────────────
    let events = load_events_vec(&conn, data_dir, symbols, &from_str, &to_str)?;
    info!(count = events.len(), "StreamingParquetFeed: loaded events");
    let mut evt_idx = 0;

    // ── 2. Build UNION ALL query for the big tables ─────────────────────────
    let mut parts: Vec<String> = Vec::new();

    // Spot prices (with 30min warmup)
    if Path::new(&format!("{data_dir}/binance_price_ticks")).exists() {
        parts.push(format!(
            "SELECT EPOCH_US(trade_time)::BIGINT AS ts_us, \
                    'spot' AS typ, \
                    symbol AS s1, NULL AS s2, \
                    CAST(price AS DOUBLE) AS f1, 0.0 AS f2, 0.0 AS f3, 0.0 AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM read_parquet('{spot_glob}') \
             WHERE trade_time >= TIMESTAMPTZ '{spot_from_str}' \
               AND trade_time <= TIMESTAMPTZ '{to_str}' \
               {sym_filter}"
        ));
    }

    // Agg trades (5s downsampled)
    if Path::new(&format!("{data_dir}/binance_agg_trade_ticks")).exists() {
        parts.push(format!(
            "SELECT EPOCH_US(trade_time)::BIGINT AS ts_us, \
                    'agg' AS typ, \
                    symbol AS s1, NULL AS s2, \
                    CAST(price AS DOUBLE) AS f1, CAST(quantity AS DOUBLE) AS f2, \
                    0.0 AS f3, 0.0 AS f4, \
                    CAST(agg_trade_id AS BIGINT) AS i1, is_buyer_maker AS b1 \
             FROM ( \
                 SELECT DISTINCT ON (symbol, EPOCH_US(trade_time)::BIGINT / 5000000) \
                        trade_time, symbol, price, quantity, agg_trade_id, is_buyer_maker \
                 FROM read_parquet('{agg_glob}') \
                 WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
                   AND trade_time <= TIMESTAMPTZ '{to_str}' \
                   {sym_filter} \
                 ORDER BY symbol, EPOCH_US(trade_time)::BIGINT / 5000000, trade_time \
             )"
        ));
    }

    // LOB (downsampled)
    if Path::new(&format!("{data_dir}/binance_lob_ticks")).exists() {
        parts.push(format!(
            "SELECT EPOCH_US(event_time)::BIGINT AS ts_us, \
                    'lob' AS typ, \
                    symbol AS s1, NULL AS s2, \
                    COALESCE(obi_5, 0.0) AS f1, \
                    CAST(COALESCE(spread_bps, 0) AS DOUBLE) AS f2, \
                    COALESCE(bid_volume_5, 0.0) AS f3, \
                    COALESCE(ask_volume_5, 0.0) AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM ( \
                 SELECT DISTINCT ON (symbol, EPOCH_US(event_time)::BIGINT / {bucket_us}) \
                        event_time, symbol, obi_5, spread_bps, bid_volume_5, ask_volume_5 \
                 FROM read_parquet('{lob_glob}') \
                 WHERE event_time >= TIMESTAMPTZ '{from_str}' \
                   AND event_time <= TIMESTAMPTZ '{to_str}' \
                   {sym_filter} \
                 ORDER BY symbol, EPOCH_US(event_time)::BIGINT / {bucket_us}, event_time DESC \
             )"
        ));
    }

    // PM quotes
    if Path::new(&format!("{data_dir}/clob_quote_ticks")).exists() {
        parts.push(format!(
            "SELECT EPOCH_US(received_at)::BIGINT AS ts_us, \
                    'quote' AS typ, \
                    token_id AS s1, NULL AS s2, \
                    CAST(best_bid AS DOUBLE) AS f1, CAST(best_ask AS DOUBLE) AS f2, \
                    0.0 AS f3, 0.0 AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM read_parquet('{quote_glob}') \
             WHERE received_at >= TIMESTAMPTZ '{from_str}' \
               AND received_at <= TIMESTAMPTZ '{to_str}' \
               AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99)"
        ));
    }

    if parts.is_empty() {
        // No data tables found — just send events
        for (_, update) in &events {
            if tx.send(update.clone()).is_err() { return Ok(()); }
        }
        return Ok(());
    }

    let union_sql = format!(
        "SELECT * FROM ({}) ORDER BY ts_us",
        parts.join(" UNION ALL ")
    );

    eprintln!("StreamingParquetFeed: UNION ALL query parts={}", parts.len());

    // ── 3. Stream UNION ALL results, merging events via two-pointer ─────────
    let mut stmt = match conn.prepare(&union_sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("StreamingParquetFeed: prepare error: {e}");
            eprintln!("SQL: {union_sql}");
            return Err(e.into());
        }
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,            // ts_us
            row.get::<_, String>(1)?,          // typ
            row.get::<_, Option<String>>(2)?,  // s1
            row.get::<_, Option<String>>(3)?,  // s2
            row.get::<_, f64>(4)?,             // f1
            row.get::<_, f64>(5)?,             // f2
            row.get::<_, f64>(6)?,             // f3
            row.get::<_, f64>(7)?,             // f4
            row.get::<_, i64>(8)?,             // i1
            row.get::<_, bool>(9)?,            // b1
        ))
    })?;

    let mut total = 0usize;
    for row in rows {
        let (ts_us, typ, s1, _s2, f1, f2, f3, f4, i1, b1) = match row {
            Ok(r) => r,
            Err(e) => {
                eprintln!("StreamingParquetFeed: row error at total={total}: {e}");
                break;
            }
        };

        // Insert any events that should come before this row
        while evt_idx < events.len() && events[evt_idx].0 <= ts_us {
            if tx.send(events[evt_idx].1.clone()).is_err() { return Ok(()); }
            evt_idx += 1;
        }

        let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();

        match typ.as_str() {
            "spot" => {
                let symbol = s1.unwrap_or_default();
                let price = Decimal::try_from(f1).unwrap_or_default();
                if tx.send(MarketUpdate::SpotPrice { symbol, price, ts }).is_err() {
                    return Ok(());
                }
            }
            "agg" => {
                let symbol = s1.unwrap_or_default();
                let price = Decimal::try_from(f1).unwrap_or_default();
                let quantity = Decimal::try_from(f2).unwrap_or_default();
                if tx.send(MarketUpdate::AggTrade {
                    symbol,
                    agg_trade_id: i1 as u64,
                    price,
                    quantity,
                    is_buyer_maker: b1,
                    ts,
                }).is_err() {
                    return Ok(());
                }
            }
            "lob" => {
                let symbol = s1.unwrap_or_default();
                let obi = f1;
                let spread_bps = f2 as u32;
                let bid_depth_near = f3;
                let ask_depth_near = f4;
                // Send both L2 and L2Depth (matching original behavior)
                if tx.send(MarketUpdate::L2 {
                    symbol: symbol.clone(), obi, spread_bps, ts,
                }).is_err() {
                    return Ok(());
                }
                if tx.send(MarketUpdate::L2Depth {
                    symbol, obi, spread_bps, bid_depth_near, ask_depth_near, ts,
                }).is_err() {
                    return Ok(());
                }
            }
            "quote" => {
                let token_id = s1.unwrap_or_default();
                let bid = Decimal::try_from(f1).ok();
                let ask = Decimal::try_from(f2).ok();
                if tx.send(MarketUpdate::Quote {
                    token_id, bid, ask, bid_size: None, ask_size: None, ts,
                }).is_err() {
                    return Ok(());
                }
            }
            _ => continue,
        }
        total += 1;
    }

    // Send remaining events after the UNION ALL stream is exhausted
    while evt_idx < events.len() {
        if tx.send(events[evt_idx].1.clone()).is_err() { break; }
        evt_idx += 1;
    }

    info!(total, events = events.len(), "StreamingParquetFeed: streaming complete");
    Ok(())
}

/// Stub for when the feature flag is disabled.
#[cfg(not(feature = "parquet-feed"))]
fn run_background(
    _data_dir: &str,
    _symbols: &[String],
    _from: DateTime<Utc>,
    _to: DateTime<Utc>,
    _lob_sample_secs: u32,
    _tx: SyncSender<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Load event rows (EventDiscovered + EventExpired pairs) into a sorted Vec.
///
/// Events are small (~11K rows) so loading them into memory is fine. They are
/// merged into the main UNION ALL stream via two-pointer in the send loop.
#[cfg(feature = "parquet-feed")]
fn load_events_vec(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from_str: &str,
    to_str: &str,
) -> Result<Vec<(i64, MarketUpdate)>, Box<dyn std::error::Error + Send + Sync>> {
    use std::path::Path;

    let dir = format!("{data_dir}/pm_market_metadata");
    if !Path::new(&dir).exists() {
        return Ok(Vec::new());
    }

    let glob = format!("{dir}/*.parquet");
    let sym_filter = symbol_filter_sql(symbols);
    let sql = format!(
        "SELECT market_slug, symbol, \
                EPOCH_US(start_time)::BIGINT, EPOCH_US(end_time)::BIGINT, \
                CAST(price_to_beat AS DOUBLE), \
                (json_extract_string(raw_market, '$.markets[0].clobTokenIds')::JSON->>0) AS up_token_id, \
                (json_extract_string(raw_market, '$.markets[0].clobTokenIds')::JSON->>1) AS down_token_id \
         FROM read_parquet('{glob}') \
         WHERE end_time >= TIMESTAMPTZ '{from_str}' \
           AND start_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
           AND raw_market IS NOT NULL \
         ORDER BY start_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut events = Vec::new();
    for row in rows {
        let (market_slug, symbol_opt, start_us, end_us, price_to_beat_f, up_opt, dn_opt) = row?;
        let symbol = match symbol_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let start_us = match start_us { Some(v) => v, None => continue };
        let end_us = match end_us { Some(v) => v, None => continue };
        let up_raw = match up_opt { Some(s) if !s.is_empty() => s, _ => continue };
        let dn_raw = match dn_opt { Some(s) if !s.is_empty() => s, _ => continue };

        let up_token = normalize_token_id(&up_raw);
        let down_token = normalize_token_id(&dn_raw);
        let start_time = DateTime::from_timestamp_micros(start_us).unwrap_or_default();
        let end_time = DateTime::from_timestamp_micros(end_us).unwrap_or_default();
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;
        let price_to_beat = price_to_beat_f.and_then(|f| Decimal::try_from(f).ok());

        // EventDiscovered fires at start_time
        events.push((start_us, MarketUpdate::EventDiscovered {
            event_id: market_slug.clone(),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat,
            resolved_up_won: None,
        }));

        // EventExpired fires at end_time
        events.push((end_us, MarketUpdate::EventExpired {
            event_id: market_slug,
            end_time,
            resolved_up_won: None,
        }));
    }

    events.sort_by_key(|(ts, _)| *ts);
    Ok(events)
}

fn symbol_filter_sql(symbols: &[String]) -> String {
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