//! Live market data feed producers.
//!
//! Two async tasks that bridge vendor SDK WebSocket streams into the
//! unified `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Timelike, Utc};
use futures::StreamExt;
use ploy_market_contracts::MarketUpdate;
use polymarket_client_sdk::rtds::{Client as RtdsClient, EquityPriceMessage};
use polymarket_client_sdk::types::U256;
use polymarket_client_sdk::ws::config::{Config as PolymarketWsConfig, ReconnectConfig};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, error, info, warn};

use crate::reference_prices::{
    ReferenceAssetClass, ReferencePriceKey, ReferencePriceRegistry, ReferencePriceSnapshot,
    ReferencePriceSource, infer_pyth_asset_class, market_symbol_to_binance_symbol,
    normalize_reference_symbol, pyth_symbol, upsert_reference_price,
};

const POLYMARKET_RTDS_WS_ENDPOINT: &str = "wss://ws-live-data.polymarket.com";
const NEAR_DEPTH_PCT_RANGE: f64 = 0.001;

fn rtds_market_data_ws_config() -> PolymarketWsConfig {
    let mut config = PolymarketWsConfig::default();
    // These feeds only need resilient market-data delivery. A wider heartbeat
    // window avoids unnecessary reconnect churn on transient stalls.
    config.heartbeat_interval = StdDuration::from_secs(15);
    config.heartbeat_timeout = StdDuration::from_secs(45);
    config.reconnect = ReconnectConfig::default();
    config
}

/// Spawn a task that subscribes to Binance spot prices via RTDS WebSocket
/// and publishes `MarketUpdate::SpotPrice` events in real-time.
///
/// When `pool` is provided, each tick is also persisted to `binance_price_ticks`
/// (at full tick resolution) so that historical backtests can replay
/// the same spot-price stream.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_spot_symbols = HashSet::new();
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        // Track last-persisted second per symbol to deduplicate high-frequency ticks.
        let mut last_persisted: HashMap<String, DateTime<Utc>> = HashMap::new();

        info!(
            symbols = ?symbols_upper,
            "Starting RTDS WebSocket spot price feed"
        );

        let client = RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
            .expect("RTDS market-data config should be valid");

        // Subscribe to crypto prices (Binance feed)
        let stream = match client.subscribe_crypto_prices(Some(symbols_upper.clone())) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "Failed to subscribe to crypto_prices");
                return;
            }
        };

        let mut stream = Box::pin(stream);
        let mut price_count = 0_u64;

        while let Some(result) = stream.next().await {
            match result {
                Ok(crypto_price) => {
                    // Convert Unix millis to DateTime<Utc>
                    let ts = DateTime::from_timestamp_millis(crypto_price.timestamp)
                        .unwrap_or_else(Utc::now);

                    let symbol_upper = crypto_price.symbol.to_uppercase();

                    upsert_reference_price(
                        &reference_prices,
                        ReferencePriceSnapshot {
                            key: ReferencePriceKey {
                                source: ReferencePriceSource::Binance,
                                symbol: market_symbol_to_binance_symbol(&crypto_price.symbol),
                            },
                            asset_class: ReferenceAssetClass::Crypto,
                            value: crypto_price.value,
                            full_accuracy_value: None,
                            source_timestamp: ts,
                            received_at: Utc::now(),
                            is_carried_forward: false,
                        },
                    )
                    .await;

                    let update = MarketUpdate::SpotPrice {
                        symbol: Arc::from(symbol_upper.as_str()),
                        price: crypto_price.value,
                        ts,
                    };

                    let receivers = tx.receiver_count();

                    match tx.send(update) {
                        Ok(_) => {
                            price_count += 1;
                            if logged_spot_symbols.insert(symbol_upper.clone()) {
                                info!(
                                    symbol = %symbol_upper,
                                    price = %crypto_price.value,
                                    receivers,
                                    "First RTDS spot price received"
                                );
                            }
                            if price_count % 100 == 0 {
                                debug!(
                                    prices = price_count,
                                    tracked_symbols = logged_spot_symbols.len(),
                                    receivers,
                                    "RTDS spot prices forwarded"
                                );
                            }

                            // Persist to DB at most once per second per symbol.
                            if let Some(ref db) = pool {
                                // Truncate to second by zeroing sub-second component.
                                let ts_sec = ts.with_nanosecond(0).unwrap_or(ts);
                                let last = last_persisted.get(&symbol_upper).copied();
                                if last.map_or(true, |l| ts_sec > l) {
                                    last_persisted.insert(symbol_upper.clone(), ts_sec);
                                    persist_spot_price(db, &symbol_upper, crypto_price.value, ts)
                                        .await;
                                }
                            }
                        }
                        Err(_) => {
                            warn!(
                                symbols = ?symbols_upper,
                                "Broadcast channel closed, stopping RTDS spot feed"
                            );
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "RTDS crypto_prices stream error");
                    // Don't exit on transient errors, let SDK handle reconnection
                }
            }
        }

        info!("RTDS spot price feed ended");
    })
}

