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
use ploy_market_contracts::MarketUpdate;
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
use crate::reference_prices::{
    ReferencePriceRegistry, ReferencePriceSource, latest_reference_price,
};

const SCAN_INTERVAL_SECS: u64 = 30;
const SPORTS_DISCOVERY_REFRESH_SECS: i64 = 300;
const SPORTS_DISCOVERY_LIMIT: i32 = 500;
/// How far back to look for open positions that need recovery on startup.
const RECOVERY_LOOKBACK_HOURS: i64 = 48;

struct TrackedEvent {
    end_time: chrono::DateTime<Utc>,
    symbol: String,
    price_to_beat: Option<Decimal>,
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

        // On startup, recover any open positions from events that expired while
        // the runner was offline. These will never receive a new EventExpired from
        // the live scanner, so we emit them now with official settlement outcomes.
        if let Some(ref db) = pool {
            recover_expired_open_positions(&tx, db).await;
            recover_pending_open_positions(tx.clone(), db, &mut tracked, &mut subscribed_tokens, &mut quote_handles).await;
        }

        loop {
            let now = Utc::now();

            expire_tracked_events(&tx, &mut tracked, &reference_prices, now).await;

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
                        let window_secs = market.market_window_secs;
                        let price_to_beat = market.price_to_beat.clone();

                        tracked.insert(
                            market.compatibility_event_id.clone(),
                            TrackedEvent {
                                end_time,
                                symbol: market.symbol.clone(),
                                price_to_beat: price_to_beat.clone(),
                            },
                        );

                        let _ = tx.send(MarketUpdate::EventDiscovered {
                            event_id: Arc::from(market.compatibility_event_id.as_str()),
                            symbol: Arc::from(market.symbol.as_str()),
                            up_token: Arc::from(market.up_token.as_str()),
                            down_token: Arc::from(market.down_token.as_str()),
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

/// On startup, find events that expired while the runner was offline and still
/// have open positions in `strategy_runtime_fills`. Emit `EventExpired` with
/// the official settlement outcome from `pm_token_settlements` so the strategy
/// can close those positions immediately.
async fn recover_expired_open_positions(tx: &broadcast::Sender<MarketUpdate>, pool: &PgPool) {
    let lookback = Utc::now() - Duration::hours(RECOVERY_LOOKBACK_HOURS);

    // Find open positions: BUY fills with no matching SELL, for events that
    // ended before now. Join pm_market_metadata for end_time and
    // pm_token_settlements for the official outcome.
    let rows: Vec<(String, String, String, DateTime<Utc>, Option<bool>)> = match sqlx::query_as(
        r#"
        SELECT DISTINCT
            f.event_id,
            f.token_id,
            COALESCE(f.symbol, '') AS symbol,
            COALESCE(m.end_time, NOW() - INTERVAL '1 second') AS end_time,
            CASE
                WHEN s.resolved AND s.settled_price >= 0.99 THEN true
                WHEN s.resolved AND s.settled_price <= 0.01 THEN false
                ELSE NULL
            END AS resolved_up_won
        FROM strategy_runtime_fills f
        LEFT JOIN pm_market_metadata m ON m.market_slug = f.event_id
        LEFT JOIN pm_token_settlements s ON s.token_id = f.token_id
        WHERE f.fill_side = 'BUY'
          AND f.fill_timestamp >= $1
          AND COALESCE(m.end_time, NOW()) < NOW()
          AND NOT EXISTS (
              SELECT 1 FROM strategy_runtime_fills s2
              WHERE s2.token_id = f.token_id
                AND s2.fill_side = 'SELL'
                AND s2.intent_id LIKE 'settle_%'
          )
        "#,
    )
    .bind(lookback)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Failed to query open positions for recovery");
            return;
        }
    };

    if rows.is_empty() {
        debug!("Startup recovery: no expired open positions found");
        return;
    }

    info!(
        count = rows.len(),
        "Startup recovery: emitting EventExpired for expired open positions"
    );

    // Group by event_id — one EventExpired per event is enough.
    let mut seen_events: HashSet<String> = HashSet::new();
    for (event_id, _token_id, _symbol, end_time, resolved_up_won) in rows {
        if seen_events.insert(event_id.clone()) {
            info!(
                event_id = %event_id,
                end_time = %end_time,
                resolved_up_won = ?resolved_up_won,
                "Recovery: emitting EventExpired",
            );
            let _ = tx.send(MarketUpdate::EventExpired {
                event_id: Arc::from(event_id.as_str()),
                end_time,
                resolved_up_won,
            });
        }
    }
}

/// On startup, find events that have NOT yet expired but already have open
/// positions from a previous runner instance. Re-inject them into the scanner's
/// `tracked` map and emit `EventDiscovered` so the strategy can manage them
/// through to settlement.
async fn recover_pending_open_positions(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    pool: &PgPool,
    tracked: &mut HashMap<String, TrackedEvent>,
    subscribed_tokens: &mut HashSet<String>,
    quote_handles: &mut Vec<JoinHandle<()>>,
) {
    let lookback = Utc::now() - Duration::hours(RECOVERY_LOOKBACK_HOURS);

    let rows: Vec<(String, String, String, String, String, DateTime<Utc>, Option<Decimal>)> =
        match sqlx::query_as(
            r#"
            SELECT DISTINCT
                f.event_id,
                f.symbol,
                m.raw_market->'markets'->0->>'clobTokenIds' AS clob_token_ids_json,
                f.token_id,
                f.market_side,
                m.end_time,
                m.price_to_beat
            FROM strategy_runtime_fills f
            JOIN pm_market_metadata m ON m.market_slug = f.event_id
            WHERE f.fill_side = 'BUY'
              AND f.fill_timestamp >= $1
              AND m.end_time > NOW()
              AND NOT EXISTS (
                  SELECT 1 FROM strategy_runtime_fills s2
                  WHERE s2.token_id = f.token_id
                    AND s2.fill_side = 'SELL'
              )
            "#,
        )
        .bind(lookback)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "Failed to query pending open positions for recovery");
                return;
            }
        };

    if rows.is_empty() {
        debug!("Startup recovery: no pending (not-yet-expired) open positions found");
        return;
    }

    info!(
        count = rows.len(),
        "Startup recovery: re-injecting pending open positions into scanner"
    );

    let mut new_tokens: Vec<U256> = Vec::new();
    let mut seen_events: HashSet<String> = HashSet::new();

    for (event_id, symbol, clob_json, _token_id, _market_side, end_time, price_to_beat) in rows {
        if !seen_events.insert(event_id.clone()) {
            continue;
        }

        let (up_token, down_token) = match parse_clob_token_pair(&clob_json) {
            Some(pair) => pair,
            None => {
                warn!(event_id = %event_id, "Recovery: cannot parse clobTokenIds, skipping");
                continue;
            }
        };

        let Some(up_asset_id) = parse_token_id(&up_token) else { continue };
        let Some(down_asset_id) = parse_token_id(&down_token) else { continue };

        if subscribed_tokens.insert(up_token.clone()) {
            new_tokens.push(up_asset_id);
        }
        if subscribed_tokens.insert(down_token.clone()) {
            new_tokens.push(down_asset_id);
        }

        tracked.insert(event_id.clone(), TrackedEvent {
            end_time,
            symbol: symbol.clone(),
            price_to_beat: price_to_beat.clone(),
        });

        info!(
            event_id = %event_id,
            symbol = %symbol,
            end_time = %end_time,
            "Recovery: re-emitting EventDiscovered for pending position",
        );

        let _ = tx.send(MarketUpdate::EventDiscovered {
            event_id: Arc::from(event_id.as_str()),
            symbol: Arc::from(symbol.as_str()),
            up_token: Arc::from(up_token.as_str()),
            down_token: Arc::from(down_token.as_str()),
            end_time,
            window_secs: 300,
            price_to_beat,
            resolved_up_won: None,
        });
    }

    if !new_tokens.is_empty() {
        info!(
            tokens = new_tokens.len(),
            "Recovery: subscribing to quote feeds for pending positions"
        );
        let handle = spawn_quote_feed(tx.clone(), new_tokens, Some(pool.clone()));
        quote_handles.push(handle);
    }
}

