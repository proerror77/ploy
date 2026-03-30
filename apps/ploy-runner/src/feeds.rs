//! Live market data feed producers.
//!
//! Two async tasks that bridge vendor SDK WebSocket streams into the
//! unified `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Spawn a task that polls Binance REST API for spot prices
/// and publishes `MarketUpdate::SpotPrice` events every 2 seconds.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let poll_interval = std::time::Duration::from_secs(2);
        let mut logged_spot_symbols = HashSet::new();

        info!(symbols = ?symbols, "Starting Binance REST spot price poller");

        loop {
            for symbol in &symbols {
                let url = format!(
                    "https://api.binance.com/api/v3/ticker/price?symbol={}",
                    symbol.to_uppercase()
                );
                match http.get(&url).send().await {
                    Ok(resp) => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            if let Some(price_str) = body["price"].as_str() {
                                if let Ok(price) = price_str.parse::<Decimal>() {
                                    let update = MarketUpdate::SpotPrice {
                                        symbol: symbol.to_uppercase(),
                                        price,
                                        ts: Utc::now(),
                                    };
                                    let symbol_upper = symbol.to_uppercase();
                                    let receivers = tx.receiver_count();
                                    match tx.send(update) {
                                        Ok(_) => {
                                            if logged_spot_symbols.insert(symbol_upper.clone()) {
                                                info!(
                                                    symbol = %symbol_upper,
                                                    price = %price,
                                                    receivers,
                                                    "First spot price observed"
                                                );
                                            }
                                            debug!(
                                                symbol = %symbol_upper,
                                                price = %price,
                                                receivers,
                                                "Spot price sent to broadcast"
                                            );
                                        }
                                        Err(_) => {
                                            warn!("Broadcast channel closed");
                                            return;
                                        }
                                    }
                                }
                            } else {
                                warn!(symbol, "No price in Binance response: {body}");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, symbol = %symbol, "Binance REST fetch failed");
                    }
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
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
                                    warn!("All receivers dropped, stopping quote poller");
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