/// Spawn a task that polls `binance_price_ticks` every 5 seconds and publishes
/// `MarketUpdate::SpotPrice` events as a fallback when the RTDS WebSocket is unavailable.
///
/// This ensures the strategy always has fresh spot prices even if the RTDS
/// subscription fails (e.g. protocol mismatch, network issues).
pub fn spawn_db_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_ts: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
        let mut price_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB spot price fallback feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Fetch latest price per symbol from binance_price_ticks
            let rows: Vec<(String, rust_decimal::Decimal, chrono::DateTime<chrono::Utc>)> =
                match sqlx::query_as(
                    r#"
                    SELECT DISTINCT ON (symbol) symbol, price, trade_time
                    FROM binance_price_ticks
                    WHERE symbol = ANY($1)
                      AND trade_time > NOW() - INTERVAL '30 seconds'
                    ORDER BY symbol, trade_time DESC
                    "#,
                )
                .bind(&symbols_upper)
                .fetch_all(&pool)
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "DB spot feed query failed");
                        continue;
                    }
                };

            for (symbol, price, ts) in rows {
                // Only emit if newer than last seen
                let last = last_ts.get(&symbol).copied();
                if last.map_or(true, |l| ts > l) {
                    last_ts.insert(symbol.clone(), ts);
                    let update = MarketUpdate::SpotPrice { symbol: Arc::from(symbol.as_str()), price, ts };
                    if tx.send(update).is_err() {
                        return; // channel closed
                    }
                    price_count += 1;
                    if price_count % 50 == 0 {
                        debug!(prices = price_count, "DB spot feed forwarded prices");
                    }
                }
            }
        }
    })
}

/// Spawn a task that polls `binance_agg_trade_ticks` and publishes
/// `MarketUpdate::AggTrade` events for live/dry-run strategies.
///
/// This keeps aggTrade collection decoupled from strategy runtimes while still
/// allowing the runtime to consume a near-real-time trade-flow signal stream.
pub fn spawn_db_aggtrade_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_seen: HashMap<String, (chrono::DateTime<chrono::Utc>, i64)> = HashMap::new();
        let mut trade_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB aggTrade fallback feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let rows: Vec<(
                String,
                i64,
                rust_decimal::Decimal,
                rust_decimal::Decimal,
                bool,
                chrono::DateTime<chrono::Utc>,
            )> = match sqlx::query_as(
                r#"
                SELECT symbol, agg_trade_id, price, quantity, is_buyer_maker, trade_time
                FROM binance_agg_trade_ticks
                WHERE symbol = ANY($1)
                  AND trade_time > NOW() - INTERVAL '30 seconds'
                ORDER BY trade_time ASC, agg_trade_id ASC
                "#,
            )
            .bind(&symbols_upper)
            .fetch_all(&pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "DB aggTrade feed query failed");
                    continue;
                }
            };

            for (symbol, agg_trade_id, price, quantity, is_buyer_maker, ts) in rows {
                let should_emit = match last_seen.get(&symbol).copied() {
                    Some((last_ts, last_id)) => {
                        ts > last_ts || (ts == last_ts && agg_trade_id > last_id)
                    }
                    None => true,
                };
                if !should_emit {
                    continue;
                }

                last_seen.insert(symbol.clone(), (ts, agg_trade_id));
                let update = MarketUpdate::AggTrade {
                    symbol: Arc::from(symbol.as_str()),
                    agg_trade_id: agg_trade_id as u64,
                    price,
                    quantity,
                    is_buyer_maker,
                    ts,
                };
                if tx.send(update).is_err() {
                    return;
                }
                trade_count += 1;
                if trade_count % 100 == 0 {
                    debug!(trades = trade_count, "DB aggTrade feed forwarded trades");
                }
            }
        }
    })
}

