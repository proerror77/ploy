//! Binance WebSocket data collectors — price ticks, aggregated trades, and L2 orderbook.
//!
//! Each collector is a long-running `async fn` that connects to Binance's combined
//! WebSocket stream, subscribes to the relevant `@trade` / `@aggTrade` / `@depth<N>@100ms`
//! channels, parses the JSON payloads, and persists normalized rows to PostgreSQL.
//!
//! All three share the same reconnection and batching strategy.
//!
//! Run via `ploy-runner`:
//!   ploy-runner collect-binance-lob --symbols BTCUSDT,ETHUSDT,SOLUSDT
//!   ploy-runner collect-binance-price --symbols BTCUSDT,ETHUSDT,SOLUSDT
//!   ploy-runner collect-binance-aggtrade --symbols BTCUSDT,ETHUSDT,SOLUSDT

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/stream";

fn parse_symbols(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a millisecond epoch from a Binance JSON field into a `DateTime<Utc>`.
fn ms_to_utc(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
        .unwrap()
}

/// Shared signal — set to false on shutdown request.
type SharedRunning = Arc<AtomicBool>;
type CollectorResult<T> = Result<T, String>;

fn running_flag() -> SharedRunning {
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received, stopping collector...");
        f.store(false, Ordering::SeqCst);
    });
    flag
}

