//! Market scanner that discovers active Polymarket markets.
//!
//! Crypto discovery still emits the existing `EventDiscovered` /
//! `EventExpired` runtime updates so the current strategy logic remains intact.
//! In parallel, the scanner now persists normalized market descriptors into
//! `pm_market_catalog`, including low-frequency sports discovery for later
//! capture and replay work.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use ploy_strategy_bundles::MarketUpdate;
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::discovery::crypto::DiscoveredCryptoMarket;
use crate::discovery::crypto::discover_crypto_markets;
use crate::discovery::sports::discover_sports_markets;
use crate::discovery::upsert_market_catalog;
use crate::feeds::spawn_quote_feed;
use crate::reference_prices::ReferencePriceRegistry;

const SCAN_INTERVAL_SECS: u64 = 30;
const SPORTS_DISCOVERY_REFRESH_SECS: i64 = 300;
const SPORTS_DISCOVERY_LIMIT: i32 = 500;

struct TrackedEvent {
    end_time: chrono::DateTime<Utc>,
}

/// Spawn a background task that periodically discovers active 5-min binary
/// option markets, injects lifecycle events, and spawns quote feeds for
/// newly discovered token IDs.
///
/// Uses the reference-price registry to populate price_to_beat with the most
/// recent Chainlink price for each symbol.
///
/// When `pool` is provided, newly discovered markets are upserted into
/// `pm_market_metadata` so that historical backtests can replay the same data.
pub fn spawn_market_scanner(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = GammaClient::default();
        let mut tracked: HashMap<String, TrackedEvent> = HashMap::new();
        let mut subscribed_tokens: HashSet<String> = HashSet::new();
        let mut quote_handles: Vec<JoinHandle<()>> = Vec::new();
        let mut last_sports_refresh: Option<DateTime<Utc>> = None;

        loop {
            let now = Utc::now();

            expire_tracked_events(&tx, &mut tracked, now);

            let mut request = MarketsRequest::default();
            request.end_date_min = Some(now);
            request.end_date_max = Some(now + Duration::minutes(6));
            request.closed = Some(false);
            request.limit = Some(100);

            match client.markets(&request).await {
                Ok(markets) => {
                    let mut new_tokens: Vec<U256> = Vec::new();
                    let discovered =
                        discover_crypto_markets(&markets, &symbols, &reference_prices, now).await;

                    if discovered.is_empty() {
                        debug!("No active crypto markets in the next 6 minutes");
                    }

                    for market in discovered {
                        persist_discovered_crypto_market(pool.as_ref(), &market).await;

                        if tracked.contains_key(&market.compatibility_event_id) {
                            continue;
                        }

                        let Some(up_asset_id) = parse_token_id(&market.up_token) else {
                            warn!(
                                market_id = %market.descriptor.market_id,
                                token_id = %market.up_token,
                                "Skipping discovered market with invalid up token id"
                            );
                            continue;
                        };
                        let Some(down_asset_id) = parse_token_id(&market.down_token) else {
                            warn!(
                                market_id = %market.descriptor.market_id,
                                token_id = %market.down_token,
                                "Skipping discovered market with invalid down token id"
                            );
                            continue;
                        };

                        if subscribed_tokens.insert(market.up_token.clone()) {
                            new_tokens.push(up_asset_id);
                        }
                        if subscribed_tokens.insert(market.down_token.clone()) {
                            new_tokens.push(down_asset_id);
                        }

                        let end_time = market.end_time.clone();
                        let window_secs = market.window_secs;
                        let price_to_beat = market.price_to_beat.clone();

                        tracked.insert(
                            market.compatibility_event_id.clone(),
                            TrackedEvent { end_time },
                        );

                        let _ = tx.send(MarketUpdate::EventDiscovered {
                            event_id: market.compatibility_event_id,
                            symbol: market.symbol,
                            up_token: market.up_token,
                            down_token: market.down_token,
                            end_time,
                            window_secs,
                            price_to_beat,
                            resolved_up_won: None,
                        });
                    }

                    if !new_tokens.is_empty() {
                        info!(
                            new_tokens = new_tokens.len(),
                            total_tracked = tracked.len(),
                            "Discovered new markets, subscribing to quotes",
                        );
                        let handle = spawn_quote_feed(tx.clone(), new_tokens, pool.clone());
                        quote_handles.push(handle);
                    } else {
                        debug!(tracked = tracked.len(), "Scanner poll: no new markets");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Gamma markets query failed, will retry");
                }
            }

            if should_refresh_sports_catalog(now, last_sports_refresh, pool.is_some()) {
                refresh_sports_catalog(&client, pool.as_ref()).await;
                last_sports_refresh = Some(now);
            }

            quote_handles.retain(|h| !h.is_finished());

            tokio::time::sleep(std::time::Duration::from_secs(SCAN_INTERVAL_SECS)).await;
        }
    })
}