/// Spawn a task that polls `binance_lob_ticks` and publishes
/// `MarketUpdate::L2` and `MarketUpdate::L2Depth` events for live/dry-run strategies.
pub fn spawn_db_l2_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_seen: HashMap<String, (chrono::DateTime<chrono::Utc>, i64)> = HashMap::new();
        let mut l2_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB L2 feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let rows: Vec<(
                String,
                i64,
                rust_decimal::Decimal,
                i32,
                rust_decimal::Decimal,
                rust_decimal::Decimal,
                chrono::DateTime<chrono::Utc>,
            )> = match sqlx::query_as(
                r#"
                    SELECT symbol,
                           COALESCE(update_id, 0) AS update_id,
                    COALESCE(obi_5, 0) AS obi_5,
                           COALESCE(spread_bps, 0)::int AS spread_bps,
                           COALESCE(bid_volume_5, 0) AS bid_volume_5,
                           COALESCE(ask_volume_5, 0) AS ask_volume_5,
                           event_time
                    FROM binance_lob_ticks
                    WHERE symbol = ANY($1)
                      AND event_time > NOW() - INTERVAL '30 seconds'
                    ORDER BY event_time ASC, update_id ASC
                    "#,
            )
            .bind(&symbols_upper)
            .fetch_all(&pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "DB L2 feed query failed");
                    continue;
                }
            };

            for (symbol, update_id, obi, spread_bps, bid_volume_5, ask_volume_5, ts) in rows {
                let should_emit = match last_seen.get(&symbol).copied() {
                    Some((last_ts, last_id)) => {
                        ts > last_ts || (ts == last_ts && update_id > last_id)
                    }
                    None => true,
                };
                if !should_emit {
                    continue;
                }

                last_seen.insert(symbol.clone(), (ts, update_id));
                for update in l2_updates_from_depth_totals(
                    &symbol,
                    obi.to_f64().unwrap_or_default(),
                    spread_bps as u32,
                    bid_volume_5,
                    ask_volume_5,
                    ts,
                ) {
                    if tx.send(update).is_err() {
                        return;
                    }
                }
                l2_count += 1;
                if l2_count % 100 == 0 {
                    debug!(updates = l2_count, "DB L2 feed forwarded updates");
                }
            }
        }
    })
}

fn l2_updates_from_depth_totals(
    symbol: &str,
    obi: f64,
    spread_bps: u32,
    bid_depth_near: Decimal,
    ask_depth_near: Decimal,
    ts: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let sym: Arc<str> = Arc::from(symbol);
    let mut updates = vec![MarketUpdate::L2 {
        symbol: sym.clone(),
        obi,
        spread_bps,
        ts,
    }];

    let bid_depth_near = bid_depth_near.to_f64().unwrap_or(0.0);
    let ask_depth_near = ask_depth_near.to_f64().unwrap_or(0.0);

    updates.push(MarketUpdate::L2Depth {
        symbol: sym,
        obi,
        spread_bps,
        bid_depth_near,
        ask_depth_near,
        ts,
    });

    updates
}