/// Connect to Binance combined stream, subscribe, return the writer + reader.
async fn binance_connect(
    streams: &[String],
) -> Result<
    (
        futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    String,
> {
    let (ws, _) = connect_async(BINANCE_WS_URL)
        .await
        .map_err(|error| error.to_string())?;
    let (mut write, read) = ws.split();

    let subscribe = serde_json::json!({
        "method": "SUBSCRIBE",
        "params": streams,
        "id": 1,
    });
    write
        .send(Message::Text(subscribe.to_string().into()))
        .await
        .map_err(|error| error.to_string())?;

    Ok((write, read))
}

// ---------------------------------------------------------------------------
// Price collector (binance_price_ticks)
// ---------------------------------------------------------------------------

/// Collect spot trade prices from Binance `@trade` streams.
///
/// Schema: `binance_price_ticks (symbol, price, quantity, trade_time, received_at)`
pub async fn collect_binance_price(pool: PgPool, symbols_raw: &str, batch_size: usize) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@trade", s.to_lowercase()))
        .collect();

    info!(
        "[binance-price] Starting collector for symbols: {:?}",
        symbols
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_price_ws(&pool, &symbols, &streams, batch_size, &running).await {
            Ok(()) => {
                info!("[binance-price] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-price] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-price] Collector stopped");
}

async fn run_price_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-price] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => {
                        warn!("[binance-price] WebSocket stream ended");
                        break;
                    }
                    Some(Err(e)) => {
                        warn!("[binance-price] WebSocket error: {e}");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        // Skip subscription confirmation
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") {
                            Some(d) => d,
                            None => continue,
                        };

                        let symbol = data["s"].as_str().unwrap_or("").to_string();
                        let price_str = data["p"].as_str().unwrap_or("0");
                        let qty_str = data["q"].as_str().unwrap_or("0");
                        let trade_time_ms = data["T"].as_i64().unwrap_or(0);

                        let price: Decimal = price_str.parse().unwrap_or(Decimal::ZERO);
                        let qty: Decimal = qty_str.parse().unwrap_or(Decimal::ZERO);
                        let trade_time = ms_to_utc(trade_time_ms);

                        sqlx::query(
                            "INSERT INTO binance_price_ticks (symbol, price, quantity, trade_time) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"
                        )
                        .bind(&symbol)
                        .bind(price)
                        .bind(qty)
                        .bind(trade_time)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        pending += 1;
                        inserted += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-price] Inserted {} ticks in last minute", inserted);
                            inserted = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => {
                        warn!("[binance-price] WebSocket closed by server");
                        break;
                    }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AggTrade collector (binance_agg_trade_ticks)
// ---------------------------------------------------------------------------

/// Collect aggregated trade data from Binance `@aggTrade` streams.
///
/// Schema: `binance_agg_trade_ticks (symbol, agg_trade_id, first_trade_id, last_trade_id,
///          price, quantity, trade_time, event_time, is_buyer_maker, received_at)`
pub async fn collect_binance_aggtrade(pool: PgPool, symbols_raw: &str, batch_size: usize) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@aggTrade", s.to_lowercase()))
        .collect();

    info!(
        "[binance-aggtrade] Starting collector for symbols: {:?}",
        symbols
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_aggtrade_ws(&pool, &symbols, &streams, batch_size, &running).await {
            Ok(()) => {
                info!("[binance-aggtrade] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-aggtrade] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-aggtrade] Collector stopped");
}

async fn run_aggtrade_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-aggtrade] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut duplicates: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => { warn!("[binance-aggtrade] WebSocket stream ended"); break; }
                    Some(Err(e)) => { warn!("[binance-aggtrade] WebSocket error: {e}"); break; }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") { Some(d) => d, None => continue, };

                        let symbol = data["s"].as_str().unwrap_or("").to_string();
                        let agg_trade_id = data["a"].as_i64().unwrap_or(0);
                        let first_trade_id = data["f"].as_i64().unwrap_or(0);
                        let last_trade_id = data["l"].as_i64().unwrap_or(0);
                        let price: Decimal = data["p"].as_str().unwrap_or("0").parse().unwrap_or(Decimal::ZERO);
                        let qty: Decimal = data["q"].as_str().unwrap_or("0").parse().unwrap_or(Decimal::ZERO);
                        let trade_time = ms_to_utc(data["T"].as_i64().unwrap_or(0));
                        let event_time = ms_to_utc(data["E"].as_i64().unwrap_or(0));
                        let is_buyer_maker = data["m"].as_bool().unwrap_or(false);

                        let result = sqlx::query(
                            "INSERT INTO binance_agg_trade_ticks \
                             (symbol, agg_trade_id, first_trade_id, last_trade_id, price, quantity, trade_time, event_time, is_buyer_maker) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING"
                        )
                        .bind(&symbol)
                        .bind(agg_trade_id)
                        .bind(first_trade_id)
                        .bind(last_trade_id)
                        .bind(price)
                        .bind(qty)
                        .bind(trade_time)
                        .bind(event_time)
                        .bind(is_buyer_maker)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        if result.rows_affected() > 0 {
                            inserted += 1;
                        } else {
                            duplicates += 1;
                        }
                        pending += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-aggtrade] Inserted {} agg trades in last minute (duplicates={})", inserted, duplicates);
                            inserted = 0;
                            duplicates = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => { warn!("[binance-aggtrade] WS closed"); break; }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// LOB collector (binance_lob_ticks)
// ---------------------------------------------------------------------------

/// Collect orderbook depth snapshots from Binance `@depth<N>@100ms` streams.
///
/// Computes mid-price, spread (bps), OBI (order-book imbalance at depths 5/10),
/// and persists the full level-2 snapshot as JSONB in `bids` / `asks`.
///
/// Schema: `binance_lob_ticks (symbol, update_id, best_bid, best_ask, mid_price,
///          spread_bps, obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks,
///          event_time, source)`
pub async fn collect_binance_lob(
    pool: PgPool,
    symbols_raw: &str,
    depth_levels: usize,
    batch_size: usize,
) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@depth{}@100ms", s.to_lowercase(), depth_levels))
        .collect();

    info!(
        "[binance-lob] Starting collector symbols={:?} depth={}",
        symbols, depth_levels
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_lob_ws(
            &pool,
            &symbols,
            &streams,
            depth_levels,
            batch_size,
            &running,
        )
        .await
        {
            Ok(()) => {
                info!("[binance-lob] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-lob] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-lob] Collector stopped");
}

fn parse_level(raw_levels: &[Value]) -> Vec<(Decimal, Decimal)> {
    raw_levels
        .iter()
        .filter_map(|level| {
            let arr = level.as_array()?;
            if arr.len() < 2 {
                return None;
            }
            let price: Decimal = arr[0].as_str()?.parse().ok()?;
            let size: Decimal = arr[1].as_str()?.parse().ok()?;
            if price <= Decimal::ZERO || size <= Decimal::ZERO {
                return None;
            }
            Some((price, size))
        })
        .collect()
}

fn sum_volume(levels: &[(Decimal, Decimal)], depth: usize) -> Decimal {
    levels.iter().take(depth).map(|(_, s)| s).sum()
}

fn obi(bid_vol: Decimal, ask_vol: Decimal) -> Decimal {
    let denom = bid_vol + ask_vol;
    if denom == Decimal::ZERO {
        return Decimal::ZERO;
    }
    (bid_vol - ask_vol) / denom
}

fn levels_to_json(levels: &[(Decimal, Decimal)]) -> Value {
    let arr: Vec<Value> = levels
        .iter()
        .map(|(p, s)| serde_json::json!({"price": format!("{}", p), "size": format!("{}", s)}))
        .collect();
    Value::Array(arr)
}

async fn run_lob_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    depth_levels: usize,
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-lob] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => { warn!("[binance-lob] WebSocket stream ended"); break; }
                    Some(Err(e)) => { warn!("[binance-lob] WebSocket error: {e}"); break; }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") { Some(d) => d, None => continue, };

                        let stream_name = val.get("stream").and_then(|s| s.as_str());
                        let symbol = data["s"].as_str()
                            .or_else(|| stream_name.and_then(|s| s.split('@').next()))
                            .unwrap_or("")
                            .to_uppercase();

                        let received_at = Utc::now();
                        let event_time_ms = data["E"].as_i64();
                        let update_id = data["u"].as_i64().or_else(|| data["lastUpdateId"].as_i64());
                        let raw_bids = data["b"].as_array().or_else(|| data["bids"].as_array());
                        let raw_asks = data["a"].as_array().or_else(|| data["asks"].as_array());

                        let update_id = match update_id {
                            Some(u) => u,
                            _ => continue,
                        };

                        let bids = raw_bids.map(|levels| parse_level(levels)).unwrap_or_default();
                        let asks = raw_asks.map(|levels| parse_level(levels)).unwrap_or_default();

                        if bids.is_empty() || asks.is_empty() { continue; }

                        let best_bid = bids[0].0;
                        let best_ask = asks[0].0;
                        if best_bid <= Decimal::ZERO || best_ask <= Decimal::ZERO || best_ask < best_bid {
                            continue;
                        }

                        let mid_price = (best_bid + best_ask) / Decimal::from(2);
                        if mid_price <= Decimal::ZERO { continue; }

                        let spread = (best_ask - best_bid) / mid_price * Decimal::from(10_000);
                        let bid_vol_5 = sum_volume(&bids, 5);
                        let ask_vol_5 = sum_volume(&asks, 5);
                        let bid_vol_10 = sum_volume(&bids, 10);
                        let ask_vol_10 = sum_volume(&asks, 10);
                        let event_time = event_time_ms.map(ms_to_utc).unwrap_or(received_at);

                        let bids_json = levels_to_json(&bids[..bids.len().min(depth_levels)]);
                        let asks_json = levels_to_json(&asks[..asks.len().min(depth_levels)]);

                        sqlx::query(
                            "INSERT INTO binance_lob_ticks \
                             (symbol, update_id, best_bid, best_ask, mid_price, spread_bps, \
                              obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks, event_time, source) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'binance_depth_ws')"
                        )
                        .bind(&symbol)
                        .bind(update_id)
                        .bind(best_bid)
                        .bind(best_ask)
                        .bind(mid_price)
                        .bind(spread)
                        .bind(obi(bid_vol_5, ask_vol_5))
                        .bind(obi(bid_vol_10, ask_vol_10))
                        .bind(bid_vol_5)
                        .bind(ask_vol_5)
                        .bind(bids_json)
                        .bind(asks_json)
                        .bind(event_time)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        pending += 1;
                        inserted += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-lob] Inserted {} snapshots in last minute", inserted);
                            inserted = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => { warn!("[binance-lob] WS closed"); break; }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}
