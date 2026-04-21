//! Polymarket CLOB quote collector — WebSocket-based continuous orderbook subscription.
//!
//! Subscribes to Polymarket CLOB WebSocket for active 5m/15m markets and persists
//! raw orderbook snapshots to `clob_orderbook_snapshots`, plus derived
//! best_bid/best_ask rows to `clob_quote_ticks`.
//!
//! This is a standalone data collection mode, separate from the strategy runtime.
//! Run with: `ploy-runner collect-quotes --symbols BTCUSDT,ETHUSDT,SOLUSDT --timeframe 5m`

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use polymarket_client_sdk::clob::ws::types::response::OrderBookLevel;
use polymarket_client_sdk::clob::ws::{BookUpdate, Client as ClobWsClient};
use polymarket_client_sdk::gamma::types::request::MarketByIdRequest;
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::rtds::Client as RtdsClient;
use polymarket_client_sdk::ws::config::{Config as PolymarketWsConfig, ReconnectConfig};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::reference_prices::{
    latest_reference_price, market_symbol_to_chainlink_symbol, new_reference_price_registry,
    normalize_reference_symbol, upsert_reference_price, ReferenceAssetClass, ReferencePriceKey,
    ReferencePriceRegistry, ReferencePriceSnapshot, ReferencePriceSource,
};

/// Configuration for the quote collector.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    pub symbols: Vec<String>,
    pub timeframe: String,
    pub refresh_interval_secs: u64,
}

/// Metadata for a tracked token.
#[derive(Debug, Clone)]
struct TokenMetadata {
    slug: String,
    symbol: String,
    side: String,
    end_time: DateTime<Utc>,
}

/// Quote collector state.
pub struct QuoteCollector {
    config: CollectorConfig,
    pool: PgPool,
    subscribed_tokens: Arc<RwLock<HashSet<String>>>,
    token_metadata: Arc<RwLock<HashMap<String, TokenMetadata>>>,
    stats: Arc<RwLock<CollectorStats>>,
    reference_prices: ReferencePriceRegistry,
}