#[cfg(test)]
fn l2_updates_from_book(
    symbol: &str,
    obi: f64,
    spread_bps: u32,
    mid_price: Decimal,
    bids: Option<&Value>,
    asks: Option<&Value>,
    ts: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let sym: Arc<str> = Arc::from(symbol);
    let mut updates = vec![MarketUpdate::L2 {
        symbol: sym.clone(),
        obi,
        spread_bps,
        ts,
    }];

    if bids.is_none() && asks.is_none() {
        return updates;
    }

    let Some(mid_price) = mid_price.to_f64() else {
        return updates;
    };
    if !mid_price.is_finite() || mid_price <= 0.0 {
        return updates;
    }

    let empty = Value::Null;
    let (bid_depth_near, ask_depth_near) = near_depth(
        bids.unwrap_or(&empty),
        asks.unwrap_or(&empty),
        mid_price,
        NEAR_DEPTH_PCT_RANGE,
    );

    updates.push(MarketUpdate::L2Depth {
        symbol: sym,
        obi,
        spread_bps,
        bid_depth_near,
        ask_depth_near,
        ts,
    });

    updates
}

fn near_depth(bids: &Value, asks: &Value, mid_price: f64, pct_range: f64) -> (f64, f64) {
    if !mid_price.is_finite() || mid_price <= 0.0 || !pct_range.is_finite() || pct_range < 0.0 {
        return (0.0, 0.0);
    }

    let bid_min = mid_price * (1.0 - pct_range);
    let ask_max = mid_price * (1.0 + pct_range);

    (
        sum_depth_in_range(bids, bid_min, mid_price),
        sum_depth_in_range(asks, mid_price, ask_max),
    )
}

fn sum_depth_in_range(levels: &Value, min_price: f64, max_price: f64) -> f64 {
    levels
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(parse_depth_level)
                .filter(|(price, _)| *price >= min_price && *price <= max_price)
                .map(|(_, size)| size)
                .sum()
        })
        .unwrap_or(0.0)
}

fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Spawn a task that polls the Polymarket CLOB REST API for orderbook data
/// and publishes `MarketUpdate::Quote` events.
///
/// REST polling is more reliable than WS for the 5-min window lifecycle.
/// Polls every 5 seconds per token batch.
///
/// When `pool` is provided, each non-empty quote is also persisted to
/// `clob_quote_ticks` so that historical backtests can replay the same data.
pub fn spawn_quote_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let poll_interval = std::time::Duration::from_secs(5);
        let mut quoted_tokens = 0_u64;
        let mut logged_quote_tokens = HashSet::new();

        info!(tokens = token_ids.len(), "Starting REST quote poller");

        loop {
            for token in &token_ids {
                let token_str = token.to_string();

                // Use /midpoint API instead of /book to get the real market price.
                // The /book endpoint returns extreme placeholder orders (bid=0.01, ask=0.99)
                // when there is no real liquidity, which is useless for strategy evaluation.
                // /midpoint returns the actual market consensus price.
                let url = format!(
                    "https://clob.polymarket.com/midpoint?token_id={}",
                    token_str
                );

                match http.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            let mid = body["mid"].as_str().and_then(|p| p.parse::<Decimal>().ok());

                            if let Some(mid_price) = mid {
                                // Store mid as both bid and ask — strategy uses ask for entry.
                                // A small synthetic spread (0.5%) is applied so bid < ask.
                                let half_spread = mid_price * rust_decimal_macros::dec!(0.005);
                                let bid = Some(
                                    (mid_price - half_spread).max(rust_decimal_macros::dec!(0.01)),
                                );
                                let ask = Some(
                                    (mid_price + half_spread).min(rust_decimal_macros::dec!(0.99)),
                                );

                                let now = Utc::now();
                                let update = MarketUpdate::Quote {
                                    token_id: Arc::from(token_str.as_str()),
                                    bid,
                                    ask,
                                    bid_size: None,
                                    ask_size: None,
                                    ts: now,
                                };
                                if tx.send(update).is_err() {
                                    warn!(
                                        tokens = token_ids.len(),
                                        "All receivers dropped, stopping quote poller"
                                    );
                                    return;
                                }

                                // Persist to DB for backtest replay.
                                if let Some(ref db) = pool {
                                    persist_quote(db, &token_str, bid, ask, now).await;
                                }

                                quoted_tokens += 1;
                                if logged_quote_tokens.insert(token_str.clone()) {
                                    info!(
                                        token = %token_str,
                                        mid = %mid_price,
                                        bid = ?bid,
                                        ask = ?ask,
                                        "First midpoint quote observed"
                                    );
                                } else if quoted_tokens % 100 == 0 {
                                    info!(
                                        quotes = quoted_tokens,
                                        tracked_tokens = logged_quote_tokens.len(),
                                        "REST quote poller forwarded midpoint quotes"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, token = %token_str, "REST midpoint fetch failed");
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    })
}

/// Spawn a task that subscribes to Chainlink price feeds via RTDS WebSocket.
///
/// Used to capture S0 (open price) at eventStartTime for 5M markets.
/// Polymarket uses Chainlink as the canonical price source for settlement.
///
/// Prices are stored in the shared cache for scanner to use when creating EventDiscovered.
pub fn spawn_chainlink_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_chainlink_symbols = HashSet::new();
        let symbols_chainlink: Vec<String> = symbols
            .iter()
            .map(|s| {
                let base = s.trim_end_matches("USDT").to_lowercase();
                format!("{}/usd", base)
            })
            .collect();

        info!(
            symbols = ?symbols_chainlink,
            "Starting RTDS Chainlink price feed"
        );

        let client = RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
            .expect("RTDS market-data config should be valid");

        // Subscribe to all Chainlink symbols (None = all, Some(vec) = specific)
        // For now subscribe to all since we only have 7 symbols
        let stream = match client.subscribe_chainlink_prices(None) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "Failed to subscribe to chainlink_prices");
                return;
            }
        };

        let mut stream = Box::pin(stream);
        let mut price_count = 0_u64;

        while let Some(result) = stream.next().await {
            match result {
                Ok(chainlink_price) => {
                    // Filter to only our symbols
                    if !symbols_chainlink.contains(&chainlink_price.symbol) {
                        continue;
                    }

                    // Convert Unix millis to DateTime<Utc>
                    let ts = DateTime::from_timestamp_millis(chainlink_price.timestamp)
                        .unwrap_or_else(Utc::now);

                    upsert_reference_price(
                        &reference_prices,
                        ReferencePriceSnapshot {
                            key: ReferencePriceKey {
                                source: ReferencePriceSource::Chainlink,
                                symbol: normalize_reference_symbol(&chainlink_price.symbol),
                            },
                            asset_class: ReferenceAssetClass::Crypto,
                            value: chainlink_price.value,
                            full_accuracy_value: None,
                            source_timestamp: ts,
                            received_at: Utc::now(),
                            is_carried_forward: false,
                        },
                    )
                    .await;

                    let update = MarketUpdate::ReferencePrice {
                        symbol: Arc::from(normalize_reference_symbol(&chainlink_price.symbol).as_str()),
                        source: Arc::from(ReferencePriceSource::Chainlink.as_str()),
                        asset_class: Arc::from(ReferenceAssetClass::Crypto.as_str()),
                        price: chainlink_price.value,
                        full_accuracy_value: None,
                        is_carried_forward: false,
                        ts,
                    };

                    if tx.send(update).is_err() {
                        warn!(
                            symbols = ?symbols_chainlink,
                            "Broadcast channel closed, stopping RTDS Chainlink feed"
                        );
                        return;
                    }

                    if let Some(ref db) = pool {
                        persist_chainlink_price(
                            db,
                            &chainlink_price.symbol,
                            chainlink_price.value,
                            ts,
                        )
                        .await;
                    }

                    let receivers = tx.receiver_count();
                    price_count += 1;

                    if logged_chainlink_symbols.insert(chainlink_price.symbol.clone()) {
                        info!(
                            symbol = %chainlink_price.symbol,
                            price = %chainlink_price.value,
                            receivers,
                            "First Chainlink price received and cached"
                        );
                    }
                    if price_count % 100 == 0 {
                        debug!(
                            prices = price_count,
                            tracked_symbols = logged_chainlink_symbols.len(),
                            receivers,
                            "Chainlink prices cached"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "RTDS chainlink_prices stream error");
                    // Don't exit on transient errors, let SDK handle reconnection
                }
            }
        }

        info!("RTDS Chainlink price feed ended");
    })
}

