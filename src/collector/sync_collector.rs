//! Synchronized data collector for lag analysis
//!
//! Collects and aligns Binance LOB data with Polymarket prices
//! to analyze the lead-lag relationship for the 15-min crypto prediction strategy.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use super::binance_depth::{BinanceDepthStream, LobUpdate};
use super::token_targets::{
    ensure_collector_token_targets_table, upsert_collector_token_targets, CollectorTokenTarget,
};
use crate::error::Result;

/// Synchronized price record for lag analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    pub timestamp: DateTime<Utc>,
    pub symbol: String, // e.g., "BTCUSDT"

    // Binance LOB data
    pub bn_mid_price: Decimal,
    pub bn_best_bid: Decimal,
    pub bn_best_ask: Decimal,
    pub bn_spread_bps: Decimal,
    pub bn_obi_5: Decimal,
    pub bn_obi_10: Decimal,
    pub bn_bid_volume: Decimal,
    pub bn_ask_volume: Decimal,

    // Polymarket data (YES price for "up" outcome)
    pub pm_yes_price: Option<Decimal>,
    pub pm_no_price: Option<Decimal>,
    pub pm_market_slug: Option<String>,
    pub pm_yes_token_id: Option<String>,
    pub pm_no_token_id: Option<String>,

    // Derived signals
    pub bn_price_change_1s: Option<Decimal>, // Price change vs 1s ago
    pub bn_price_change_5s: Option<Decimal>, // Price change vs 5s ago
    pub bn_momentum: Option<Decimal>,        // Short-term momentum signal
}

/// Polymarket price snapshot
#[derive(Debug, Clone)]
pub struct PolymarketPrice {
    pub timestamp: DateTime<Utc>,
    pub market_slug: String,
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub yes_token_id: Option<String>,
    pub no_token_id: Option<String>,
}

/// Configuration for the sync collector
#[derive(Debug, Clone)]
pub struct SyncCollectorConfig {
    /// Binance symbols to track (e.g., ["BTCUSDT", "ETHUSDT"])
    pub binance_symbols: Vec<String>,
    /// Polymarket market slugs to track
    pub polymarket_slugs: Vec<String>,
    /// How often to save snapshots (milliseconds)
    pub snapshot_interval_ms: u64,
    /// Database connection string
    pub database_url: String,
}

impl Default for SyncCollectorConfig {
    fn default() -> Self {
        Self {
            binance_symbols: vec!["BTCUSDT".to_string()],
            polymarket_slugs: vec![],
            snapshot_interval_ms: 100, // 100ms = 10 snapshots/sec
            database_url: String::new(),
        }
    }
}

/// Price history for momentum calculation
#[derive(Debug, Clone, Default)]
struct PriceHistory {
    /// (timestamp, price) pairs, newest first
    history: Vec<(DateTime<Utc>, Decimal)>,
    max_size: usize,
}

impl PriceHistory {
    fn new(max_size: usize) -> Self {
        Self {
            history: Vec::with_capacity(max_size),
            max_size,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: Decimal) {
        self.history.insert(0, (ts, price));
        if self.history.len() > self.max_size {
            self.history.pop();
        }
    }

    fn price_secs_ago(&self, secs: i64) -> Option<Decimal> {
        if self.history.is_empty() {
            return None;
        }

        let now = self.history.first()?.0;
        let target = now - chrono::Duration::seconds(secs);

        for (ts, price) in &self.history {
            if *ts <= target {
                return Some(*price);
            }
        }

        self.history.last().map(|(_, p)| *p)
    }

    fn momentum(&self, lookback_secs: i64) -> Option<Decimal> {
        let current = self.history.first()?.1;
        let past = self.price_secs_ago(lookback_secs)?;

        if past.is_zero() {
            return None;
        }

        Some((current - past) / past)
    }
}

fn symbol_aliases(symbol: &str) -> &'static [&'static str] {
    match symbol {
        "BTCUSDT" => &["btc", "bitcoin"],
        "ETHUSDT" => &["eth", "ethereum"],
        "SOLUSDT" => &["sol", "solana"],
        "XRPUSDT" => &["xrp", "ripple"],
        _ => &[],
    }
}

fn slug_has_alias_token(slug: &str, aliases: &[&str]) -> bool {
    slug.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .any(|tok| aliases.iter().any(|a| tok == *a))
}

