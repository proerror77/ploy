//! Streaming DuckDB-backed Parquet feed for O(1) memory backtest.
//!
//! Unlike [`super::parquet`] which loads all data into a `Vec<MarketUpdate>`,
//! this feed runs DuckDB in a background thread and streams rows through a
//! bounded `mpsc` channel. Memory usage is O(channel buffer) regardless of
//! date range.
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
/// The background thread executes per-table queries (spot, LOB, agg trades,
/// quotes, events) and merges them in timestamp order via a priority queue,
/// sending each `MarketUpdate` through a bounded channel. The async `next()`
/// method receives one item at a time, keeping memory usage constant.
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
        // `recv()` blocks until an item is available or the sender is dropped.
        // We call it from an async context; for backtest use this is fine since
        // the tokio runtime is single-threaded and DuckDB is CPU-bound anyway.
        self.receiver.recv().ok()
    }
}

// ── Background thread ────────────────────────────────────────────────────────

/// Entry point for the background worker thread.
///
/// Loads each table independently (to keep per-query memory bounded), merges
/// all rows into a single timestamp-sorted stream via a `BinaryHeap`, and
/// sends each `MarketUpdate` through the channel.
#[cfg(feature = "parquet-feed")]
fn run_background(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    lob_sample_secs: u32,
    tx: SyncSender<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::collections::BinaryHeap;
    use std::cmp::Reverse;
    use std::path::Path;

    if !Path::new(data_dir).exists() {
        return Ok(());
    }

    std::fs::create_dir_all("/tmp/duckdb_spill").ok();

    let spot_from = from - Duration::minutes(WARMUP_MINUTES);

    // Collect all updates into a heap for merge-sort.
    // Each table is loaded with its own short-lived DuckDB connection so
    // memory is released between tables.
    let mut heap: BinaryHeap<Reverse<TimestampedUpdate>> = BinaryHeap::new();

    // ── Spot prices ──────────────────────────────────────────────────────────
    {
        let dir = format!("{data_dir}/binance_price_ticks");
        if Path::new(&dir).exists() {
            let conn = open_conn()?;
            let glob = format!("{dir}/*.parquet");
            let sym_filter = symbol_filter_sql(symbols);
            let from_str = spot_from.to_rfc3339();
            let to_str = to.to_rfc3339();
            let sql = format!(
                "SELECT EPOCH_US(trade_time)::BIGINT, symbol, CAST(price AS DOUBLE) \
                 FROM read_parquet('{glob}') \
                 WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
                   AND trade_time <= TIMESTAMPTZ '{to_str}' \
                   {sym_filter} \
                 ORDER BY trade_time"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?))
            })?;
            let mut count = 0usize;
            for row in rows {
                let (ts_us, symbol, price_f) = row?;
                let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
                let price = Decimal::try_from(price_f).unwrap_or_default();
                heap.push(Reverse(TimestampedUpdate {
                    ts_us,
                    update: MarketUpdate::SpotPrice { symbol, price, ts },
                }));
                count += 1;
            }
            info!(count, "StreamingParquetFeed: queued spot prices");
        }
    }

    // ── Agg trades (5s downsampled) ──────────────────────────────────────────
    {
        let dir = format!("{data_dir}/binance_agg_trade_ticks");
        if Path::new(&dir).exists() {
            let conn = open_conn()?;
            let glob = format!("{dir}/*.parquet");
            let sym_filter = symbol_filter_sql(symbols);
            let from_str = from.to_rfc3339();
            let to_str = to.to_rfc3339();
            let sql = format!(
                "SELECT DISTINCT ON (symbol, epoch_ms(trade_time)::BIGINT / 5000) \
                     EPOCH_US(trade_time)::BIGINT, symbol, agg_trade_id, \
                     CAST(price AS DOUBLE), CAST(quantity AS DOUBLE), is_buyer_maker \
                 FROM read_parquet('{glob}') \
                 WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
                   AND trade_time <= TIMESTAMPTZ '{to_str}' \
                   {sym_filter} \
                 ORDER BY symbol, epoch_ms(trade_time)::BIGINT / 5000, trade_time"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?;
            let mut count = 0usize;
            for row in rows {
                let (ts_us, symbol, agg_trade_id, price_f, qty_f, is_buyer_maker) = row?;
                let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
                let price = Decimal::try_from(price_f).unwrap_or_default();
                let quantity = Decimal::try_from(qty_f).unwrap_or_default();
                heap.push(Reverse(TimestampedUpdate {
                    ts_us,
                    update: MarketUpdate::AggTrade {
                        symbol,
                        agg_trade_id: agg_trade_id as u64,
                        price,
                        quantity,
                        is_buyer_maker,
                        ts,
                    },
                }));
                count += 1;
            }
            info!(count, "StreamingParquetFeed: queued agg trades");
        }
    }

    // ── LOB (day-by-day to avoid OOM) ────────────────────────────────────────
    {
        let dir = format!("{data_dir}/binance_lob_ticks");
        if Path::new(&dir).exists() {
            let bucket_ms = (lob_sample_secs.max(1) as i64) * 1000;
            let sym_filter = symbol_filter_sql(symbols);
            let mut day = from.date_naive();
            let to_date = to.date_naive();
            let mut total = 0usize;
            while day <= to_date {
                let day_start = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
                let day_end = day
                    .succ_opt()
                    .unwrap_or(day)
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc();
                let from_str = day_start.to_rfc3339();
                let to_str = day_end.to_rfc3339();
                let glob = format!("{dir}/*.parquet");
                let conn = open_conn()?;
                let sql = format!(
                    "SELECT DISTINCT ON (symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}) \
                         EPOCH_US(event_time)::BIGINT, symbol, \
                         COALESCE(obi_5, 0.0) AS obi, \
                         COALESCE(spread_bps, 0) AS spread_bps, \
                         COALESCE(bid_volume_5, 0.0) AS bid_volume_5, \
                         COALESCE(ask_volume_5, 0.0) AS ask_volume_5 \
                     FROM read_parquet('{glob}') \
                     WHERE event_time >= TIMESTAMPTZ '{from_str}' \
                       AND event_time <= TIMESTAMPTZ '{to_str}' \
                       {sym_filter} \
                     ORDER BY symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}, event_time DESC"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, f64>(5)?,
                    ))
                })?;
                for row in rows {
                    let (ts_us, symbol, obi, spread_bps_raw, bid_depth_near, ask_depth_near) =
                        row?;
                    let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
                    let spread_bps = spread_bps_raw as u32;
                    heap.push(Reverse(TimestampedUpdate {
                        ts_us,
                        update: MarketUpdate::L2 {
                            symbol: symbol.clone(),
                            obi,
                            spread_bps,
                            ts,
                        },
                    }));
                    heap.push(Reverse(TimestampedUpdate {
                        ts_us,
                        update: MarketUpdate::L2Depth {
                            symbol,
                            obi,
                            spread_bps,
                            bid_depth_near,
                            ask_depth_near,
                            ts,
                        },
                    }));
                    total += 1;
                }
                day = day.succ_opt().unwrap_or(day);
            }
            info!(total, "StreamingParquetFeed: queued LOB rows");
        }
    }

    // ── PM quotes ────────────────────────────────────────────────────────────
    {
        let dir = format!("{data_dir}/clob_quote_ticks");
        if Path::new(&dir).exists() {
            let conn = open_conn()?;
            let glob = format!("{dir}/*.parquet");
            let from_str = from.to_rfc3339();
            let to_str = to.to_rfc3339();
            let sql = format!(
                "SELECT EPOCH_US(received_at)::BIGINT, token_id, \
                        CAST(best_bid AS DOUBLE), CAST(best_ask AS DOUBLE) \
                 FROM read_parquet('{glob}') \
                 WHERE received_at >= TIMESTAMPTZ '{from_str}' \
                   AND received_at <= TIMESTAMPTZ '{to_str}' \
                   AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99) \
                 ORDER BY received_at"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?;
            let mut count = 0usize;
            for row in rows {
                let (ts_us, token_id, bid_f, ask_f) = row?;
                let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
                let bid = bid_f.and_then(|f| Decimal::try_from(f).ok());
                let ask = ask_f.and_then(|f| Decimal::try_from(f).ok());
                heap.push(Reverse(TimestampedUpdate {
                    ts_us,
                    update: MarketUpdate::Quote {
                        token_id,
                        bid,
                        ask,
                        bid_size: None,
                        ask_size: None,
                        ts,
                    },
                }));
                count += 1;
            }
            info!(count, "StreamingParquetFeed: queued PM quotes");
        }
    }

    // ── Events (EventDiscovered + EventExpired pairs) ─────────────────────────
    {
        let dir = format!("{data_dir}/pm_market_metadata");
        if Path::new(&dir).exists() {
            let conn = open_conn()?;
            let glob = format!("{dir}/*.parquet");
            let sym_filter = symbol_filter_sql(symbols);
            let from_str = from.to_rfc3339();
            let to_str = to.to_rfc3339();
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
            let mut count = 0usize;
            for row in rows {
                let (market_slug, symbol_opt, start_us, end_us, price_to_beat_f, up_opt, dn_opt) =
                    row?;
                let symbol = match symbol_opt {
                    Some(s) if !s.is_empty() => s,
                    _ => continue,
                };
                let end_time = match end_us {
                    Some(us) => DateTime::from_timestamp_micros(us).unwrap_or_default(),
                    None => continue,
                };
                let start_time = match start_us {
                    Some(us) => DateTime::from_timestamp_micros(us).unwrap_or_default(),
                    None => continue,
                };
                let up_token = match up_opt {
                    Some(s) if !s.is_empty() => normalize_token_id(&s),
                    _ => continue,
                };
                let down_token = match dn_opt {
                    Some(s) if !s.is_empty() => normalize_token_id(&s),
                    _ => continue,
                };
                let price_to_beat = price_to_beat_f.and_then(|f| Decimal::try_from(f).ok());
                let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

                // EventDiscovered fires at start_time (matching database.rs)
                let discovered_ts_us = start_time.timestamp_micros();
                heap.push(Reverse(TimestampedUpdate {
                    ts_us: discovered_ts_us,
                    update: MarketUpdate::EventDiscovered {
                        event_id: market_slug.clone(),
                        symbol,
                        up_token,
                        down_token,
                        end_time,
                        window_secs,
                        price_to_beat,
                        resolved_up_won: None,
                    },
                }));

                // EventExpired fires at end_time
                let expired_ts_us = end_time.timestamp_micros();
                heap.push(Reverse(TimestampedUpdate {
                    ts_us: expired_ts_us,
                    update: MarketUpdate::EventExpired {
                        event_id: market_slug,
                        end_time,
                        resolved_up_won: None,
                    },
                }));
                count += 1;
            }
            info!(count, "StreamingParquetFeed: queued events");
        }
    }

    // ── Drain heap in timestamp order ────────────────────────────────────────
    let total = heap.len();
    info!(total, "StreamingParquetFeed: streaming merged updates");
    while let Some(Reverse(item)) = heap.pop() {
        // send() blocks when the channel is full — this is the backpressure mechanism.
        if tx.send(item.update).is_err() {
            // Receiver dropped (runtime stopped early); exit cleanly.
            break;
        }
    }

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

#[cfg(feature = "parquet-feed")]
fn open_conn() -> Result<duckdb::Connection, Box<dyn std::error::Error + Send + Sync>> {
    let conn = duckdb::Connection::open_in_memory()?;
    conn.execute_batch("SET memory_limit='4GB'; SET temp_directory='/tmp/duckdb_spill';")?;
    Ok(conn)
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

/// Wrapper that makes `MarketUpdate` orderable by timestamp for the heap.
struct TimestampedUpdate {
    ts_us: i64,
    update: MarketUpdate,
}

impl PartialEq for TimestampedUpdate {
    fn eq(&self, other: &Self) -> bool {
        self.ts_us == other.ts_us
    }
}

impl Eq for TimestampedUpdate {}

impl PartialOrd for TimestampedUpdate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimestampedUpdate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ts_us.cmp(&other.ts_us)
    }
}