/// Spawn one RTDS Pyth feed task per symbol and publish all ticks into the
/// shared reference-price registry.
pub fn spawn_pyth_reference_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if symbols.is_empty() {
            info!("No Pyth symbols configured, skipping equity_prices feed");
            return;
        }

        let mut join_set = JoinSet::new();

        for raw_symbol in symbols {
            let tx = tx.clone();
            let registry = reference_prices.clone();
            let pool = pool.clone();
            let subscribe_symbol = raw_symbol.clone();

            join_set.spawn(async move {
                let normalized_symbol = pyth_symbol(&subscribe_symbol);
                let asset_class = infer_pyth_asset_class(&subscribe_symbol);
                let client =
                    RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
                        .expect("RTDS market-data config should be valid");
                let stream =
                    match client.subscribe_equity_prices(Some(subscribe_symbol.clone()), true) {
                        Ok(stream) => stream,
                        Err(error) => {
                            error!(
                                symbol = %subscribe_symbol,
                                error = %error,
                                "Failed to subscribe to equity_prices"
                            );
                            return;
                        }
                    };

                let mut stream = Box::pin(stream);
                let mut message_count = 0_u64;

                while let Some(result) = stream.next().await {
                    match result {
                        Ok(EquityPriceMessage::Update(update)) => {
                            let source_timestamp =
                                DateTime::from_timestamp_millis(update.timestamp)
                                    .unwrap_or_else(Utc::now);
                            let received_at = update
                                .received_at
                                .and_then(DateTime::from_timestamp_millis)
                                .unwrap_or_else(Utc::now);

                            let snapshot = ReferencePriceSnapshot {
                                key: ReferencePriceKey {
                                    source: ReferencePriceSource::Pyth,
                                    symbol: normalize_reference_symbol(&update.symbol),
                                },
                                asset_class,
                                value: update.value,
                                full_accuracy_value: Some(update.full_accuracy_value.clone()),
                                source_timestamp,
                                received_at,
                                is_carried_forward: update.is_carried_forward,
                            };

                            upsert_reference_price(&registry, snapshot.clone()).await;

                            if tx.send(reference_price_update(&snapshot)).is_err() {
                                warn!(
                                    symbol = %subscribe_symbol,
                                    "Broadcast channel closed, stopping Pyth reference-price worker"
                                );
                                return;
                            }

                            if let Some(ref db) = pool {
                                persist_reference_price(db, &snapshot).await;
                            }

                            message_count += 1;
                            if message_count == 1 || message_count % 100 == 0 {
                                info!(
                                    symbol = %normalized_symbol,
                                    source = %ReferencePriceSource::Pyth.as_str(),
                                    asset_class = %asset_class.as_str(),
                                    carried_forward = snapshot.is_carried_forward,
                                    count = message_count,
                                    "Pyth reference prices captured"
                                );
                            }
                        }
                        Ok(EquityPriceMessage::Snapshot(snapshot)) => {
                            let snapshot_symbol = normalize_reference_symbol(&snapshot.symbol);
                            for point in snapshot.data {
                                let source_timestamp =
                                    DateTime::from_timestamp_millis(point.timestamp)
                                        .unwrap_or_else(Utc::now);
                                let received_at = point
                                    .received_at
                                    .and_then(DateTime::from_timestamp_millis)
                                    .unwrap_or_else(Utc::now);
                                let snapshot = ReferencePriceSnapshot {
                                    key: ReferencePriceKey {
                                        source: ReferencePriceSource::Pyth,
                                        symbol: snapshot_symbol.clone(),
                                    },
                                    asset_class,
                                    value: point.value,
                                    full_accuracy_value: point.full_accuracy_value.clone(),
                                    source_timestamp,
                                    received_at,
                                    is_carried_forward: point.is_carried_forward,
                                };

                                upsert_reference_price(&registry, snapshot.clone()).await;

                                if tx.send(reference_price_update(&snapshot)).is_err() {
                                    warn!(
                                        symbol = %subscribe_symbol,
                                        "Broadcast channel closed, stopping Pyth reference-price worker"
                                    );
                                    return;
                                }

                                if let Some(ref db) = pool {
                                    persist_reference_price(db, &snapshot).await;
                                }
                            }
                        }
                        Ok(_) => {
                            debug!(
                                symbol = %subscribe_symbol,
                                "Ignoring unsupported non-exhaustive equity_prices message"
                            );
                        }
                        Err(error) => {
                            warn!(
                                symbol = %subscribe_symbol,
                                error = %error,
                                "RTDS equity_prices stream error"
                            );
                        }
                    }
                }

                warn!(symbol = %subscribe_symbol, "RTDS equity_prices stream ended");
            });
        }

        while let Some(result) = join_set.join_next().await {
            if let Err(error) = result {
                warn!(error = %error, "A Pyth reference-price worker exited");
            }
        }
    })
}