fn select_polymarket_price_for_symbol(
    symbol: &str,
    prices: &std::collections::HashMap<String, PolymarketPrice>,
) -> (
    Option<Decimal>,
    Option<Decimal>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let aliases = symbol_aliases(symbol);
    if aliases.is_empty() {
        return (None, None, None, None, None);
    }

    let mut best: Option<(
        bool,
        DateTime<Utc>,
        Decimal,
        Decimal,
        String,
        Option<String>,
        Option<String>,
    )> = None;
    for (slug, price) in prices.iter() {
        let slug_lower = slug.to_ascii_lowercase();
        if !slug_has_alias_token(&slug_lower, aliases) {
            continue;
        }

        let has_updown = slug_lower.contains("up") && slug_lower.contains("down");
        match &best {
            None => {
                best = Some((
                    has_updown,
                    price.timestamp,
                    price.yes_price,
                    price.no_price,
                    slug.clone(),
                    price.yes_token_id.clone(),
                    price.no_token_id.clone(),
                ));
            }
            Some((best_updown, best_ts, _, _, _, _, _)) => {
                if (has_updown && !*best_updown)
                    || (has_updown == *best_updown && price.timestamp > *best_ts)
                {
                    best = Some((
                        has_updown,
                        price.timestamp,
                        price.yes_price,
                        price.no_price,
                        slug.clone(),
                        price.yes_token_id.clone(),
                        price.no_token_id.clone(),
                    ));
                }
            }
        }
    }

    if let Some((_, _, yes, no, slug, yes_token_id, no_token_id)) = best {
        (
            Some(yes),
            Some(no),
            Some(slug),
            yes_token_id,
            no_token_id,
        )
    } else {
        (None, None, None, None, None)
    }
}

/// Synchronized data collector
pub struct SyncCollector {
    depth_stream: Arc<BinanceDepthStream>,
    price_histories: Arc<RwLock<std::collections::HashMap<String, PriceHistory>>>,
    polymarket_prices: Arc<RwLock<std::collections::HashMap<String, PolymarketPrice>>>,
    record_tx: broadcast::Sender<SyncRecord>,
    pool: Option<PgPool>,
    persist_sync_records: bool,
}

impl SyncCollector {
    /// Create a new sync collector
    pub fn new(config: SyncCollectorConfig) -> Self {
        let depth_stream = Arc::new(BinanceDepthStream::new(config.binance_symbols.clone()));
        let (record_tx, _) = broadcast::channel(10000);
        let persist_sync_records = std::env::var("PLOY_COLLECTOR_PERSIST_SYNC_RECORDS")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);