fn parse_clob_token_pair(raw: &str) -> Option<(String, String)> {
    let parsed: Vec<String> = serde_json::from_str(raw).ok()?;
    if parsed.len() == 2 {
        Some((parsed[0].clone(), parsed[1].clone()))
    } else {
        None
    }
}

async fn expire_tracked_events(
    tx: &broadcast::Sender<MarketUpdate>,
    tracked: &mut HashMap<String, TrackedEvent>,
    reference_prices: &ReferencePriceRegistry,
    now: DateTime<Utc>,
) {
    let expired: Vec<String> = tracked
        .iter()
        .filter(|(_, event)| event.end_time <= now)
        .map(|(event_id, _)| event_id.clone())
        .collect();

    for event_id in expired {
        let Some(event) = tracked.remove(&event_id) else {
            continue;
        };
        let resolved_up_won = infer_expired_event_outcome(&event, reference_prices).await;
        let end_time = event.end_time;
        let _ = tx.send(MarketUpdate::EventExpired {
            event_id: Arc::from(event_id.as_str()),
            end_time,
            resolved_up_won,
        });
    }
}

async fn infer_expired_event_outcome(
    event: &TrackedEvent,
    reference_prices: &ReferencePriceRegistry,
) -> Option<bool> {
    let price_to_beat = event.price_to_beat?;
    for source in [
        ReferencePriceSource::Chainlink,
        ReferencePriceSource::Pyth,
        ReferencePriceSource::Binance,
    ] {
        if let Some(snapshot) =
            latest_reference_price(reference_prices, source, &event.symbol).await
        {
            return Some(snapshot.value >= price_to_beat);
        }
    }
    None
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
