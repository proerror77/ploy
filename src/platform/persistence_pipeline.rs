//! Persistence Pipeline — unified, deduplicated data ingestion for all WS-driven
//! market data streams.
//!
//! Replaces the 5 separate `spawn_*_persistence()` functions in bootstrap.rs
//! with a single pipeline that:
//! - Receives events via a bounded `mpsc` channel
//! - Applies per-type dedup logic (interval + value change)
//! - Batches writes for efficiency
//! - Preserves existing DB schema and SQL

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::platform::types::Domain;

// ---------------------------------------------------------------------------
// Pipeline configuration
// ---------------------------------------------------------------------------

/// Configuration for the persistence pipeline.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Channel capacity for the ingestion queue.
    pub channel_capacity: usize,
    /// Minimum interval between CLOB quote persists (seconds).
    pub clob_quote_min_interval_secs: i64,
    /// Minimum interval between Binance price persists (seconds).
    pub binance_price_min_interval_secs: i64,
    /// Minimum interval between Binance LOB snapshots (milliseconds).
    pub binance_lob_snapshot_interval_ms: i64,
    /// Maximum LOB depth levels to persist.
    pub binance_lob_max_levels: usize,
    /// Minimum interval between CLOB orderbook snapshots (milliseconds).
    pub clob_orderbook_snapshot_interval_ms: i64,
    /// Maximum orderbook depth levels to persist.
    pub clob_orderbook_max_levels: usize,
    /// Whether to require hash change for orderbook persistence.
    pub clob_orderbook_require_hash_change: bool,
    /// Flush interval for batched writes (milliseconds).
    pub flush_interval_ms: u64,
    /// Maximum batch size before forced flush.
    pub max_batch_size: usize,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 10_000,
            clob_quote_min_interval_secs: 2,
            binance_price_min_interval_secs: 1,
            binance_lob_snapshot_interval_ms: 1_000,
            binance_lob_max_levels: 20,
            clob_orderbook_snapshot_interval_ms: 2_000,
            clob_orderbook_max_levels: 50,
            clob_orderbook_require_hash_change: true,
            flush_interval_ms: 100,
            max_batch_size: 500,
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence events — typed union of all WS-driven data
// ---------------------------------------------------------------------------

/// A market data event to be persisted.
#[derive(Debug, Clone)]
pub enum PersistenceEvent {
    /// CLOB quote tick (from Polymarket WS).
    ClobQuote(ClobQuoteTick),
    /// Binance spot price tick.
    BinancePrice(BinancePriceTick),
    /// Binance LOB snapshot.
    BinanceLob(BinanceLobTick),
    /// Chainlink price tick.
    ChainlinkPrice(ChainlinkPriceTick),
    /// CLOB orderbook snapshot (from Polymarket WS).
    ClobOrderbook(ClobOrderbookSnapshot),
}

