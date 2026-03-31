//! Live market data feed producers.
//!
//! Two async tasks that bridge vendor SDK WebSocket streams into the
//! unified `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::rtds::Client as RtdsClient;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Spawn a task that subscribes to Binance spot prices via RTDS WebSocket
/// and publishes `MarketUpdate::SpotPrice` events in real-time.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_spot_symbols = HashSet::new();
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();

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

                    let update = MarketUpdate::SpotPrice {
                        symbol: crypto_price.symbol.to_uppercase(),
                        price: crypto_price.value,
                        ts,
                    };

                    let symbol_upper = crypto_price.symbol.to_uppercase();
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
pub fn spawn_quote_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
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
                let url = format!(
                    "https://clob.polymarket.com/book?token_id={}",
                    token_str
                );

                match http.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            let bid = body["bids"]
                                .as_array()
                                .and_then(|b| b.first())
                                .and_then(|b| b["price"].as_str())
                                .and_then(|p| p.parse::<Decimal>().ok());

                            let ask = body["asks"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|a| a["price"].as_str())
                                .and_then(|p| p.parse::<Decimal>().ok());

                            if bid.is_some() || ask.is_some() {
                                let update = MarketUpdate::Quote {
                                    token_id: token_str.clone(),
                                    bid,
                                    ask,
                                    ts: Utc::now(),
                                };
                                if tx.send(update).is_err() {
                                    warn!(
                                        tokens = token_ids.len(),
                                        "All receivers dropped, stopping quote poller"
                                    );
                                    return;
                                }
                                quoted_tokens += 1;
                                if logged_quote_tokens.insert(token_str.clone()) {
                                    info!(
                                        token = %token_str,
                                        bid = ?bid,
                                        ask = ?ask,
                                        "First non-empty quote observed"
                                    );
                                } else if quoted_tokens % 100 == 0 {
                                    info!(
                                        quotes = quoted_tokens,
                                        tracked_tokens = logged_quote_tokens.len(),
                                        "REST quote poller forwarded non-empty quotes"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, token = %token_str, "REST book fetch failed");
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    })
}