#[derive(Debug, Default)]
struct CollectorStats {
    books_received: u64,
    snapshots_inserted: u64,
    quotes_inserted: u64,
    last_refresh: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct PersistedOrderBookLevel {
    price: String,
    size: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct SnapshotContext {
    slug: String,
    symbol: String,
    side: String,
    timeframe: String,
    collector: &'static str,
    end_time: String,
}

#[derive(Debug, Default)]
struct PersistResult {
    snapshot_inserted: bool,
    quote_inserted: bool,
}

#[derive(Debug, Deserialize)]
struct OfficialMarketSettlementPayload {
    closed: Option<bool>,
    #[serde(rename = "resolvedBy")]
    resolved_by: Option<String>,
    #[serde(rename = "umaResolutionStatus")]
    uma_resolution_status: Option<String>,
    outcomes: Option<String>,
    #[serde(rename = "outcomePrices")]
    outcome_prices: Option<String>,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OfficialTokenSettlement {
    token_id: String,
    outcome: &'static str,
    settled_price: Decimal,
}

enum OfficialMarketSettlementStatus {
    Closed(Vec<OfficialTokenSettlement>),
    Open,
    Unknown,
}

const POLYMARKET_CLOB_WS_ENDPOINT: &str = "wss://ws-subscriptions-clob.polymarket.com";
const POLYMARKET_RTDS_WS_ENDPOINT: &str = "wss://ws-live-data.polymarket.com";

fn is_tradeable_price(price: Decimal) -> bool {
    price > rust_decimal_macros::dec!(0.02) && price < rust_decimal_macros::dec!(0.98)
}

fn best_tradeable_bid<I>(prices: I) -> Option<Decimal>
where
    I: IntoIterator<Item = Decimal>,
{
    prices
        .into_iter()
        .filter(|price| is_tradeable_price(*price))
        .max()
}

fn best_tradeable_ask<I>(prices: I) -> Option<Decimal>
where
    I: IntoIterator<Item = Decimal>,
{
    prices
        .into_iter()
        .filter(|price| is_tradeable_price(*price))
        .min()
}

fn serialize_orderbook_levels(levels: &[OrderBookLevel]) -> String {
    let levels = levels
        .iter()
        .map(|level| PersistedOrderBookLevel {
            price: level.price.to_string(),
            size: level.size.to_string(),
        })
        .collect::<Vec<_>>();

    serde_json::to_string(&levels).expect("serializing persisted orderbook levels cannot fail")
}

fn bridge_sdk_json<T: Serialize>(value: Option<T>) -> Option<String> {
    value.and_then(|inner| serde_json::to_string(&inner).ok())
}

fn parse_json_string_array(raw: &str) -> Option<Vec<String>> {
    serde_json::from_str(raw).ok()
}

fn parse_json_decimal_array(raw: &str) -> Option<Vec<Decimal>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(raw).ok()?;
    values
        .into_iter()
        .map(|value| match value {
            serde_json::Value::String(s) => s.parse::<Decimal>().ok(),
            serde_json::Value::Number(n) => n.to_string().parse::<Decimal>().ok(),
            _ => None,
        })
        .collect()
}

fn parse_official_market_settlements(
    payload: &OfficialMarketSettlementPayload,
) -> Option<Vec<OfficialTokenSettlement>> {
    if !payload.closed.unwrap_or(false) {
        return None;
    }

    let token_ids = parse_json_string_array(payload.clob_token_ids.as_deref()?)?;
    let prices = parse_json_decimal_array(payload.outcome_prices.as_deref()?)?;
    let outcomes = payload
        .outcomes
        .as_deref()
        .and_then(parse_json_string_array)
        .unwrap_or_default();

    if token_ids.len() != prices.len() || token_ids.is_empty() {
        return None;
    }

    let mut settlements = Vec::with_capacity(token_ids.len());
    let mut winners = 0usize;
    let mut losers = 0usize;

    for (idx, (token_id, price)) in token_ids.into_iter().zip(prices.into_iter()).enumerate() {
        let normalized_token_id = normalize_token_id(&token_id);
        let outcome_name = outcomes
            .get(idx)
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();

        let (outcome, settled_price) = if price >= rust_decimal_macros::dec!(0.95) {
            winners += 1;
            ("winner", rust_decimal_macros::dec!(1.0))
        } else if price <= rust_decimal_macros::dec!(0.05) {
            losers += 1;
            ("loser", rust_decimal_macros::dec!(0.0))
        } else {
            return None;
        };

        if outcome_name.contains("down") || outcome_name.contains("no") {
            settlements.push(OfficialTokenSettlement {
                token_id: normalized_token_id,
                outcome,
                settled_price,
            });
        } else {
            settlements.push(OfficialTokenSettlement {
                token_id: normalized_token_id,
                outcome,
                settled_price,
            });
        }
    }

    if winners == 1 && losers == settlements.len().saturating_sub(1) {
        Some(settlements)
    } else {
        None
    }
}

async fn fetch_official_market_settlements(
    gamma: &GammaClient,
    market_id: &str,
) -> OfficialMarketSettlementStatus {
    let request = MarketByIdRequest::builder().id(market_id).build();
    let market = match gamma.market_by_id(&request).await {
        Ok(market) => market,
        Err(_) => return OfficialMarketSettlementStatus::Unknown,
    };

    if !market.closed.unwrap_or(false) {
        return OfficialMarketSettlementStatus::Open;
    }

    // Bridge SDK types to local payload format for existing parse logic
    let payload = OfficialMarketSettlementPayload {
        closed: market.closed,
        resolved_by: market.resolved_by,
        uma_resolution_status: market.uma_resolution_status,
        outcomes: market
            .outcomes
            .map(|v| serde_json::to_string(&v).unwrap_or_default()),
        outcome_prices: market
            .outcome_prices
            .map(|v| serde_json::to_string(&v).unwrap_or_default()),
        clob_token_ids: bridge_sdk_json(market.clob_token_ids),
    };

    match parse_official_market_settlements(&payload) {
        Some(settlements) => OfficialMarketSettlementStatus::Closed(settlements),
        None => OfficialMarketSettlementStatus::Unknown,
    }
}

async fn clear_unofficial_market_settlements(
    pool: &PgPool,
    market_id: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE pm_token_settlements
        SET settled_price = NULL,
            outcome = NULL,
            resolved = FALSE,
            resolved_at = NULL,
            fetched_at = NOW()
        WHERE market_slug = $1
          AND resolved = TRUE
        "#,
    )
    .bind(market_id)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

fn snapshot_context(meta: &TokenMetadata, timeframe: &str) -> String {
    let context = SnapshotContext {
        slug: meta.slug.clone(),
        symbol: meta.symbol.clone(),
        side: meta.side.clone(),
        timeframe: timeframe.to_string(),
        collector: "collect-quotes",
        end_time: meta.end_time.to_rfc3339(),
    };

    serde_json::to_string(&context).expect("serializing snapshot context cannot fail")
}

fn book_timestamp(timestamp_ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(timestamp_ms)
}

fn collector_market_data_ws_config() -> PolymarketWsConfig {
    let mut config = PolymarketWsConfig::default();
    // The collector only needs a healthy market-data stream, not a tightly policed
    // heartbeat. A wider window reduces needless reconnect churn on transient stalls.
    config.heartbeat_interval = StdDuration::from_secs(15);
    config.heartbeat_timeout = StdDuration::from_secs(45);
    config.reconnect = ReconnectConfig::default();
    config
}

impl QuoteCollector {
    /// Create a new quote collector.
    pub fn new(config: CollectorConfig, pool: PgPool) -> Self {
        Self {
            config,
            pool,
            subscribed_tokens: Arc::new(RwLock::new(HashSet::new())),
            token_metadata: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(CollectorStats::default())),
            reference_prices: new_reference_price_registry(),
        }
    }

    /// Run the collector loop.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            symbols = ?self.config.symbols,
            timeframe = %self.config.timeframe,
            "Starting Polymarket quote collector"
        );