        Self {
            depth_stream,
            price_histories: Arc::new(RwLock::new(std::collections::HashMap::new())),
            polymarket_prices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            record_tx,
            pool: None,
            persist_sync_records,
        }
    }

    /// Set database pool for persistence
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Subscribe to sync records
    pub fn subscribe(&self) -> broadcast::Receiver<SyncRecord> {
        self.record_tx.subscribe()
    }

    /// Update Polymarket price (call this from Polymarket WS handler)
    pub async fn update_polymarket_price(&self, price: PolymarketPrice) {
        let mut prices = self.polymarket_prices.write().await;
        prices.insert(price.market_slug.clone(), price);
    }

    /// Persist a raw Polymarket quote tick into canonical quote table.
    pub async fn persist_polymarket_quote_tick(
        &self,
        token_id: &str,
        side: &str,
        best_bid: Option<Decimal>,
        best_ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
        received_at: DateTime<Utc>,
    ) -> Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        sqlx::query(
            r#"
            INSERT INTO clob_quote_ticks (
                token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(token_id)
        .bind(side)
        .bind(best_bid)
        .bind(best_ask)
        .bind(bid_size)
        .bind(ask_size)
        .bind("sync_collector")
        .bind(received_at)
        .bind("crypto")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Upsert token targets for canonical symbol/slug/side mapping.
    pub async fn upsert_token_targets(&self, targets: &[CollectorTokenTarget]) -> Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        ensure_collector_token_targets_table(pool).await?;
        upsert_collector_token_targets(pool, targets).await
    }

    /// Get LOB cache reference
    pub fn lob_cache(&self) -> &super::binance_depth::LobCache {
        self.depth_stream.cache()
    }

    /// Run the collector
    pub async fn run(&self) -> Result<()> {
        info!("Starting sync collector");

        // Create tables if pool is set
        if let Some(pool) = &self.pool {
            self.create_tables(pool).await?;
        }

        // Subscribe to LOB updates
        let mut lob_rx = self.depth_stream.subscribe();

        // Spawn depth stream
        let depth_stream = self.depth_stream.clone();
        tokio::spawn(async move {
            if let Err(e) = depth_stream.run().await {
                error!("Depth stream error: {}", e);
            }
        });

        // Process LOB updates
        loop {
            match lob_rx.recv().await {
                Ok(update) => {
                    self.process_lob_update(update).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Sync collector lagged {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("LOB channel closed");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process a LOB update and generate sync record
    async fn process_lob_update(&self, update: LobUpdate) {
        let snapshot = &update.snapshot;

        // Update price history
        {
            let mut histories = self.price_histories.write().await;
            let history = histories
                .entry(update.symbol.clone())
                .or_insert_with(|| PriceHistory::new(600)); // 60 seconds at 10/sec
            history.push(snapshot.timestamp, snapshot.mid_price);
        }

        // Get momentum signals
        let (price_change_1s, price_change_5s, momentum) = {
            let histories = self.price_histories.read().await;
            if let Some(history) = histories.get(&update.symbol) {
                (
                    history.momentum(1),
                    history.momentum(5),
                    history.momentum(10),
                )
            } else {
                (None, None, None)
            }
        };

        // Get Polymarket prices (symbol -> slug mapping, deterministic and safe for unknown symbols).
        let (pm_yes, pm_no, pm_slug, pm_yes_token_id, pm_no_token_id) = {
            let prices = self.polymarket_prices.read().await;
            select_polymarket_price_for_symbol(&update.symbol, &prices)
        };

        // Create sync record
        let record = SyncRecord {
            timestamp: snapshot.timestamp,
            symbol: update.symbol.clone(),
            bn_mid_price: snapshot.mid_price,
            bn_best_bid: snapshot.best_bid,
            bn_best_ask: snapshot.best_ask,
            bn_spread_bps: snapshot.spread_bps,
            bn_obi_5: snapshot.obi_5,
            bn_obi_10: snapshot.obi_10,
            bn_bid_volume: snapshot.bid_volume_5,
            bn_ask_volume: snapshot.ask_volume_5,
            pm_yes_price: pm_yes,
            pm_no_price: pm_no,
            pm_market_slug: pm_slug,
            pm_yes_token_id,
            pm_no_token_id,
            bn_price_change_1s: price_change_1s,
            bn_price_change_5s: price_change_5s,
            bn_momentum: momentum,
        };

        // Broadcast
        let _ = self.record_tx.send(record.clone());

        // Persist if pool is set:
        // 1) canonical raw LOB table (primary sink)
        // 2) legacy sync_records table (optional compatibility sink)
        if let Some(pool) = &self.pool {
            if let Err(e) = self.save_binance_lob_tick(pool, &update).await {
                debug!("Failed to save raw Binance LOB tick: {}", e);
            }

            if self.persist_sync_records {
                if let Err(e) = self.save_record(pool, &record).await {
                    debug!("Failed to save sync record: {}", e);
                }
            }
        }
    }

    /// Create database tables
    async fn create_tables(&self, pool: &PgPool) -> Result<()> {
        crate::platform::persistence_schema::ensure_binance_lob_ticks_table(pool).await?;
        crate::platform::persistence_schema::ensure_clob_quote_ticks_table(pool).await?;

        if self.persist_sync_records {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS sync_records (
                    id BIGSERIAL PRIMARY KEY,
                    timestamp TIMESTAMPTZ NOT NULL,
                    symbol VARCHAR(20) NOT NULL,
                    bn_mid_price DECIMAL(20, 8) NOT NULL,
                    bn_best_bid DECIMAL(20, 8) NOT NULL,
                    bn_best_ask DECIMAL(20, 8) NOT NULL,
                    bn_spread_bps DECIMAL(10, 4) NOT NULL,
                    bn_obi_5 DECIMAL(10, 6) NOT NULL,
                    bn_obi_10 DECIMAL(10, 6) NOT NULL,
                    bn_bid_volume DECIMAL(20, 8) NOT NULL,
                    bn_ask_volume DECIMAL(20, 8) NOT NULL,
                    pm_yes_price DECIMAL(10, 4),
                    pm_no_price DECIMAL(10, 4),
                    pm_market_slug VARCHAR(100),
                    pm_yes_token_id TEXT,
                    pm_no_token_id TEXT,
                    bn_price_change_1s DECIMAL(10, 6),
                    bn_price_change_5s DECIMAL(10, 6),
                    bn_momentum DECIMAL(10, 6),
                    created_at TIMESTAMPTZ DEFAULT NOW()
                )
                "#,
            )
            .execute(pool)
            .await?;

            sqlx::query(
                r#"
                ALTER TABLE sync_records
                    ADD COLUMN IF NOT EXISTS pm_yes_token_id TEXT,
                    ADD COLUMN IF NOT EXISTS pm_no_token_id TEXT
                "#,
            )
            .execute(pool)
            .await?;

            sqlx::query("CREATE INDEX IF NOT EXISTS idx_sync_records_ts ON sync_records(timestamp)")
                .execute(pool)
                .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_symbol ON sync_records(symbol)",
            )
            .execute(pool)
            .await?;
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_sync_records_symbol_ts ON sync_records(symbol, timestamp)")
                .execute(pool)
                .await?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_sync_records_pm_market_slug ON sync_records(pm_market_slug)",
            )
            .execute(pool)
            .await?;
        }

        self.ensure_sync_records_derived_view(pool).await?;
        info!(
            persist_sync_records = self.persist_sync_records,
            "sync collector schema initialized"
        );
        Ok(())
    }

    async fn ensure_sync_records_derived_view(&self, pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE OR REPLACE VIEW sync_records_derived AS
            WITH token_pairs AS (
                SELECT
                    NULLIF(BTRIM(metadata->>'symbol'), '') AS symbol,
                    NULLIF(BTRIM(metadata->>'slug'), '') AS market_slug,
                    MAX(CASE WHEN UPPER(COALESCE(metadata->>'side', '')) = 'UP' THEN token_id END) AS pm_yes_token_id,
                    MAX(CASE WHEN UPPER(COALESCE(metadata->>'side', '')) = 'DOWN' THEN token_id END) AS pm_no_token_id,
                    MAX(updated_at) AS updated_at
                FROM collector_token_targets
                GROUP BY 1, 2
            ),
            token_choice AS (
                SELECT DISTINCT ON (symbol)
                    symbol,
                    market_slug,
                    pm_yes_token_id,
                    pm_no_token_id
                FROM token_pairs
                WHERE symbol IS NOT NULL
                  AND pm_yes_token_id IS NOT NULL
                  AND pm_no_token_id IS NOT NULL
                ORDER BY symbol, updated_at DESC
            )
            SELECT
                b.event_time AS timestamp,
                b.symbol,
                b.mid_price AS bn_mid_price,
                b.best_bid AS bn_best_bid,
                b.best_ask AS bn_best_ask,
                b.spread_bps AS bn_spread_bps,
                b.obi_5 AS bn_obi_5,
                b.obi_10 AS bn_obi_10,
                b.bid_volume_5 AS bn_bid_volume,
                b.ask_volume_5 AS bn_ask_volume,
                qy.best_ask AS pm_yes_price,
                qn.best_ask AS pm_no_price,
                tc.market_slug AS pm_market_slug,
                tc.pm_yes_token_id,
                tc.pm_no_token_id,
                CASE
                    WHEN p1.mid_price IS NULL OR p1.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p1.mid_price) / p1.mid_price
                END AS bn_price_change_1s,
                CASE
                    WHEN p5.mid_price IS NULL OR p5.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p5.mid_price) / p5.mid_price
                END AS bn_price_change_5s,
                CASE
                    WHEN p10.mid_price IS NULL OR p10.mid_price = 0 THEN NULL
                    ELSE (b.mid_price - p10.mid_price) / p10.mid_price
                END AS bn_momentum
            FROM binance_lob_ticks b
            LEFT JOIN token_choice tc
              ON tc.symbol = b.symbol
            LEFT JOIN LATERAL (
                SELECT q.best_ask
                FROM clob_quote_ticks q
                WHERE q.token_id = tc.pm_yes_token_id
                  AND q.side = 'UP'
                  AND q.received_at <= b.event_time
                ORDER BY q.received_at DESC
                LIMIT 1
            ) qy ON true
            LEFT JOIN LATERAL (
                SELECT q.best_ask
                FROM clob_quote_ticks q
                WHERE q.token_id = tc.pm_no_token_id
                  AND q.side = 'DOWN'
                  AND q.received_at <= b.event_time
                ORDER BY q.received_at DESC
                LIMIT 1
            ) qn ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '1 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p1 ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '5 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p5 ON true
            LEFT JOIN LATERAL (
                SELECT b1.mid_price
                FROM binance_lob_ticks b1
                WHERE b1.symbol = b.symbol
                  AND b1.event_time <= (b.event_time - INTERVAL '10 second')
                ORDER BY b1.event_time DESC
                LIMIT 1
            ) p10 ON true
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn save_binance_lob_tick(&self, pool: &PgPool, update: &LobUpdate) -> Result<()> {
        let bids = depth_levels_json(&update.raw_state, true, 20);
        let asks = depth_levels_json(&update.raw_state, false, 20);

        sqlx::query(
            r#"
            INSERT INTO binance_lob_ticks (
                symbol, update_id, best_bid, best_ask, mid_price, spread_bps,
                obi_5, obi_10, bid_volume_5, ask_volume_5,
                bids, asks, event_time, source
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14
            )
            "#,
        )
        .bind(&update.symbol)
        .bind(update.snapshot.update_id)
        .bind(update.snapshot.best_bid)
        .bind(update.snapshot.best_ask)
        .bind(update.snapshot.mid_price)
        .bind(update.snapshot.spread_bps)
        .bind(update.snapshot.obi_5)
        .bind(update.snapshot.obi_10)
        .bind(update.snapshot.bid_volume_5)
        .bind(update.snapshot.ask_volume_5)
        .bind(bids)
        .bind(asks)
        .bind(update.snapshot.timestamp)
        .bind("sync_collector")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Save a sync record to database
    async fn save_record(&self, pool: &PgPool, record: &SyncRecord) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO sync_records (
                timestamp, symbol, bn_mid_price, bn_best_bid, bn_best_ask,
                bn_spread_bps, bn_obi_5, bn_obi_10, bn_bid_volume, bn_ask_volume,
                pm_yes_price, pm_no_price, pm_market_slug,
                pm_yes_token_id, pm_no_token_id,
                bn_price_change_1s, bn_price_change_5s, bn_momentum
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
            "#,
        )
        .bind(record.timestamp)
        .bind(&record.symbol)
        .bind(record.bn_mid_price)
        .bind(record.bn_best_bid)
        .bind(record.bn_best_ask)
        .bind(record.bn_spread_bps)
        .bind(record.bn_obi_5)
        .bind(record.bn_obi_10)
        .bind(record.bn_bid_volume)
        .bind(record.bn_ask_volume)
        .bind(record.pm_yes_price)
        .bind(record.pm_no_price)
        .bind(&record.pm_market_slug)
        .bind(&record.pm_yes_token_id)
        .bind(&record.pm_no_token_id)
        .bind(record.bn_price_change_1s)
        .bind(record.bn_price_change_5s)
        .bind(record.bn_momentum)
        .execute(pool)
        .await?;

        Ok(())
    }
}

/// Lag analyzer for studying BN -> PM lead-lag relationship
pub struct LagAnalyzer {
    pool: PgPool,
}

impl LagAnalyzer {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Analyze lag between Binance price moves and Polymarket reactions
    pub async fn analyze_lag(
        &self,
        symbol: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LagAnalysisResult> {
        // Get records with significant price moves
        let records = sqlx::query_as::<
            _,
            (
                DateTime<Utc>,
                Decimal,
                Option<Decimal>,
                Option<Decimal>,
                Option<Decimal>,
            ),
        >(
            r#"
            SELECT timestamp, bn_mid_price, bn_price_change_5s, pm_yes_price, pm_no_price
            FROM sync_records_derived
            WHERE symbol = $1 AND timestamp BETWEEN $2 AND $3
            AND ABS(bn_price_change_5s) > 0.001
            ORDER BY timestamp
            "#,
        )
        .bind(symbol)
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await?;

        // Analyze lead-lag patterns
        let mut up_moves_bn_lead = 0;
        let mut down_moves_bn_lead = 0;
        let mut total_up_moves = 0;
        let mut total_down_moves = 0;

        for (i, record) in records.iter().enumerate() {
            let (_ts, _bn_price, bn_change, pm_yes, _pm_no) = record;

            if let Some(change) = bn_change.as_ref() {
                // Look ahead 1-10 seconds for PM reaction
                let pm_reacted =
                    records
                        .iter()
                        .skip(i + 1)
                        .take(100)
                        .any(|(_, _, _, future_pm, _)| {
                            if let (Some(current), Some(future)) =
                                (pm_yes.as_ref(), future_pm.as_ref())
                            {
                                let pm_change = (*future - *current) / *current;
                                (*change > Decimal::ZERO && pm_change > Decimal::ZERO)
                                    || (*change < Decimal::ZERO && pm_change < Decimal::ZERO)
                            } else {
                                false
                            }
                        });

                if *change > Decimal::ZERO {
                    total_up_moves += 1;
                    if pm_reacted {
                        up_moves_bn_lead += 1;
                    }
                } else {
                    total_down_moves += 1;
                    if pm_reacted {
                        down_moves_bn_lead += 1;
                    }
                }
            }
        }

        Ok(LagAnalysisResult {
            symbol: symbol.to_string(),
            start,
            end,
            total_records: records.len(),
            total_up_moves,
            total_down_moves,
            up_moves_bn_lead,
            down_moves_bn_lead,
            bn_lead_rate: if total_up_moves + total_down_moves > 0 {
                (up_moves_bn_lead + down_moves_bn_lead) as f64
                    / (total_up_moves + total_down_moves) as f64
            } else {
                0.0
            },
        })
    }
}

fn depth_levels_json(
    state: &super::binance_depth::OrderBookState,
    is_bid: bool,
    max_levels: usize,
) -> serde_json::Value {
    let levels: Vec<serde_json::Value> = if is_bid {
        state
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(p, q)| {
                let price = Decimal::from(*p) / Decimal::from(100);
                serde_json::json!({
                    "price": price.to_string(),
                    "size": q.to_string(),
                })
            })
            .collect()
    } else {
        state
            .asks
            .iter()
            .take(max_levels)
            .map(|(p, q)| {
                let price = Decimal::from(*p) / Decimal::from(100);
                serde_json::json!({
                    "price": price.to_string(),
                    "size": q.to_string(),
                })
            })
            .collect()
    };
    serde_json::Value::Array(levels)
}

/// Result of lag analysis
#[derive(Debug, Clone, Serialize)]
pub struct LagAnalysisResult {
    pub symbol: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub total_records: usize,
    pub total_up_moves: usize,
    pub total_down_moves: usize,
    pub up_moves_bn_lead: usize,
    pub down_moves_bn_lead: usize,
    pub bn_lead_rate: f64, // How often Binance leads Polymarket
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn pm_price(ts: DateTime<Utc>, slug: &str, yes: i64, no: i64) -> PolymarketPrice {
        PolymarketPrice {
            timestamp: ts,
            market_slug: slug.to_string(),
            yes_price: Decimal::new(yes, 2),
            no_price: Decimal::new(no, 2),
            yes_token_id: Some(format!("{slug}-yes")),
            no_token_id: Some(format!("{slug}-no")),
        }
    }

    #[test]
    fn select_pm_price_handles_xrp_and_avoids_empty_prefix_bug() {
        let now = Utc::now();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            "bitcoin-up-or-down-5m".to_string(),
            pm_price(now, "bitcoin-up-or-down-5m", 55, 45),
        );
        prices.insert(
            "xrp-up-or-down-5m".to_string(),
            pm_price(now + Duration::seconds(1), "xrp-up-or-down-5m", 52, 48),
        );

        let (yes, no, slug, yes_token_id, no_token_id) =
            select_polymarket_price_for_symbol("XRPUSDT", &prices);
        assert_eq!(slug.as_deref(), Some("xrp-up-or-down-5m"));
        assert_eq!(yes, Some(Decimal::new(52, 2)));
        assert_eq!(no, Some(Decimal::new(48, 2)));
        assert_eq!(yes_token_id.as_deref(), Some("xrp-up-or-down-5m-yes"));
        assert_eq!(no_token_id.as_deref(), Some("xrp-up-or-down-5m-no"));
    }

    #[test]
    fn select_pm_price_returns_none_for_unknown_symbol() {
        let now = Utc::now();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            "bitcoin-up-or-down-5m".to_string(),
            pm_price(now, "bitcoin-up-or-down-5m", 55, 45),
        );

        let (yes, no, slug, yes_token_id, no_token_id) =
            select_polymarket_price_for_symbol("DOGEUSDT", &prices);
        assert!(yes.is_none());
        assert!(no.is_none());
        assert!(slug.is_none());
        assert!(yes_token_id.is_none());
        assert!(no_token_id.is_none());
    }
}
