//! Live market data feed producers.
//!
//! Two async tasks that bridge vendor SDK WebSocket streams into the
//! unified `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use futures::StreamExt;
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::rtds::Client as RtdsClient;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Shared cache of recent Chainlink prices.
///
/// Keyed by symbol (e.g., "btc/usd"), stores the most recent price and timestamp.
/// Used by scanner to populate price_to_beat when EventDiscovered is sent.
pub type ChainlinkPriceCache = Arc<RwLock<HashMap<String, (Decimal, DateTime<Utc>)>>>;

/// Create a new empty Chainlink price cache.
pub fn new_chainlink_cache() -> ChainlinkPriceCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Spawn a task that subscribes to Binance spot prices via RTDS WebSocket
/// and publishes `MarketUpdate::SpotPrice` events in real-time.
///
/// When `pool` is provided, each tick is also persisted to `binance_price_ticks`
/// (deduplicated at second granularity) so that historical backtests can replay
/// the same spot-price stream.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
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

        // Create RTDS client with default config (wss://ws-live-data.polymarket.com)
        let client = RtdsClient::default();

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

                    let update = MarketUpdate::SpotPrice {
                        symbol: symbol_upper.clone(),
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
                                    persist_spot_price(db, &symbol_upper, crypto_price.value, ts).await;
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
                let url = format!("https://clob.polymarket.com/midpoint?token_id={}", token_str);

                match http.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            let mid = body["mid"]
                                .as_str()
                                .and_then(|p| p.parse::<Decimal>().ok());

                            if let Some(mid_price) = mid {
                                // Store mid as both bid and ask — strategy uses ask for entry.
                                // A small synthetic spread (0.5%) is applied so bid < ask.
                                let half_spread = mid_price * rust_decimal_macros::dec!(0.005);
                                let bid = Some((mid_price - half_spread).max(rust_decimal_macros::dec!(0.01)));
                                let ask = Some((mid_price + half_spread).min(rust_decimal_macros::dec!(0.99)));

                                let now = Utc::now();
                                let update = MarketUpdate::Quote {
                                    token_id: token_str.clone(),
                                    bid,
                                    ask,
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
    cache: ChainlinkPriceCache,
    symbols: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_chainlink_symbols = HashSet::new();
        // Chainlink uses slash-separated format: "btc/usd", "eth/usd"
        let symbols_chainlink: Vec<String> = symbols
            .iter()
            .map(|s| {
                // BTCUSDT -> btc/usd
                let base = s.trim_end_matches("USDT").to_lowercase();
                format!("{}/usd", base)
            })
            .collect();

        info!(
            symbols = ?symbols_chainlink,
            "Starting RTDS Chainlink price feed"
        );

        // Create RTDS client
        let client = RtdsClient::default();

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

                    // Store in cache for scanner to use
                    {
                        let mut cache_guard = cache.write().await;
                        cache_guard
                            .insert(chainlink_price.symbol.clone(), (chainlink_price.value, ts));
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
/// Deduplicates at second granularity via the unique index added in migration 023.
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
        ON CONFLICT (token_id, date_trunc('second', received_at AT TIME ZONE 'UTC')) DO NOTHING
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
