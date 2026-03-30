//! Live market data feed producers.
//!
//! Two async tasks that bridge vendor SDK WebSocket streams into the
//! unified `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::rtds::Client as RtdsClient;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Spawn a task that streams Binance spot prices via Polymarket RTDS
/// and publishes `MarketUpdate::SpotPrice` events.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            info!(symbols = ?symbols, "Connecting to RTDS spot price feed");

            let client = RtdsClient::default();
            let stream = match client.subscribe_crypto_prices(Some(symbols.clone())) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to subscribe to RTDS, retrying in 5s");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let mut stream = Box::pin(stream);
            let mut count: u64 = 0;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(price) => {
                        let symbol = price.symbol.to_uppercase();
                        let value = match Decimal::try_from(price.value) {
                            Ok(d) => d,
                            Err(_) => continue,
                        };
                        let ts = DateTime::<Utc>::from_timestamp(price.timestamp as i64, 0)
                            .unwrap_or_else(Utc::now);

                        let update = MarketUpdate::SpotPrice {
                            symbol,
                            price: value,
                            ts,
                        };

                        if tx.send(update).is_err() {
                            warn!("All receivers dropped, stopping spot feed");
                            return;
                        }
                        count += 1;
                        if count % 10000 == 0 {
                            debug!(count, "Spot price updates forwarded");
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "RTDS stream error, continuing");
                    }
                }
            }

            warn!("RTDS stream ended, reconnecting in 3s");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}

/// Spawn a task that subscribes to Polymarket CLOB WS price changes
/// for the given token IDs and publishes `MarketUpdate::Quote` events.
///
/// Token IDs are collected by the market scanner and passed here.
/// Each `PriceChange` message is a batch — we flatten and emit one
/// `Quote` per entry.
pub fn spawn_quote_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            info!(tokens = token_ids.len(), "Connecting to CLOB WS for quote feed");

            let client = WsClient::default();
            let stream = match client.subscribe_prices(token_ids.clone()) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to subscribe to CLOB WS prices, retrying in 5s");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let mut stream = Box::pin(stream);
            let mut count: u64 = 0;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(batch) => {
                        for entry in &batch.price_changes {
                            let token_id = entry.asset_id.to_string();
                            let ts = DateTime::<Utc>::from_timestamp(batch.timestamp, 0)
                                .unwrap_or_else(Utc::now);

                            let (bid, ask) = match entry.side {
                                Side::Buy => (Some(entry.price), None),
                                Side::Sell => (None, Some(entry.price)),
                                _ => continue,
                            };

                            let update = MarketUpdate::Quote {
                                token_id,
                                bid,
                                ask,
                                ts,
                            };

                            if tx.send(update).is_err() {
                                warn!("All receivers dropped, stopping quote feed");
                                return;
                            }
                            count += 1;
                        }
                        if count % 50000 == 0 && count > 0 {
                            debug!(count, "Quote updates forwarded");
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "CLOB WS stream error, continuing");
                    }
                }
            }

            warn!("CLOB WS stream ended, reconnecting in 3s");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}