        // Spawn Chainlink price feed
        let _chainlink_handle = self.spawn_chainlink_feed();

        // Spawn price_to_beat updater
        let _price_updater_handle = self.spawn_price_to_beat_updater();

        // Spawn settlement collector (market_resolved WS events)
        let _settlement_handle = self.spawn_settlement_collector();

        loop {
            // Refresh subscriptions and get current token list
            self.refresh_subscriptions().await?;

            let token_ids = {
                let subscribed = self.subscribed_tokens.read().await;
                subscribed.iter().cloned().collect::<Vec<_>>()
            };

            if token_ids.is_empty() {
                info!("No active tokens to subscribe, waiting for next refresh...");
                sleep(StdDuration::from_secs(self.config.refresh_interval_secs)).await;
                continue;
            }

            // Convert token IDs to U256
            let asset_ids: Vec<polymarket_client_sdk::types::U256> =
                token_ids.iter().filter_map(|id| id.parse().ok()).collect();

            if asset_ids.is_empty() {
                warn!("No valid U256 token IDs found");
                sleep(StdDuration::from_secs(self.config.refresh_interval_secs)).await;
                continue;
            }

            info!(tokens = asset_ids.len(), "Subscribing to orderbook updates");

            // Create WebSocket client and subscribe
            let client = ClobWsClient::new(
                POLYMARKET_CLOB_WS_ENDPOINT,
                collector_market_data_ws_config(),
            )
            .expect("collector WebSocket config should be valid");
            let stream = match client.subscribe_orderbook(asset_ids) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to subscribe to orderbook");
                    sleep(StdDuration::from_secs(5)).await;
                    continue;
                }
            };

            let mut stream = Box::pin(stream);

            info!("WebSocket connected, listening for quotes...");

            // Listen for quotes until refresh interval
            let refresh_deadline = tokio::time::Instant::now()
                + StdDuration::from_secs(self.config.refresh_interval_secs);

            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(refresh_deadline) => {
                        info!("Refresh interval reached, reconnecting...");
                        break;
                    }
                    result = stream.next() => {
                        match result {
                            Some(Ok(book)) => {
                                let token_id = book.asset_id.to_string();

                                // Log first few messages for debugging
                                {
                                    let s = self.stats.read().await;
                                    if s.books_received < 10 {
                                        info!(
                                            token = %token_id,
                                            bids = book.bids.len(),
                                            asks = book.asks.len(),
                                            "Received orderbook update"
                                        );
                                    }
                                }

                                // Check if we're tracking this token
                                let is_tracked = {
                                    let sub = self.subscribed_tokens.read().await;
                                    sub.contains(&token_id)
                                };

                                if !is_tracked {
                                    continue;
                                }

                                {
                                    let mut s = self.stats.write().await;
                                    s.books_received += 1;
                                }

                                // Select the actual best tradeable prices, not just the first
                                // non-placeholder level returned by the SDK.
                                let real_bid =
                                    best_tradeable_bid(book.bids.iter().map(|bid| bid.price));
                                let real_ask =
                                    best_tradeable_ask(book.asks.iter().map(|ask| ask.price));

                                let (best_bid, best_ask) = (real_bid, real_ask);

                                // Get metadata
                                let meta = {
                                    let m = self.token_metadata.read().await;
                                    m.get(&token_id).cloned()
                                };

                                if let Some(meta) = meta {
                                    match persist_book_update(
                                        &self.pool,
                                        &self.config.timeframe,
                                        &meta,
                                        &book,
                                        &token_id,
                                        best_bid,
                                        best_ask,
                                    )
                                    .await
                                    {
                                        Ok(result) => {
                                            let active_tokens = self.subscribed_tokens.read().await.len();

                                            let mut s = self.stats.write().await;
                                            s.snapshots_inserted += u64::from(result.snapshot_inserted);
                                            s.quotes_inserted += u64::from(result.quote_inserted);

                                            if s.books_received % 100 == 0 {
                                                info!(
                                                    books_received = s.books_received,
                                                    snapshots_inserted = s.snapshots_inserted,
                                                    quotes_inserted = s.quotes_inserted,
                                                    active_tokens,
                                                    "Quote collector stats"
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            warn!(error = %e, token = %token_id, "Failed to persist book update");
                                        }
                                    }
                                } else {
                                    warn!(token = %token_id, "Received quote for unknown token");
                                }
                            }
                            Some(Err(e)) => {
                                warn!(error = %e, "WebSocket stream error");
                            }
                            None => {
                                warn!("WebSocket stream ended, reconnecting...");
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Refresh market subscriptions by querying database for active markets.
    async fn refresh_subscriptions(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Refreshing market subscriptions...");

        let markets = self.get_active_markets().await?;
        info!(markets = markets.len(), "Found active markets");

        let mut new_tokens = HashSet::new();
        let mut new_metadata = HashMap::new();

        for market in markets {
            for (side, token_id) in [("UP", market.up_token), ("DOWN", market.down_token)] {
                new_tokens.insert(token_id.clone());
                new_metadata.insert(
                    token_id,
                    TokenMetadata {
                        slug: market.slug.clone(),
                        symbol: market.symbol.clone(),
                        side: side.to_string(),
                        end_time: market.end_time,
                    },
                );
            }
        }

        let mut subscribed = self.subscribed_tokens.write().await;
        let mut metadata = self.token_metadata.write().await;

        let added = new_tokens.difference(&*subscribed).count();
        let removed = subscribed.difference(&new_tokens).count();

        if added > 0 {
            info!(added, "Adding new token subscriptions");
        }
        if removed > 0 {
            info!(removed, "Removing expired token subscriptions");
        }

        *subscribed = new_tokens;
        *metadata = new_metadata;

        let mut stats = self.stats.write().await;
        stats.last_refresh = Some(Utc::now());

        info!(
            active_tokens = subscribed.len(),
            "Subscription refresh complete"
        );

        Ok(())
    }

    /// Query database for active markets.
    async fn get_active_markets(&self) -> Result<Vec<ActiveMarket>, sqlx::Error> {
        // Build IN clause dynamically
        let placeholders: Vec<String> = (1..=self.config.symbols.len())
            .map(|i| format!("${}", i))
            .collect();
        let in_clause = placeholders.join(", ");

        // Query by symbol and time window only — market slugs may be numeric IDs
        // or human-readable strings depending on the Polymarket market type.
        let query = format!(
            r#"
            SELECT
                market_slug,
                symbol,
                start_time,
                end_time,
                ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text AS up_token,
                ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text AS down_token
            FROM pm_market_metadata
            WHERE symbol IN ({})
              AND end_time > NOW()
              AND start_time < NOW() + INTERVAL '2 hours'
              AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            ORDER BY start_time
            "#,
            in_clause,
        );

        info!(symbols = ?self.config.symbols, "Querying active markets");

        let mut q = sqlx::query_as::<_, ActiveMarketRow>(&query);
        for symbol in &self.config.symbols {
            q = q.bind(symbol);
        }

        let rows = q.fetch_all(&self.pool).await?;

        let markets = rows
            .into_iter()
            .map(|row| ActiveMarket {
                slug: row.market_slug,
                symbol: row.symbol,
                _start_time: row.start_time,
                end_time: row.end_time,
                up_token: normalize_token_id(&row.up_token),
                down_token: normalize_token_id(&row.down_token),
            })
            .collect();

        Ok(markets)
    }

    /// Spawn settlement collector — polls /midpoint for recently expired events
    /// and persists settlement outcomes to pm_token_settlements.
    fn spawn_settlement_collector(&self) -> tokio::task::JoinHandle<()> {
        let pool = self.pool.clone();

        tokio::spawn(async move {
            let gamma = GammaClient::default();
            let mut settled_count = 0u64;

            loop {
                // Poll every 60 seconds
                tokio::time::sleep(StdDuration::from_secs(60)).await;

                // Query tokens from events that expired in the last 2 hours
                // but don't yet have settlement data. This catches all recently
                // expired events regardless of whether they were in the active list.
                let rows: Vec<(String, String, String)> = sqlx::query_as(
                    r#"
                    SELECT
                        market_slug,
                        trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0)) as up_token,
                        trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1)) as down_token
                    FROM pm_market_metadata
                    WHERE end_time >= NOW() - INTERVAL '2 hours'
                      AND end_time < NOW()
                      AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
                    LIMIT 200
                    "#,
                )
                .fetch_all(&pool)
                .await
                .unwrap_or_default();

                if rows.is_empty() {
                    continue;
                }

                info!(
                    pending = rows.len(),
                    "Settlement collector checking expired events"
                );

                for (market_id, up_token, down_token) in &rows {
                    let settlements = match fetch_official_market_settlements(&gamma, market_id)
                        .await
                    {
                        OfficialMarketSettlementStatus::Closed(settlements) => settlements,
                        OfficialMarketSettlementStatus::Open => {
                            match clear_unofficial_market_settlements(&pool, market_id).await {
                                Ok(rows) if rows > 0 => {
                                    warn!(
                                        market_id = %market_id,
                                        cleared_rows = rows,
                                        "Cleared previously resolved settlement rows for market still open in official API"
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    warn!(error = %e, market_id = %market_id, "Failed to clear stale settlement rows")
                                }
                            }
                            continue;
                        }
                        OfficialMarketSettlementStatus::Unknown => {
                            warn!(
                                market_id = %market_id,
                                "Official settlement API unavailable or malformed; leaving existing settlement rows unchanged"
                            );
                            continue;
                        }
                    };

                    for settlement in settlements {
                        if settlement.token_id != normalize_token_id(up_token)
                            && settlement.token_id != normalize_token_id(down_token)
                        {
                            continue;
                        }

                        let result = sqlx::query(
                            r#"
                            INSERT INTO pm_token_settlements (
                                token_id, market_slug, outcome,
                                settled_price, resolved, resolved_at, fetched_at
                            ) VALUES ($1, $2, $3, $4, true, NOW(), NOW())
                            ON CONFLICT (token_id) DO UPDATE SET
                                settled_price = EXCLUDED.settled_price,
                                outcome = EXCLUDED.outcome,
                                resolved = true,
                                resolved_at = COALESCE(pm_token_settlements.resolved_at, NOW()),
                                fetched_at = NOW()
                            WHERE pm_token_settlements.resolved = false
                               OR pm_token_settlements.settled_price IS DISTINCT FROM EXCLUDED.settled_price
                               OR pm_token_settlements.outcome IS DISTINCT FROM EXCLUDED.outcome
                            "#,
                        )
                        .bind(&settlement.token_id)
                        .bind(market_id)
                        .bind(settlement.outcome)
                        .bind(settlement.settled_price)
                        .execute(&pool)
                        .await;

                        match result {
                            Ok(r) if r.rows_affected() > 0 => {
                                settled_count += 1;
                                if settled_count % 10 == 0 || settled_count <= 5 {
                                    info!(
                                        token = %&settlement.token_id[..12.min(settlement.token_id.len())],
                                        market_id = %market_id,
                                        outcome = settlement.outcome,
                                        settled_price = %settlement.settled_price,
                                        total = settled_count,
                                        "Official settlement recorded"
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!(error = %e, market_id = %market_id, "Failed to record official settlement");
                            }
                        }
                    }

                    // Rate limit: ~2 req/sec
                    tokio::time::sleep(StdDuration::from_millis(500)).await;
                }
            }
        })
    }

    /// Spawn Chainlink price feed subscriber.
    fn spawn_chainlink_feed(&self) -> tokio::task::JoinHandle<()> {
        let registry = self.reference_prices.clone();
        let symbols = self.config.symbols.clone();

        tokio::spawn(async move {
            let symbols_chainlink: Vec<String> = symbols
                .iter()
                .map(|s| market_symbol_to_chainlink_symbol(s))
                .collect();

            info!(symbols = ?symbols_chainlink, "Starting Chainlink price feed");

            let client = RtdsClient::new(
                POLYMARKET_RTDS_WS_ENDPOINT,
                collector_market_data_ws_config(),
            )
            .expect("collector RTDS WebSocket config should be valid");
            let stream = match client.subscribe_chainlink_prices(None) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Failed to subscribe to Chainlink prices");
                    return;
                }
            };

            let mut stream = Box::pin(stream);
            let mut price_count = 0_u64;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(chainlink_price) => {
                        if !symbols_chainlink.contains(&chainlink_price.symbol) {
                            continue;
                        }

                        let ts = DateTime::from_timestamp_millis(chainlink_price.timestamp)
                            .unwrap_or_else(Utc::now);

                        upsert_reference_price(
                            &registry,
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

                        price_count += 1;
                        if price_count % 100 == 0 {
                            info!(prices = price_count, "Chainlink prices cached");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Chainlink price stream error");
                    }
                }
            }

            warn!("Chainlink price feed ended");
        })
    }

    /// Spawn price_to_beat updater task.
    fn spawn_price_to_beat_updater(&self) -> tokio::task::JoinHandle<()> {
        let pool = self.pool.clone();
        let registry = self.reference_prices.clone();
        let symbols = self.config.symbols.clone();

        tokio::spawn(async move {
            loop {
                sleep(StdDuration::from_secs(10)).await;

                let now = Utc::now();

                // Two passes:
                // 1. Backfill: events that started in the past 10 minutes with no price_to_beat
                //    Use the Chainlink price closest to their start_time (best available).
                // 2. Upcoming: events starting in the next 60 seconds — write at exact start_time.
                let window_start = now - chrono::Duration::minutes(10);
                let window_end = now + chrono::Duration::seconds(60);

                let query = format!(
                    r#"
                    SELECT market_slug, symbol, start_time
                    FROM pm_market_metadata
                    WHERE symbol IN ({})
                      AND start_time >= ${}
                      AND start_time <= ${}
                      AND price_to_beat IS NULL
                    ORDER BY start_time
                    LIMIT 50
                    "#,
                    (1..=symbols.len())
                        .map(|i| format!("${}", i))
                        .collect::<Vec<_>>()
                        .join(", "),
                    symbols.len() + 1,
                    symbols.len() + 2,
                );

                let mut q = sqlx::query_as::<_, (String, String, DateTime<Utc>)>(&query);
                for symbol in &symbols {
                    q = q.bind(symbol);
                }
                q = q.bind(window_start);
                q = q.bind(window_end);

                let markets = match q.fetch_all(&pool).await {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "Failed to query markets for price_to_beat update");
                        continue;
                    }
                };

                if markets.is_empty() {
                    continue;
                }

                info!(
                    markets = markets.len(),
                    "Found markets needing price_to_beat"
                );

                for (slug, symbol, start_time) in markets {
                    // For upcoming events: wait until start_time to capture the exact open price.
                    // For past events (backfill): write immediately with current Chainlink price.
                    let wait_ms = (start_time - Utc::now()).num_milliseconds();
                    if wait_ms > 0 {
                        sleep(StdDuration::from_millis(wait_ms as u64)).await;
                    }

                    // Get Chainlink price
                    let chainlink_symbol = market_symbol_to_chainlink_symbol(&symbol);
                    let price = latest_reference_price(
                        &registry,
                        ReferencePriceSource::Chainlink,
                        &chainlink_symbol,
                    )
                    .await
                    .map(|snapshot| snapshot.value);

                    if let Some(price) = price {
                        // Update database
                        let result = sqlx::query(
                            "UPDATE pm_market_metadata SET price_to_beat = $1 WHERE market_slug = $2"
                        )
                        .bind(price)
                        .bind(&slug)
                        .execute(&pool)
                        .await;

                        match result {
                            Ok(_) => {
                                info!(
                                    slug = %slug,
                                    symbol = %symbol,
                                    price = %price,
                                    "Updated price_to_beat"
                                );
                            }
                            Err(e) => {
                                warn!(error = %e, slug = %slug, "Failed to update price_to_beat");
                            }
                        }
                    } else {
                        warn!(
                            slug = %slug,
                            symbol = %symbol,
                            chainlink_symbol = %chainlink_symbol,
                            "No Chainlink price available for price_to_beat"
                        );
                    }
                }
            }
        })
    }
}