/// Persist a spot price tick to `binance_price_ticks` for backtest replay.
/// Called at most once per second per symbol (throttled in spawn_spot_feed).
async fn persist_spot_price(
    pool: &PgPool,
    symbol: &str,
    price: Decimal,
    trade_time: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO binance_price_ticks (symbol, price, trade_time, received_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(symbol)
    .bind(price)
    .bind(trade_time)
    .execute(pool)
    .await;

    if let Err(e) = result {
        debug!(symbol, error = %e, "Failed to persist spot price tick");
    }
}

/// Persist a quote tick to `clob_quote_ticks` for backtest replay.
/// Every tick is stored at full resolution (no per-second dedup).
async fn persist_quote(
    pool: &PgPool,
    token_id: &str,
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    received_at: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO clob_quote_ticks (token_id, best_bid, best_ask, received_at, source)
        VALUES ($1, $2, $3, $4, 'ploy_runner_live')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(token_id)
    .bind(bid)
    .bind(ask)
    .bind(received_at)
    .execute(pool)
    .await;

    if let Err(e) = result {
        debug!(token_id, error = %e, "Failed to persist quote tick");
    }
}

async fn persist_chainlink_price(
    pool: &PgPool,
    symbol: &str,
    price: Decimal,
    source_timestamp: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO chainlink_price_ticks (symbol, price, source_timestamp, received_at)
        VALUES ($1, $2, $3, NOW())
        "#,
    )
    .bind(symbol)
    .bind(price)
    .bind(source_timestamp)
    .execute(pool)
    .await;

    if let Err(error) = result {
        debug!(symbol, error = %error, "Failed to persist Chainlink price tick");
    }
}

