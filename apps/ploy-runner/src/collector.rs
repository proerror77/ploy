//! Polymarket CLOB quote collector — WebSocket-based continuous orderbook subscription.
//!
//! Subscribes to Polymarket CLOB WebSocket for active 5m/15m markets and persists
//! best_bid/best_ask to `clob_quote_ticks` table.
//!
//! This is a standalone data collection mode, separate from the strategy runtime.
//! Run with: `ploy-runner collect-quotes --symbols BTCUSDT,ETHUSDT,SOLUSDT --timeframe 5m`

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use polymarket_client_sdk::clob::ws::Client as ClobWsClient;
use polymarket_client_sdk::rtds::Client as RtdsClient;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info, warn};

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
    chainlink_prices: Arc<RwLock<HashMap<String, (Decimal, DateTime<Utc>)>>>,
}

#[derive(Debug, Default)]
struct CollectorStats {
    quotes_received: u64,
    quotes_inserted: u64,
    last_refresh: Option<DateTime<Utc>>,
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
            chainlink_prices: Arc::new(RwLock::new(HashMap::new())),
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
        let chainlink_handle = self.spawn_chainlink_feed();

        // Spawn price_to_beat updater
        let price_updater_handle = self.spawn_price_to_beat_updater();

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
            let client = ClobWsClient::default();
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
                                let token_id = book.asset_id.to_string();  // Use asset_id, not market

                                // Log first few messages for debugging
                                {
                                    let s = self.stats.read().await;
                                    if s.quotes_received < 10 {
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

                                // Extract best bid/ask
                                let best_bid = book.bids.first().map(|b| b.price);
                                let best_ask = book.asks.first().map(|a| a.price);

                                if best_bid.is_none() && best_ask.is_none() {
                                    continue;
                                }

                                // Get metadata
                                let meta = {
                                    let m = self.token_metadata.read().await;
                                    m.get(&token_id).cloned()
                                };

                                if let Some(meta) = meta {
                                    // Insert quote
                                    if let Err(e) = insert_quote(
                                        &self.pool,
                                        &token_id,
                                        &meta.side,
                                        best_bid,
                                        best_ask,
                                    )
                                    .await
                                    {
                                        warn!(error = %e, token = %token_id, "Failed to insert quote");
                                    } else {
                                        let mut s = self.stats.write().await;
                                        s.quotes_received += 1;
                                        s.quotes_inserted += 1;

                                        if s.quotes_received % 100 == 0 {
                                            info!(
                                                received = s.quotes_received,
                                                inserted = s.quotes_inserted,
                                                active_tokens = self.subscribed_tokens.read().await.len(),
                                                "Quote stats"
                                            );
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
        let pattern = format!("%-updown-{}-%", self.config.timeframe);

        // Build IN clause dynamically
        let placeholders: Vec<String> = (1..=self.config.symbols.len())
            .map(|i| format!("${}", i))
            .collect();
        let in_clause = placeholders.join(", ");

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
              AND market_slug LIKE ${}
              AND end_time > NOW()
              AND start_time < NOW() + INTERVAL '2 hours'
              AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
            ORDER BY start_time
            "#,
            in_clause,
            self.config.symbols.len() + 1
        );

        info!(query = %query, symbols = ?self.config.symbols, pattern = %pattern, "Executing market query");

        let mut q = sqlx::query_as::<_, ActiveMarketRow>(&query);
        for symbol in &self.config.symbols {
            q = q.bind(symbol);
        }
        q = q.bind(&pattern);

        let rows = q.fetch_all(&self.pool).await?;

        let markets = rows
            .into_iter()
            .map(|row| ActiveMarket {
                slug: row.market_slug,
                symbol: row.symbol,
                start_time: row.start_time,
                end_time: row.end_time,
                up_token: normalize_token_id(&row.up_token),
                down_token: normalize_token_id(&row.down_token),
            })
            .collect();

        Ok(markets)
    }

    /// Spawn Chainlink price feed subscriber.
    fn spawn_chainlink_feed(&self) -> tokio::task::JoinHandle<()> {
        let prices = self.chainlink_prices.clone();
        let symbols = self.config.symbols.clone();

        tokio::spawn(async move {
            let symbols_chainlink: Vec<String> = symbols
                .iter()
                .map(|s| {
                    let base = s.trim_end_matches("USDT").to_lowercase();
                    format!("{}/usd", base)
                })
                .collect();

            info!(symbols = ?symbols_chainlink, "Starting Chainlink price feed");

            let client = RtdsClient::default();
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

                        {
                            let mut cache = prices.write().await;
                            cache.insert(
                                chainlink_price.symbol.clone(),
                                (chainlink_price.value, ts),
                            );
                        }

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
        let prices = self.chainlink_prices.clone();
        let symbols = self.config.symbols.clone();
        let timeframe = self.config.timeframe.clone();

        tokio::spawn(async move {
            loop {
                sleep(StdDuration::from_secs(10)).await;

                // Query markets starting in the next 30 seconds that don't have price_to_beat
                let pattern = format!("%-updown-{}-%", timeframe);
                let now = Utc::now();
                let window_start = now;
                let window_end = now + chrono::Duration::seconds(30);

                let query = format!(
                    r#"
                    SELECT market_slug, symbol, start_time
                    FROM pm_market_metadata
                    WHERE symbol IN ({})
                      AND market_slug LIKE ${}
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
                    symbols.len() + 3
                );

                let mut q = sqlx::query_as::<_, (String, String, DateTime<Utc>)>(&query);
                for symbol in &symbols {
                    q = q.bind(symbol);
                }
                q = q.bind(&pattern);
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
                    // Wait until start_time
                    let wait_duration = (start_time - Utc::now()).num_milliseconds();
                    if wait_duration > 0 {
                        sleep(StdDuration::from_millis(wait_duration as u64)).await;
                    }

                    // Get Chainlink price
                    let chainlink_symbol = symbol.trim_end_matches("USDT").to_lowercase() + "/usd";
                    let price = {
                        let cache = prices.read().await;
                        cache.get(&chainlink_symbol).map(|(p, _)| *p)
                    };

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
    start_time: DateTime<Utc>,
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

/// Insert a quote into the database.
async fn insert_quote(
    pool: &PgPool,
    token_id: &str,
    side: &str,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
) -> Result<(), sqlx::Error> {
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
    .bind(side)
    .bind(best_bid)
    .bind(best_ask)
    .bind(Utc::now())
    .bind("polymarket_ws_collector")
    .bind("Crypto")
    .execute(pool)
    .await?;

    Ok(())
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
    use super::{hex_to_decimal_string, normalize_token_id};

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
}