/// Active market from database.
#[derive(Debug)]
struct ActiveMarket {
    slug: String,
    symbol: String,
    _start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    up_token: String,
    down_token: String,
}

/// Database row for active market query.
#[derive(sqlx::FromRow)]
struct ActiveMarketRow {
    market_slug: String,
    symbol: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    up_token: String,
    down_token: String,
}

/// Persist a raw orderbook snapshot and its derived top-of-book quote.
async fn persist_book_update(
    pool: &PgPool,
    timeframe: &str,
    meta: &TokenMetadata,
    book: &BookUpdate,
    token_id: &str,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
) -> Result<PersistResult, sqlx::Error> {
    let received_at = Utc::now();
    let bids_json = serialize_orderbook_levels(&book.bids);
    let asks_json = serialize_orderbook_levels(&book.asks);
    let context_json = snapshot_context(meta, timeframe);

    let mut tx = pool.begin().await?;

    let snapshot_rows = sqlx::query(
        r#"
        INSERT INTO clob_orderbook_snapshots (
            domain, token_id, market, bids, asks,
            book_timestamp, hash, source, context, received_at
        ) VALUES ($1, $2, $3, $4::jsonb, $5::jsonb, $6, $7, $8, $9::jsonb, $10)
        "#,
    )
    .bind("Crypto")
    .bind(token_id)
    .bind(book.market.to_string())
    .bind(bids_json)
    .bind(asks_json)
    .bind(book_timestamp(book.timestamp))
    .bind(book.hash.clone())
    .bind("polymarket_ws_collector")
    .bind(context_json)
    .bind(received_at)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let quote_rows = if best_bid.is_some() || best_ask.is_some() {
        sqlx::query(
            r#"
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask,
            received_at, source, domain
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT DO NOTHING
        "#,
        )
        .bind(token_id)
        .bind(&meta.side)
        .bind(best_bid)
        .bind(best_ask)
        .bind(received_at)
        .bind("polymarket_ws_collector")
        .bind("Crypto")
        .execute(&mut *tx)
        .await?
        .rows_affected()
    } else {
        0
    };

    tx.commit().await?;

    Ok(PersistResult {
        snapshot_inserted: snapshot_rows > 0,
        quote_inserted: quote_rows > 0,
    })
}