async fn persist_reference_price(pool: &PgPool, snapshot: &ReferencePriceSnapshot) {
    let result = sqlx::query(
        r#"
        INSERT INTO reference_price_ticks (
            symbol, source, asset_class, price, full_accuracy_value,
            price_time, received_at, is_carried_forward
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(&snapshot.key.symbol)
    .bind(snapshot.key.source.as_str())
    .bind(snapshot.asset_class.as_str())
    .bind(snapshot.value)
    .bind(snapshot.full_accuracy_value.as_deref())
    .bind(snapshot.source_timestamp)
    .bind(snapshot.received_at)
    .bind(snapshot.is_carried_forward)
    .execute(pool)
    .await;

    if let Err(error) = result {
        debug!(
            symbol = %snapshot.key.symbol,
            source = %snapshot.key.source.as_str(),
            error = %error,
            "Failed to persist reference price tick"
        );
    }
}

fn reference_price_update(snapshot: &ReferencePriceSnapshot) -> MarketUpdate {
    MarketUpdate::ReferencePrice {
        symbol: Arc::from(snapshot.key.symbol.as_str()),
        source: Arc::from(snapshot.key.source.as_str()),
        asset_class: Arc::from(snapshot.asset_class.as_str()),
        price: snapshot.value,
        full_accuracy_value: snapshot.full_accuracy_value.as_deref().map(Arc::from),
        is_carried_forward: snapshot.is_carried_forward,
        ts: snapshot.source_timestamp,
    }
}

#[derive(Debug)]
struct AggTradeMsg {
    symbol: String,
    agg_trade_id: i64,
    first_trade_id: i64,
    last_trade_id: i64,
    price: rust_decimal::Decimal,
    quantity: rust_decimal::Decimal,
    trade_time: chrono::DateTime<chrono::Utc>,
    event_time: chrono::DateTime<chrono::Utc>,
    is_buyer_maker: bool,
}

fn parse_agg_trade_msg(v: &serde_json::Value) -> Option<AggTradeMsg> {
    use chrono::TimeZone;
    let symbol = v["s"].as_str()?.to_string();
    let agg_trade_id = v["a"].as_i64()?;
    let first_trade_id = v["f"].as_i64().unwrap_or(0);
    let last_trade_id = v["l"].as_i64().unwrap_or(0);
    let price_str = v["p"].as_str()?;
    let qty_str = v["q"].as_str()?;
    let trade_time_ms = v["T"].as_i64()?;
    let event_time_ms = v["E"].as_i64().unwrap_or(trade_time_ms);
    let is_buyer_maker = v["m"].as_bool().unwrap_or(false);
    let price = price_str.parse::<rust_decimal::Decimal>().ok()?;
    let quantity = qty_str.parse::<rust_decimal::Decimal>().ok()?;
    let trade_time = chrono::Utc.timestamp_millis_opt(trade_time_ms).single()?;
    let event_time = chrono::Utc.timestamp_millis_opt(event_time_ms).single()?;
    Some(AggTradeMsg {
        symbol, agg_trade_id, first_trade_id, last_trade_id,
        price, quantity, trade_time, event_time, is_buyer_maker,
    })
}

#[cfg(test)]
mod tests {
    use super::{l2_updates_from_book, parse_agg_trade_msg, rtds_market_data_ws_config};
    use chrono::Utc;
    use ploy_market_contracts::MarketUpdate;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn dry_run_rtds_market_data_uses_relaxed_ws_heartbeat_settings() {
        let config = rtds_market_data_ws_config();
        assert_eq!(config.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(config.heartbeat_timeout, Duration::from_secs(45));
        assert!(config.reconnect.max_attempts.is_none());
    }

    #[test]
    fn db_l2_feed_builds_depth_variant_from_pair_levels() {
        let ts = Utc::now();
        let updates = l2_updates_from_book(
            "BTCUSDT",
            0.2,
            11,
            dec!(100.0),
            Some(&json!([
                ["100.0", "2.0"],
                ["99.92", "3.5"],
                ["99.6", "9.0"]
            ])),
            Some(&json!([
                ["100.02", "1.5"],
                ["100.08", "4.0"],
                ["100.4", "8.0"]
            ])),
            ts,
        );

        assert!(
            matches!(updates.first(), Some(MarketUpdate::L2 { symbol, .. }) if symbol.as_ref() == "BTCUSDT")
        );
        assert!(matches!(
            updates.get(1),
            Some(MarketUpdate::L2Depth {
                bid_depth_near,
                ask_depth_near,
                spread_bps,
                ..
            }) if (bid_depth_near - 5.5).abs() < 1e-9
                && (ask_depth_near - 5.5).abs() < 1e-9
                && *spread_bps == 11
        ));
    }

    #[test]
    fn parse_agg_trade_message_extracts_fields() {
        let msg = serde_json::json!({
            "e": "aggTrade",
            "s": "BTCUSDT",
            "a": 12345_i64,
            "p": "50000.00",
            "q": "0.01",
            "f": 100_i64,
            "l": 105_i64,
            "T": 1672515782136_i64,
            "m": true
        });
        let parsed = parse_agg_trade_msg(&msg).unwrap();
        assert_eq!(parsed.symbol, "BTCUSDT");
        assert_eq!(parsed.agg_trade_id, 12345);
        assert!((parsed.price.to_f64().unwrap() - 50000.0).abs() < 0.01);
        assert!(parsed.is_buyer_maker);
    }
}