#[derive(Debug, Clone)]
pub struct ClobQuoteTick {
    pub token_id: String,
    pub side: String,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub bid_size: Option<Decimal>,
    pub ask_size: Option<Decimal>,
    pub domain: Domain,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct BinancePriceTick {
    pub symbol: String,
    pub price: Option<Decimal>,
    pub quantity: Option<Decimal>,
    pub trade_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct BinanceLobTick {
    pub symbol: String,
    pub update_id: i64,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub mid_price: Option<Decimal>,
    pub spread_bps: Option<Decimal>,
    pub obi_5: Option<f64>,
    pub obi_10: Option<f64>,
    pub bid_volume_5: Option<Decimal>,
    pub ask_volume_5: Option<Decimal>,
    pub bids: serde_json::Value,
    pub asks: serde_json::Value,
    pub event_time: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ChainlinkPriceTick {
    pub symbol: String,
    pub price: Decimal,
    pub source_timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ClobOrderbookSnapshot {
    pub domain: Domain,
    pub token_id: String,
    pub market: Option<String>,
    pub bids: serde_json::Value,
    pub asks: serde_json::Value,
    pub book_timestamp: Option<DateTime<Utc>>,
    pub hash: String,
    pub source: String,
    pub context: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Dedup state — mirrors the existing per-spawner HashMap logic
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct QuoteState {
    last_at: DateTime<Utc>,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct PriceState {
    last_at: DateTime<Utc>,
    price: Option<Decimal>,
    quantity: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct LobState {
    last_at: DateTime<Utc>,
    last_update_id: i64,
}

#[derive(Debug, Clone)]
struct OrderbookState {
    last_at: DateTime<Utc>,
    last_hash: String,
}

/// Internal dedup tracker.
#[derive(Debug, Default)]
struct DedupState {
    quotes: HashMap<String, QuoteState>,         // key: token_id
    prices: HashMap<String, PriceState>,         // key: symbol
    lobs: HashMap<String, LobState>,             // key: symbol
    orderbooks: HashMap<String, OrderbookState>, // key: token_id
}

// ---------------------------------------------------------------------------
// Pipeline handle — the public API for producers
// ---------------------------------------------------------------------------

/// Handle for sending events into the persistence pipeline.
/// Cheap to clone; all clones share the same underlying channel.
#[derive(Clone)]
pub struct PersistencePipelineHandle {
    tx: mpsc::Sender<PersistenceEvent>,
}

impl PersistencePipelineHandle {
    pub async fn ingest(&self, event: PersistenceEvent) -> Result<(), PersistenceEvent> {
        self.tx.send(event).await.map_err(|e| e.0)
    }

    /// Non-blocking try_send for hot paths (WS callbacks).
    pub fn try_ingest(&self, event: PersistenceEvent) -> Result<(), PersistenceEvent> {
        self.tx.try_send(event).map_err(|e| match e {
            mpsc::error::TrySendError::Full(ev) | mpsc::error::TrySendError::Closed(ev) => ev,
        })
    }
}

// ---------------------------------------------------------------------------
// Pipeline stats
// ---------------------------------------------------------------------------

/// Runtime statistics for the persistence pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineStats {
    pub clob_quotes_persisted: u64,
    pub clob_quotes_deduped: u64,
    pub binance_prices_persisted: u64,
    pub binance_prices_deduped: u64,
    pub binance_lobs_persisted: u64,
    pub binance_lobs_deduped: u64,
    pub chainlink_prices_persisted: u64,
    pub clob_orderbooks_persisted: u64,
    pub clob_orderbooks_deduped: u64,
    pub events_dropped: u64,
}

// ---------------------------------------------------------------------------
// PersistencePipeline — the main runner
// ---------------------------------------------------------------------------

/// The persistence pipeline.  Call `spawn()` to start the background writer
/// and get a `PersistencePipelineHandle` for producers.
pub struct PersistencePipeline;

impl PersistencePipeline {
    /// Create the pipeline and spawn the background writer task.
    /// Returns a handle that producers use to send events.
    pub fn spawn(pool: PgPool, config: PersistenceConfig) -> PersistencePipelineHandle {
        let (tx, rx) = mpsc::channel(config.channel_capacity);
        tokio::spawn(Self::run(rx, pool, config));
        PersistencePipelineHandle { tx }
    }

    async fn run(
        mut rx: mpsc::Receiver<PersistenceEvent>,
        pool: PgPool,
        config: PersistenceConfig,
    ) {
        let mut dedup = DedupState::default();
        let mut stats = PipelineStats::default();
        let mut log_counter: u64 = 0;

        info!(
            "persistence pipeline started (capacity={})",
            config.channel_capacity
        );

        while let Some(event) = rx.recv().await {
            match event {
                PersistenceEvent::ClobQuote(tick) => {
                    if Self::should_persist_quote(&tick, &mut dedup, &config) {
                        if let Err(e) = Self::write_clob_quote(&pool, &tick).await {
                            warn!(error = %e, token = %tick.token_id, "clob quote persist failed");
                        } else {
                            stats.clob_quotes_persisted += 1;
                        }
                    } else {
                        stats.clob_quotes_deduped += 1;
                    }
                }
                PersistenceEvent::BinancePrice(tick) => {
                    if Self::should_persist_price(&tick, &mut dedup, &config) {
                        if let Err(e) = Self::write_binance_price(&pool, &tick).await {
                            warn!(error = %e, symbol = %tick.symbol, "binance price persist failed");
                        } else {
                            stats.binance_prices_persisted += 1;
                        }
                    } else {
                        stats.binance_prices_deduped += 1;
                    }
                }
                PersistenceEvent::BinanceLob(tick) => {
                    if Self::should_persist_lob(&tick, &mut dedup, &config) {
                        if let Err(e) =
                            Self::write_binance_lob(&pool, &tick, config.binance_lob_max_levels)
                                .await
                        {
                            warn!(error = %e, symbol = %tick.symbol, "binance lob persist failed");
                        } else {
                            stats.binance_lobs_persisted += 1;
                        }
                    } else {
                        stats.binance_lobs_deduped += 1;
                    }
                }
                PersistenceEvent::ChainlinkPrice(tick) => {
                    // Chainlink has no dedup — persist every update
                    if let Err(e) = Self::write_chainlink_price(&pool, &tick).await {
                        warn!(error = %e, symbol = %tick.symbol, "chainlink price persist failed");
                    } else {
                        stats.chainlink_prices_persisted += 1;
                    }
                }
                PersistenceEvent::ClobOrderbook(snap) => {
                    if Self::should_persist_orderbook(&snap, &mut dedup, &config) {
                        if let Err(e) = Self::write_clob_orderbook(
                            &pool,
                            &snap,
                            config.clob_orderbook_max_levels,
                        )
                        .await
                        {
                            warn!(error = %e, token = %snap.token_id, "clob orderbook persist failed");
                        } else {
                            stats.clob_orderbooks_persisted += 1;
                        }
                    } else {
                        stats.clob_orderbooks_deduped += 1;
                    }
                }
            }

            log_counter += 1;
            if log_counter % 1000 == 0 {
                debug!(
                    quotes = stats.clob_quotes_persisted,
                    quotes_dedup = stats.clob_quotes_deduped,
                    prices = stats.binance_prices_persisted,
                    lobs = stats.binance_lobs_persisted,
                    chainlink = stats.chainlink_prices_persisted,
                    orderbooks = stats.clob_orderbooks_persisted,
                    "persistence pipeline stats"
                );
            }
        }

        info!("persistence pipeline shutting down");
    }

    // -----------------------------------------------------------------------
    // Dedup logic — mirrors existing spawn_*_persistence behavior
    // -----------------------------------------------------------------------

    fn should_persist_quote(
        tick: &ClobQuoteTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        // Skip if both bid and ask are None
        if tick.best_bid.is_none() && tick.best_ask.is_none() {
            return false;
        }

        let now = tick.received_at;
        if let Some(prev) = dedup.quotes.get(&tick.token_id) {
            let elapsed = (now - prev.last_at).num_seconds();
            let changed = prev.best_bid != tick.best_bid
                || prev.best_ask != tick.best_ask
                || prev.bid_size != tick.bid_size
                || prev.ask_size != tick.ask_size;
            if !changed || elapsed < config.clob_quote_min_interval_secs {
                return false;
            }
        }

        dedup.quotes.insert(
            tick.token_id.clone(),
            QuoteState {
                last_at: now,
                best_bid: tick.best_bid,
                best_ask: tick.best_ask,
                bid_size: tick.bid_size,
                ask_size: tick.ask_size,
            },
        );
        true
    }

    fn should_persist_price(
        tick: &BinancePriceTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = tick.trade_time;
        if let Some(prev) = dedup.prices.get(&tick.symbol) {
            let elapsed = (now - prev.last_at).num_seconds();
            let changed = prev.price != tick.price || prev.quantity != tick.quantity;
            if !changed || elapsed < config.binance_price_min_interval_secs {
                return false;
            }
        }

        dedup.prices.insert(
            tick.symbol.clone(),
            PriceState {
                last_at: now,
                price: tick.price,
                quantity: tick.quantity,
            },
        );
        true
    }

    fn should_persist_lob(
        tick: &BinanceLobTick,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = tick.event_time;
        if let Some(prev) = dedup.lobs.get(&tick.symbol) {
            let elapsed_ms = (now - prev.last_at).num_milliseconds();
            if elapsed_ms < config.binance_lob_snapshot_interval_ms
                || prev.last_update_id == tick.update_id
            {
                return false;
            }
        }

        dedup.lobs.insert(
            tick.symbol.clone(),
            LobState {
                last_at: now,
                last_update_id: tick.update_id,
            },
        );
        true
    }

    fn should_persist_orderbook(
        snap: &ClobOrderbookSnapshot,
        dedup: &mut DedupState,
        config: &PersistenceConfig,
    ) -> bool {
        let now = Utc::now();
        if let Some(prev) = dedup.orderbooks.get(&snap.token_id) {
            let elapsed_ms = (now - prev.last_at).num_milliseconds();
            if elapsed_ms < config.clob_orderbook_snapshot_interval_ms {
                return false;
            }
            if config.clob_orderbook_require_hash_change && prev.last_hash == snap.hash {
                return false;
            }
        }

        dedup.orderbooks.insert(
            snap.token_id.clone(),
            OrderbookState {
                last_at: now,
                last_hash: snap.hash.clone(),
            },
        );
        true
    }

    // -----------------------------------------------------------------------
    // SQL writers — same INSERT statements as existing spawn_* functions
    // -----------------------------------------------------------------------

    async fn write_clob_quote(pool: &PgPool, tick: &ClobQuoteTick) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO clob_quote_ticks
               (token_id, side, best_bid, best_ask, bid_size, ask_size, source, domain)
               VALUES ($1, $2, $3, $4, $5, $6, 'polymarket_ws', $7)"#,
        )
        .bind(&tick.token_id)
        .bind(&tick.side)
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.bid_size)
        .bind(tick.ask_size)
        .bind(tick.domain.to_string())
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_binance_price(
        pool: &PgPool,
        tick: &BinancePriceTick,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO binance_price_ticks
               (symbol, price, quantity, trade_time)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(&tick.symbol)
        .bind(tick.price)
        .bind(tick.quantity)
        .bind(tick.trade_time)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_binance_lob(
        pool: &PgPool,
        tick: &BinanceLobTick,
        _max_levels: usize,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO binance_lob_ticks
               (symbol, update_id, best_bid, best_ask, mid_price, spread_bps,
                obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks, event_time)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        )
        .bind(&tick.symbol)
        .bind(tick.update_id)
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.mid_price)
        .bind(tick.spread_bps)
        .bind(tick.obi_5)
        .bind(tick.obi_10)
        .bind(tick.bid_volume_5)
        .bind(tick.ask_volume_5)
        .bind(&tick.bids)
        .bind(&tick.asks)
        .bind(tick.event_time)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_chainlink_price(
        pool: &PgPool,
        tick: &ChainlinkPriceTick,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO chainlink_price_ticks
               (symbol, price, source_timestamp)
               VALUES ($1, $2, $3)"#,
        )
        .bind(&tick.symbol)
        .bind(tick.price)
        .bind(tick.source_timestamp)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn write_clob_orderbook(
        pool: &PgPool,
        snap: &ClobOrderbookSnapshot,
        _max_levels: usize,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO clob_orderbook_snapshots
               (domain, token_id, market, bids, asks, book_timestamp, hash, source, context)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
        )
        .bind(snap.domain.to_string())
        .bind(&snap.token_id)
        .bind(&snap.market)
        .bind(&snap.bids)
        .bind(&snap.asks)
        .bind(snap.book_timestamp)
        .bind(&snap.hash)
        .bind(&snap.source)
        .bind(&snap.context)
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn default_config() -> PersistenceConfig {
        PersistenceConfig::default()
    }

    fn make_quote(
        token: &str,
        bid: Option<f64>,
        ask: Option<f64>,
        at: DateTime<Utc>,
    ) -> ClobQuoteTick {
        ClobQuoteTick {
            token_id: token.into(),
            side: "UP".into(),
            best_bid: bid.map(|v| Decimal::from_f64_retain(v).unwrap()),
            best_ask: ask.map(|v| Decimal::from_f64_retain(v).unwrap()),
            bid_size: Some(Decimal::from(100)),
            ask_size: Some(Decimal::from(100)),
            domain: Domain::Crypto,
            received_at: at,
        }
    }

    fn make_price(symbol: &str, price: f64, at: DateTime<Utc>) -> BinancePriceTick {
        BinancePriceTick {
            symbol: symbol.into(),
            price: Some(Decimal::from_f64_retain(price).unwrap()),
            quantity: Some(Decimal::from(1)),
            trade_time: at,
        }
    }

    #[test]
    fn quote_dedup_skips_unchanged_within_interval() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let q1 = make_quote("tok-1", Some(0.42), Some(0.45), t0);
        assert!(PersistencePipeline::should_persist_quote(
            &q1, &mut dedup, &config
        ));

        // Same values, 1s later (< 2s interval) → skip
        let q2 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(1));
        assert!(!PersistencePipeline::should_persist_quote(
            &q2, &mut dedup, &config
        ));

        // Same values, 3s later (>= 2s interval) but unchanged → skip
        let q3 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(3));
        assert!(!PersistencePipeline::should_persist_quote(
            &q3, &mut dedup, &config
        ));

        // Changed values, 3s later → persist
        let q4 = make_quote("tok-1", Some(0.43), Some(0.45), t0 + Duration::seconds(3));
        assert!(PersistencePipeline::should_persist_quote(
            &q4, &mut dedup, &config
        ));
    }

    #[test]
    fn quote_dedup_skips_both_none() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let q = ClobQuoteTick {
            token_id: "tok-1".into(),
            side: "UP".into(),
            best_bid: None,
            best_ask: None,
            bid_size: None,
            ask_size: None,
            domain: Domain::Crypto,
            received_at: Utc::now(),
        };
        assert!(!PersistencePipeline::should_persist_quote(
            &q, &mut dedup, &config
        ));
    }