/// Normalize token ID from hex (0x...) to decimal string.
///
/// Polymarket CLOB API returns token IDs as decimal strings, but database
/// may store them as hex. This function ensures consistent decimal format.
fn normalize_token_id(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');

    // If it's hex (0x prefix), convert to decimal
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        if let Some(decimal) = hex_to_decimal_string(hex) {
            return decimal;
        }
    }

    // Otherwise return as-is
    trimmed.to_string()
}

/// Convert hex string to decimal string without external dependencies.
fn hex_to_decimal_string(hex: &str) -> Option<String> {
    if hex.is_empty() {
        return None;
    }

    let mut digits = vec![0_u8];

    for ch in hex.chars() {
        let value = ch.to_digit(16)? as u32;
        let mut carry = value;

        for digit in &mut digits {
            let next = (*digit as u32) * 16 + carry;
            *digit = (next % 10) as u8;
            carry = next / 10;
        }

        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }

    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }

    Some(
        digits
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + *digit))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        best_tradeable_ask, best_tradeable_bid, book_timestamp, bridge_sdk_json,
        collector_market_data_ws_config, hex_to_decimal_string, normalize_token_id,
        parse_official_market_settlements, serialize_orderbook_levels, snapshot_context,
        OfficialMarketSettlementPayload, OrderBookLevel, TokenMetadata,
    };
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use std::time::Duration;

    #[test]
    fn normalize_token_id_converts_hex_to_decimal() {
        let raw = "\"0x3c38c18444ab803acea0d4de7bcdecae7f0f8ddbcd0466e3323d1cb9e04b6f5d\"";
        let normalized = normalize_token_id(raw);
        assert_eq!(
            normalized,
            "27239049953613250678046988034203198692578441444398010699401021233149338414941"
        );
    }

    #[test]
    fn normalize_token_id_keeps_decimal_ids() {
        let raw = "35165169860573247111698076491591023728797123337726915178028774493274622598566";
        assert_eq!(normalize_token_id(raw), raw);
    }

    #[test]
    fn hex_to_decimal_string_rejects_invalid_hex() {
        assert_eq!(hex_to_decimal_string("xyz"), None);
    }

    #[test]
    fn best_tradeable_bid_skips_placeholders_and_takes_highest_level() {
        let prices = vec![dec!(0.01), dec!(0.45), dec!(0.62), dec!(0.99)];
        assert_eq!(best_tradeable_bid(prices), Some(dec!(0.62)));
    }

    #[test]
    fn best_tradeable_ask_skips_placeholders_and_takes_lowest_level() {
        let prices = vec![dec!(0.99), dec!(0.54), dec!(0.31), dec!(0.01)];
        assert_eq!(best_tradeable_ask(prices), Some(dec!(0.31)));
    }

    #[test]
    fn best_tradeable_prices_return_none_when_only_placeholders_exist() {
        let prices = vec![dec!(0.01), dec!(0.02), dec!(0.98), dec!(0.99)];
        assert_eq!(best_tradeable_bid(prices.clone()), None);
        assert_eq!(best_tradeable_ask(prices), None);
    }

    #[test]
    fn serialize_orderbook_levels_preserves_price_and_size_strings() {
        let levels = vec![
            OrderBookLevel::builder()
                .price(dec!(0.45))
                .size(dec!(12.5))
                .build(),
            OrderBookLevel::builder()
                .price(dec!(0.44))
                .size(dec!(8))
                .build(),
        ];

        assert_eq!(
            serialize_orderbook_levels(&levels),
            r#"[{"price":"0.45","size":"12.5"},{"price":"0.44","size":"8"}]"#
        );
    }

    #[test]
    fn snapshot_context_captures_token_metadata_and_timeframe() {
        let meta = TokenMetadata {
            slug: "btc-updown-5m-123".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: "UP".to_string(),
            end_time: Utc.with_ymd_and_hms(2026, 4, 4, 4, 5, 0).unwrap(),
        };

        assert_eq!(
            snapshot_context(&meta, "5m"),
            r#"{"slug":"btc-updown-5m-123","symbol":"BTCUSDT","side":"UP","timeframe":"5m","collector":"collect-quotes","end_time":"2026-04-04T04:05:00+00:00"}"#
        );
    }

    #[test]
    fn book_timestamp_converts_millis_to_utc_datetime() {
        let timestamp = book_timestamp(1_712_205_600_123).unwrap();
        assert_eq!(timestamp.to_rfc3339(), "2024-04-04T04:40:00.123+00:00");
    }

    #[test]
    fn collector_market_data_uses_relaxed_ws_heartbeat_settings() {
        let config = collector_market_data_ws_config();
        assert_eq!(config.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(config.heartbeat_timeout, Duration::from_secs(45));
        assert!(config.reconnect.max_attempts.is_none());
    }

    #[test]
    fn parse_official_market_settlements_returns_binary_winner_and_loser() {
        let payload = OfficialMarketSettlementPayload {
            closed: Some(true),
            resolved_by: Some("oracle".to_string()),
            uma_resolution_status: Some("resolved".to_string()),
            outcomes: Some(r#"["Up","Down"]"#.to_string()),
            outcome_prices: Some(r#"["1","0"]"#.to_string()),
            clob_token_ids: Some(r#"["123","456"]"#.to_string()),
        };

        let settlements = parse_official_market_settlements(&payload).expect("official settlement");
        assert_eq!(settlements.len(), 2);
        assert_eq!(settlements[0].token_id, "123");
        assert_eq!(settlements[0].outcome, "winner");
        assert_eq!(settlements[0].settled_price, dec!(1.0));
        assert_eq!(settlements[1].token_id, "456");
        assert_eq!(settlements[1].outcome, "loser");
        assert_eq!(settlements[1].settled_price, dec!(0.0));
    }

    #[test]
    fn parse_official_market_settlements_rejects_open_markets() {
        let payload = OfficialMarketSettlementPayload {
            closed: Some(false),
            resolved_by: None,
            uma_resolution_status: None,
            outcomes: Some(r#"["Up","Down"]"#.to_string()),
            outcome_prices: Some(r#"["0.5","0.5"]"#.to_string()),
            clob_token_ids: Some(r#"["123","456"]"#.to_string()),
        };

        assert!(parse_official_market_settlements(&payload).is_none());
    }

    #[test]
    fn bridged_clob_token_ids_stay_json_for_official_settlement_parser() {
        let bridged = bridge_sdk_json(Some(vec!["123".to_string(), "456".to_string()]))
            .expect("bridged clob token ids");
        let token_ids: Vec<String> =
            serde_json::from_str(&bridged).expect("bridge should preserve a JSON array");
        assert_eq!(token_ids, vec!["123", "456"]);

        let payload = OfficialMarketSettlementPayload {
            closed: Some(true),
            resolved_by: Some("oracle".to_string()),
            uma_resolution_status: Some("resolved".to_string()),
            outcomes: Some(r#"["Up","Down"]"#.to_string()),
            outcome_prices: Some(r#"["1","0"]"#.to_string()),
            clob_token_ids: Some(bridged),
        };

        let settlements = parse_official_market_settlements(&payload).expect("official settlement");
        assert_eq!(settlements.len(), 2);
        assert_eq!(settlements[0].token_id, "123");
        assert_eq!(settlements[1].token_id, "456");
    }
}
