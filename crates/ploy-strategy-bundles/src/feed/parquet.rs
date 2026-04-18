//! DuckDB-backed Parquet feed loader.
//!
//! Reads date-partitioned Parquet files from a local data directory and
//! returns a sorted `Vec<MarketUpdate>` for use with [`HistoricalFeed`].
//!
//! # Directory layout expected
//!
//! ```text
//! <data_dir>/
//!   binance_price_ticks/YYYY-MM-DD.parquet
//!   binance_lob_ticks/YYYY-MM-DD.parquet
//!   binance_agg_trade_ticks/YYYY-MM-DD.parquet
//!   clob_quote_ticks/YYYY-MM-DD.parquet
//!   pm_market_metadata/YYYY-MM-DD.parquet
//! ```

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::path::Path;
use std::str::FromStr;
use tracing::info;

use crate::feed::database::HistoricalLoadOptions;
use crate::traits::MarketUpdate;

/// How far before `from` to load spot prices for EWMA warm-up.
const WARMUP_MINUTES: i64 = 30;

fn update_ts(u: &MarketUpdate) -> DateTime<Utc> {
    match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::AggTrade { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::L2Depth { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered {
            end_time,
            window_secs,
            ..
        } => *end_time - Duration::seconds(*window_secs as i64) - Duration::hours(1),
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

/// Load historical market updates from date-partitioned Parquet files.
///
/// Returns an empty `Vec` if `data_dir` does not exist.
/// Requires the `parquet-feed` feature to be enabled for actual data loading;
/// without it the function always returns an empty `Vec`.
pub fn load_from_parquet(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, Box<dyn std::error::Error>> {
    if !Path::new(data_dir).exists() {
        return Ok(Vec::new());
    }

    #[cfg(not(feature = "parquet-feed"))]
    {
        let _ = (symbols, from, to, options);
        return Ok(Vec::new());
    }

    #[cfg(feature = "parquet-feed")]
    {
        load_with_duckdb(data_dir, symbols, from, to, options)
    }
}

#[cfg(feature = "parquet-feed")]
fn load_with_duckdb(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, Box<dyn std::error::Error>> {
    use duckdb::Connection;

    let conn = Connection::open_in_memory()?;
    let mut updates: Vec<MarketUpdate> = Vec::new();

    let spot_from = from - Duration::minutes(WARMUP_MINUTES);

    load_spot_prices(&conn, data_dir, symbols, spot_from, to, &mut updates)?;
    load_agg_trades(&conn, data_dir, symbols, from, to, &mut updates)?;
    load_events(&conn, data_dir, symbols, from, to, &mut updates)?;
    load_pm_quotes(&conn, data_dir, from, to, &mut updates)?;
    load_l2_data(&conn, data_dir, symbols, from, to, options.lob_sample_secs, &mut updates)?;

    updates.sort_by_key(|u| update_ts(u));

    info!(
        count = updates.len(),
        symbols = ?symbols,
        from = %from,
        to = %to,
        "Loaded historical data from Parquet files",
    );

    Ok(updates)
}

#[cfg(feature = "parquet-feed")]
fn glob_parquet_files(dir: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    // Build a glob pattern covering all dates in [from, to].
    // DuckDB accepts a glob like: 'dir/YYYY-MM-DD.parquet'
    // For simplicity, use a wildcard and let DuckDB filter by timestamp column.
    format!("{dir}/*.parquet")
}

#[cfg(feature = "parquet-feed")]
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

#[cfg(feature = "parquet-feed")]
fn load_spot_prices(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_price_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT trade_time, symbol, price \
         FROM read_parquet('{glob}') \
         WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
           AND trade_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY trade_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_str, symbol, price_str) = row?;
        let ts = parse_ts(&ts_str)?;
        let price = Decimal::from_str(&price_str)?;
        updates.push(MarketUpdate::SpotPrice { symbol, price, ts });
        count += 1;
    }
    info!(count, "Loaded spot prices from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_agg_trades(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_agg_trade_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    // 5-second downsampling: keep first row per (symbol, 5s bucket)
    let sql = format!(
        "SELECT DISTINCT ON (symbol, epoch_ms(trade_time)::BIGINT / 5000) \
             trade_time, symbol, agg_trade_id, price, quantity, is_buyer_maker \
         FROM read_parquet('{glob}') \
         WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
           AND trade_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY symbol, epoch_ms(trade_time)::BIGINT / 5000, trade_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_str, symbol, agg_trade_id, price_str, qty_str, is_buyer_maker) = row?;
        let ts = parse_ts(&ts_str)?;
        let price = Decimal::from_str(&price_str)?;
        let quantity = Decimal::from_str(&qty_str)?;
        updates.push(MarketUpdate::AggTrade {
            symbol,
            agg_trade_id: agg_trade_id as u64,
            price,
            quantity,
            is_buyer_maker,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded agg trades from Parquet (5s downsampled)");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_l2_data(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_lob_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();
    let bucket_ms = (sample_secs.max(1) as i64) * 1000;

    let sql = format!(
        "SELECT DISTINCT ON (symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}) \
             event_time, symbol, \
             COALESCE(obi_5, 0.0) AS obi, \
             COALESCE(spread_bps, 0) AS spread_bps, \
             COALESCE(bid_volume_5, 0.0) AS bid_depth_near, \
             COALESCE(ask_volume_5, 0.0) AS ask_depth_near \
         FROM read_parquet('{glob}') \
         WHERE event_time >= TIMESTAMPTZ '{from_str}' \
           AND event_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}, event_time DESC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, f64>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_str, symbol, obi, spread_bps, bid_depth_near, ask_depth_near) = row?;
        let ts = parse_ts(&ts_str)?;
        let spread_bps = spread_bps as u32;
        updates.push(MarketUpdate::L2 {
            symbol: symbol.clone(),
            obi,
            spread_bps,
            ts,
        });
        updates.push(MarketUpdate::L2Depth {
            symbol,
            obi,
            spread_bps,
            bid_depth_near,
            ask_depth_near,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded L2 rows from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_pm_quotes(
    conn: &duckdb::Connection,
    data_dir: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/clob_quote_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT received_at, token_id, best_bid, best_ask, bid_size, ask_size \
         FROM read_parquet('{glob}') \
         WHERE received_at >= TIMESTAMPTZ '{from_str}' \
           AND received_at <= TIMESTAMPTZ '{to_str}' \
           AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99) \
         ORDER BY received_at"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_str, token_id, bid_str, ask_str, bid_size_str, ask_size_str) = row?;
        let ts = parse_ts(&ts_str)?;
        let bid = bid_str.as_deref().and_then(|s| Decimal::from_str(s).ok());
        let ask = ask_str.as_deref().and_then(|s| Decimal::from_str(s).ok());
        let bid_size = bid_size_str.as_deref().and_then(|s| Decimal::from_str(s).ok());
        let ask_size = ask_size_str.as_deref().and_then(|s| Decimal::from_str(s).ok());
        updates.push(MarketUpdate::Quote {
            token_id,
            bid,
            ask,
            bid_size,
            ask_size,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded PM quotes from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_events(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/pm_market_metadata");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT market_slug, symbol, start_time, end_time, \
                up_token_id, down_token_id, price_to_beat, resolved_up_won \
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
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<bool>>(7)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (
            market_slug,
            symbol_opt,
            start_str,
            end_str,
            up_token_opt,
            down_token_opt,
            price_to_beat_str,
            resolved_up_won,
        ) = row?;

        let symbol = match symbol_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let end_time = match end_str.as_deref().map(parse_ts) {
            Some(Ok(t)) => t,
            _ => continue,
        };
        let start_time = match start_str.as_deref().map(parse_ts) {
            Some(Ok(t)) => t,
            _ => continue,
        };
        let up_token = match up_token_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let down_token = match down_token_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let price_to_beat = price_to_beat_str
            .as_deref()
            .and_then(|s| Decimal::from_str(s).ok());
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        updates.push(MarketUpdate::EventDiscovered {
            event_id: market_slug.clone(),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat,
            resolved_up_won: None,
        });
        updates.push(MarketUpdate::EventExpired {
            event_id: market_slug,
            end_time,
            resolved_up_won,
        });
        count += 1;
    }
    info!(count, "Loaded events from Parquet pm_market_metadata");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn parse_ts(s: &str) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
    // DuckDB returns timestamps in various formats; try RFC3339 first, then a
    // space-separated format like "2024-01-15 12:34:56.789 UTC".
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try "YYYY-MM-DD HH:MM:SS[.fff] UTC" or similar
    for fmt in &[
        "%Y-%m-%d %H:%M:%S%.f %Z",
        "%Y-%m-%d %H:%M:%S %Z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M:%S%z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    Err(format!("cannot parse timestamp: {s}").into())
}