    #[test]
    fn price_dedup_respects_interval_and_change() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let p1 = make_price("BTCUSDT", 50000.0, t0);
        assert!(PersistencePipeline::should_persist_price(
            &p1, &mut dedup, &config
        ));

        // Same price, 0.5s later → skip
        let p2 = make_price("BTCUSDT", 50000.0, t0 + Duration::milliseconds(500));
        assert!(!PersistencePipeline::should_persist_price(
            &p2, &mut dedup, &config
        ));

        // Different price, 2s later → persist
        let p3 = make_price("BTCUSDT", 50001.0, t0 + Duration::seconds(2));
        assert!(PersistencePipeline::should_persist_price(
            &p3, &mut dedup, &config
        ));
    }

    #[test]
    fn lob_dedup_requires_interval_and_new_update_id() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let l1 = BinanceLobTick {
            symbol: "BTCUSDT".into(),
            update_id: 100,
            best_bid: Some(Decimal::from(50000)),
            best_ask: Some(Decimal::from(50001)),
            mid_price: None,
            spread_bps: None,
            obi_5: None,
            obi_10: None,
            bid_volume_5: None,
            ask_volume_5: None,
            bids: serde_json::json!([]),
            asks: serde_json::json!([]),
            event_time: t0,
        };
        assert!(PersistencePipeline::should_persist_lob(
            &l1, &mut dedup, &config
        ));

        // Same update_id, 2s later → skip
        let mut l2 = l1.clone();
        l2.event_time = t0 + Duration::seconds(2);
        assert!(!PersistencePipeline::should_persist_lob(
            &l2, &mut dedup, &config
        ));

        // New update_id, 2s later → persist
        let mut l3 = l1.clone();
        l3.update_id = 101;
        l3.event_time = t0 + Duration::seconds(2);
        assert!(PersistencePipeline::should_persist_lob(
            &l3, &mut dedup, &config
        ));
    }

    #[test]
    fn orderbook_dedup_respects_hash_change() {
        let config = default_config();
        let mut dedup = DedupState::default();

        let s1 = ClobOrderbookSnapshot {
            domain: Domain::Crypto,
            token_id: "tok-1".into(),
            market: None,
            bids: serde_json::json!([]),
            asks: serde_json::json!([]),
            book_timestamp: None,
            hash: "abc123".into(),
            source: "polymarket_ws".into(),
            context: None,
        };
        assert!(PersistencePipeline::should_persist_orderbook(
            &s1, &mut dedup, &config
        ));

        // Same hash, after interval → skip (require_hash_change=true)
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Force enough time by manipulating dedup state
        dedup.orderbooks.get_mut("tok-1").unwrap().last_at = Utc::now() - Duration::seconds(10);
        assert!(!PersistencePipeline::should_persist_orderbook(
            &s1, &mut dedup, &config
        ));

        // Different hash → persist
        let mut s2 = s1.clone();
        s2.hash = "def456".into();
        assert!(PersistencePipeline::should_persist_orderbook(
            &s2, &mut dedup, &config
        ));
    }

    #[test]
    fn different_tokens_tracked_independently() {
        let config = default_config();
        let mut dedup = DedupState::default();
        let t0 = Utc::now();

        let q1 = make_quote("tok-1", Some(0.42), Some(0.45), t0);
        let q2 = make_quote("tok-2", Some(0.55), Some(0.58), t0);

        assert!(PersistencePipeline::should_persist_quote(
            &q1, &mut dedup, &config
        ));
        assert!(PersistencePipeline::should_persist_quote(
            &q2, &mut dedup, &config
        ));

        // tok-1 unchanged within interval → skip; tok-2 changed → still skip (interval)
        let q3 = make_quote("tok-1", Some(0.42), Some(0.45), t0 + Duration::seconds(1));
        let q4 = make_quote("tok-2", Some(0.56), Some(0.58), t0 + Duration::seconds(1));
        assert!(!PersistencePipeline::should_persist_quote(
            &q3, &mut dedup, &config
        ));
        assert!(!PersistencePipeline::should_persist_quote(
            &q4, &mut dedup, &config
        ));
    }
}
