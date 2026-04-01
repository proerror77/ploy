//! Market scanner that discovers active Polymarket 5-min binary option windows.
//!
//! Polls the Gamma REST API every 30s for markets matching the configured
//! symbols. Injects `EventDiscovered`/`EventExpired` into the broadcast
//! channel. Dynamically spawns CLOB WS quote feeds for newly discovered tokens.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{Duration, Utc};
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::feeds::{spawn_quote_feed, ChainlinkPriceCache};

const SCAN_INTERVAL_SECS: u64 = 30;

fn usable_metadata_threshold(raw: Option<&str>) -> Option<Decimal> {
    raw.and_then(|value| value.parse::<Decimal>().ok())
        .filter(|threshold| *threshold > Decimal::ONE)
}

/// Map full asset names (from Polymarket questions) to trading pair symbols.
fn name_to_symbol(question: &str) -> Option<&'static str> {
    let q = question.to_uppercase();
    if q.contains("BITCOIN") || q.contains("BTC") {
        Some("BTCUSDT")
    } else if q.contains("ETHEREUM") || q.contains("ETH") {
        Some("ETHUSDT")
    } else if q.contains("SOLANA") || q.contains("SOL ") {
        Some("SOLUSDT")
    } else if q.contains("XRP") {
        Some("XRPUSDT")
    } else if q.contains("DOGECOIN") || q.contains("DOGE") {
        Some("DOGEUSDT")
    } else if q.contains("HYPE") {
        Some("HYPEUSDT")
    } else if q.contains("BNB") || q.contains("BINANCE COIN") {
        Some("BNBUSDT")
    } else {
        None
    }
}

struct TrackedEvent {
    end_time: chrono::DateTime<Utc>,
}

/// Spawn a background task that periodically discovers active 5-min binary
/// option markets, injects lifecycle events, and spawns quote feeds for
/// newly discovered token IDs.
///
/// Uses the Chainlink price cache to populate price_to_beat with the most
/// recent Chainlink price for each symbol.
pub fn spawn_market_scanner(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    chainlink_cache: ChainlinkPriceCache,
    symbols: Vec<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = GammaClient::default();
        let mut tracked: HashMap<String, TrackedEvent> = HashMap::new();
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut quote_handles: Vec<JoinHandle<()>> = Vec::new();

        loop {
            let now = Utc::now();

            // Expire events that have passed end_time
            let expired: Vec<String> = tracked
                .iter()
                .filter(|(_, ev)| ev.end_time <= now)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &expired {
                let _ = tx.send(MarketUpdate::EventExpired {
                    event_id: id.clone(),
                });
                tracked.remove(id);
            }

            // Query active markets ending in the next 6 minutes
            let mut request = MarketsRequest::default();
            request.end_date_min = Some(now);
            request.end_date_max = Some(now + Duration::minutes(6));
            request.closed = Some(false);
            request.limit = Some(100);

            match client.markets(&request).await {
                Ok(markets) => {
                    let mut new_tokens: Vec<U256> = Vec::new();

                    if markets.is_empty() {
                        debug!("No active markets in the next 6 minutes");
                    }

                    for market in &markets {
                        let market_id = &market.id;
                        if tracked.contains_key(market_id) {
                            continue;
                        }

                        // Match symbol from question text (e.g. "Bitcoin Up or Down" → BTCUSDT)
                        let question = market.question.as_deref().unwrap_or("");
                        let symbol = match name_to_symbol(question) {
                            Some(s) if symbols.iter().any(|cfg| cfg.to_uppercase() == s) => {
                                s.to_owned()
                            }
                            _ => continue,
                        };

                        // Need exactly 2 CLOB token IDs (Up and Down)
                        let token_ids = match &market.clob_token_ids {
                            Some(ids) if ids.len() == 2 => ids,
                            _ => continue,
                        };

                        let end_time = match market.end_date {
                            Some(t) => t,
                            None => continue,
                        };

                        let window_secs = (end_time - now).num_seconds().max(0) as u64;
                        let up_token = token_ids[0].to_string();
                        let down_token = token_ids[1].to_string();

                        // Try to get price_to_beat from Chainlink cache first (most accurate).
                        // Relative 5m/15m up/down markets frequently expose
                        // groupItemThreshold=0 in Gamma, which is not a usable
                        // threshold and should not enter the runtime.
                        let price_to_beat = {
                            // Convert BTCUSDT -> btc/usd for Chainlink lookup
                            let chainlink_symbol = symbol
                                .trim_end_matches("USDT")
                                .to_lowercase()
                                + "/usd";

                            let cache_guard = chainlink_cache.read().await;
                            cache_guard
                                .get(&chainlink_symbol)
                                .map(|(price, _ts)| *price)
                                .or_else(|| usable_metadata_threshold(market.group_item_threshold.as_deref()))
                        };

                        // Collect new tokens for quote subscription
                        if subscribed_tokens.insert(up_token.clone()) {
                            new_tokens.push(token_ids[0]);
                        }
                        if subscribed_tokens.insert(down_token.clone()) {
                            new_tokens.push(token_ids[1]);
                        }

                        tracked.insert(
                            market_id.clone(),
                            TrackedEvent { end_time },
                        );

                        let _ = tx.send(MarketUpdate::EventDiscovered {
                            event_id: market_id.clone(),
                            symbol,
                            up_token,
                            down_token,
                            end_time,
                            window_secs,
                            price_to_beat,
                        });
                    }

                    // Spawn a new quote feed for newly discovered tokens
                    if !new_tokens.is_empty() {
                        info!(
                            new_tokens = new_tokens.len(),
                            total_tracked = tracked.len(),
                            "Discovered new markets, subscribing to quotes",
                        );
                        let handle = spawn_quote_feed(tx.clone(), new_tokens);
                        quote_handles.push(handle);
                    } else {
                        debug!(tracked = tracked.len(), "Scanner poll: no new markets");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Gamma markets query failed, will retry");
                }
            }

            // Clean up finished quote handles
            quote_handles.retain(|h| !h.is_finished());

            tokio::time::sleep(std::time::Duration::from_secs(SCAN_INTERVAL_SECS)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::usable_metadata_threshold;

    #[test]
    fn usable_metadata_threshold_rejects_relative_zero_threshold() {
        assert_eq!(usable_metadata_threshold(Some("0")), None);
        assert_eq!(usable_metadata_threshold(Some("0.5")), None);
        assert_eq!(usable_metadata_threshold(Some("123.45")), Some(dec!(123.45)));
    }
}