fn expire_tracked_events(
    tx: &broadcast::Sender<MarketUpdate>,
    tracked: &mut HashMap<String, TrackedEvent>,
    now: DateTime<Utc>,
) {
    let expired: Vec<String> = tracked
        .iter()
        .filter(|(_, event)| event.end_time <= now)
        .map(|(event_id, _)| event_id.clone())
        .collect();

    for event_id in expired {
        let end_time = tracked
            .remove(&event_id)
            .map(|event| event.end_time)
            .unwrap_or(now);
        let _ = tx.send(MarketUpdate::EventExpired {
            event_id,
            end_time,
            resolved_up_won: None,
        });
    }
}

fn should_refresh_sports_catalog(
    now: DateTime<Utc>,
    last_refresh: Option<DateTime<Utc>>,
    persistence_enabled: bool,
) -> bool {
    persistence_enabled
        && last_refresh
            .map(|ts| now - ts >= Duration::seconds(SPORTS_DISCOVERY_REFRESH_SECS))
            .unwrap_or(true)
}

async fn refresh_sports_catalog(client: &GammaClient, pool: Option<&PgPool>) {
    let Some(pool) = pool else {
        return;
    };

    match discover_sports_markets(client, SPORTS_DISCOVERY_LIMIT).await {
        Ok(markets) => {
            if markets.is_empty() {
                debug!("Sports discovery refresh returned no active markets");
                return;
            }

            info!(
                discovered = markets.len(),
                "Refreshing sports market catalog",
            );

            for market in markets {
                upsert_market_catalog(
                    pool,
                    &market.descriptor,
                    market.raw_event.clone(),
                    market.raw_market.clone(),
                )
                .await;
            }
        }
        Err(error) => {
            warn!(error = %error, "Sports discovery refresh failed");
        }
    }
}

async fn persist_discovered_crypto_market(pool: Option<&PgPool>, market: &DiscoveredCryptoMarket) {
    let Some(pool) = pool else {
        return;
    };

    upsert_market_catalog(
        pool,
        &market.descriptor,
        market.raw_event.clone(),
        market.raw_market.clone(),
    )
    .await;

    upsert_market_metadata(
        pool,
        &market.compatibility_event_id,
        &market.symbol,
        &market.up_token,
        &market.down_token,
        market.descriptor.start_time.clone(),
        market.end_time.clone(),
        market.price_to_beat.clone(),
    )
    .await;
}

fn parse_token_id(token_id: &str) -> Option<U256> {
    U256::from_str(token_id).ok()
}

async fn upsert_market_metadata(
    pool: &PgPool,
    market_slug: &str,
    symbol: &str,
    up_token: &str,
    down_token: &str,
    start_time: Option<DateTime<Utc>>,
    end_time: DateTime<Utc>,
    price_to_beat: Option<Decimal>,
) {
    let raw_market = serde_json::json!({
        "eventStartTime": start_time.as_ref().map(DateTime::to_rfc3339),
        "endDate": end_time.to_rfc3339(),
        "markets": [{
            "clobTokenIds": serde_json::json!([up_token, down_token]).to_string()
        }]
    });

    let result = sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (
            market_slug, symbol, start_time, end_time, price_to_beat, raw_market
        ) VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (market_slug) DO UPDATE
            SET symbol        = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
                start_time    = COALESCE(pm_market_metadata.start_time, EXCLUDED.start_time),
                end_time      = EXCLUDED.end_time,
                price_to_beat = COALESCE(EXCLUDED.price_to_beat, pm_market_metadata.price_to_beat),
                raw_market    = EXCLUDED.raw_market,
                updated_at    = NOW()
        "#,
    )
    .bind(market_slug)
    .bind(symbol)
    .bind(start_time)
    .bind(end_time)
    .bind(price_to_beat)
    .bind(raw_market)
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            market_slug,
            error = %e,
            "Failed to upsert market metadata"
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::{parse_token_id, should_refresh_sports_catalog};

    #[test]
    fn token_id_parser_accepts_decimal_strings() {
        assert!(
            parse_token_id(
                "27239049953613250678046988034203198692578441444398010699401021233149338414941"
            )
            .is_some()
        );
        assert!(parse_token_id("not-a-token").is_none());
    }

    #[test]
    fn sports_refresh_requires_pool_and_interval() {
        let now = Utc.with_ymd_and_hms(2026, 4, 6, 0, 5, 0).unwrap();
        assert!(!should_refresh_sports_catalog(now, None, false));
        assert!(should_refresh_sports_catalog(now, None, true));
        assert!(!should_refresh_sports_catalog(
            now,
            Some(now - Duration::seconds(10)),
            true
        ));
        assert!(should_refresh_sports_catalog(
            now,
            Some(now - Duration::seconds(600)),
            true
        ));
    }
}
