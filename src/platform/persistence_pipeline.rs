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
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};

use crate::platform::DataPlaneFreshness;
use crate::platform::types::Domain;

mod runtime;

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
    /// Set to `0` to persist every received update without throttling.
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
            binance_lob_snapshot_interval_ms: 0,
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

// ---------------------------------------------------------------------------
// Pipeline handle — the public API for producers
// ---------------------------------------------------------------------------

/// Handle for sending events into the persistence pipeline.
/// Cheap to clone; all clones share the same underlying channel.
#[derive(Clone)]
pub struct PersistencePipelineHandle {
    tx: mpsc::Sender<PersistenceEvent>,
    freshness: Option<Arc<DataPlaneFreshness>>,
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

    /// Spawn a bridge task from a broadcast receiver into this pipeline.
    ///
    /// The mapper closure transforms incoming broadcast messages into optional
    /// persistence events. Returning `None` drops the message.
    pub fn spawn_bridge<T, F>(
        &self,
        mut rx: broadcast::Receiver<T>,
        bridge_name: impl Into<String>,
        mut map_event: F,
    ) -> tokio::task::JoinHandle<()>
    where
        T: Clone + Send + 'static,
        F: FnMut(T) -> Option<PersistenceEvent> + Send + 'static,
    {
        let pipeline = self.clone();
        let freshness = self.freshness.clone();
        let bridge_name = bridge_name.into();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let Some(event) = map_event(msg) else {
                            continue;
                        };
                        if pipeline.try_ingest(event).is_err() {
                            warn!(bridge = %bridge_name, "persistence bridge dropped event");
                            if let Some(ref f) = freshness {
                                f.record_broadcast_drop(1);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(bridge = %bridge_name, lagged = n, "persistence bridge lagged");
                        if let Some(ref f) = freshness {
                            f.record_broadcast_lag(n as u64);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            debug!(bridge = %bridge_name, "persistence bridge stopped");
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
        Self::spawn_with_freshness(pool, config, None)
    }

    /// Create the pipeline with optional shared freshness tracking.
    pub fn spawn_with_freshness(
        pool: PgPool,
        config: PersistenceConfig,
        freshness: Option<Arc<DataPlaneFreshness>>,
    ) -> PersistencePipelineHandle {
        let (tx, rx) = mpsc::channel(config.channel_capacity);
        tokio::spawn(Self::run(rx, pool, config));
        PersistencePipelineHandle { tx, freshness }
    }
}
