//! Binance WebSocket adapter for real-time spot price data
//!
//! Connects to Binance's WebSocket API to receive live trade updates for
//! BTC, ETH, and SOL. Maintains rolling price windows for momentum calculation.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Binance WebSocket URL for spot market streams
const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";

/// Binance WebSocket host for proxy CONNECT
const BINANCE_WS_HOST: &str = "stream.binance.com";
const BINANCE_WS_PORT: u16 = 9443;

/// How often to send ping frames
const PING_INTERVAL_SECS: u64 = 30;

/// Maximum reconnection delay
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

#[path = "binance_ws/runtime.rs"]
mod runtime;
#[path = "binance_ws/spot_cache.rs"]
mod spot_cache;

pub use spot_cache::{PriceCache, SpotPrice};

/// Price update broadcast channel capacity
const CHANNEL_CAPACITY: usize = 1000;

/// Binance trade message structure
#[derive(Debug, Deserialize)]
pub struct BinanceTrade {
    #[serde(rename = "e")]
    pub event_type: String,
    #[serde(rename = "E")]
    pub event_time: u64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "T")]
    pub trade_time: u64,
}

/// Aggregated trade message (more efficient for high-volume pairs)
#[derive(Debug, Deserialize)]
pub struct BinanceAggTrade {
    #[serde(rename = "e")]
    pub event_type: String,
    #[serde(rename = "E")]
    pub event_time: u64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "T")]
    pub trade_time: u64,
}

/// Price update event broadcast to subscribers
#[derive(Debug, Clone)]
pub struct PriceUpdate {
    pub symbol: String,
    pub price: Decimal,
    pub quantity: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}

/// Binance WebSocket client for real-time price data
pub struct BinanceWebSocket {
    ws_url: String,
    price_cache: PriceCache,
    update_tx: broadcast::Sender<PriceUpdate>,
    symbols: Vec<String>,
    reconnect_delay: Duration,
    // Optional: per-symbol freshness tracking for the data plane.
    freshness: OnceLock<Arc<crate::data_plane::DataPlaneFreshness>>,
}

impl BinanceWebSocket {
    /// Create a new Binance WebSocket client
    ///
    /// # Arguments
    /// * `symbols` - List of trading pairs to subscribe to (e.g., ["BTCUSDT", "ETHUSDT"])
    pub fn new(symbols: Vec<String>) -> Self {
        let (update_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

        Self {
            ws_url: BINANCE_WS_URL.to_string(),
            price_cache: PriceCache::new(),
            update_tx,
            symbols,
            reconnect_delay: Duration::from_secs(1),
            freshness: OnceLock::new(),
        }
    }

    /// Get a reference to the price cache
    pub fn price_cache(&self) -> &PriceCache {
        &self.price_cache
    }

    /// Attach a shared freshness tracker for the data plane.
    pub fn set_freshness(&self, freshness: Arc<crate::data_plane::DataPlaneFreshness>) {
        if self.freshness.set(Arc::clone(&freshness)).is_ok() {
            freshness.set_subscription_count(
                crate::data_plane::DataSource::BinanceSpot,
                self.symbols.len() as u64,
            );
            freshness.set_source_connected(crate::data_plane::DataSource::BinanceSpot, false);
        }
    }

    /// Subscribe to price updates
    pub fn subscribe(&self) -> broadcast::Receiver<PriceUpdate> {
        self.update_tx.subscribe()
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, text: &str) {
        // Try parsing as aggregated trade
        if let Ok(trade) = serde_json::from_str::<BinanceAggTrade>(text) {
            self.process_trade(
                &trade.symbol,
                &trade.price,
                &trade.quantity,
                trade.trade_time,
            )
            .await;
            return;
        }

        // Try parsing as regular trade
        if let Ok(trade) = serde_json::from_str::<BinanceTrade>(text) {
            self.process_trade(
                &trade.symbol,
                &trade.price,
                &trade.quantity,
                trade.trade_time,
            )
            .await;
            return;
        }

        // Log unrecognized messages
        debug!(
            "Unrecognized Binance message: {}",
            &text[..text.len().min(100)]
        );
    }

    /// Process a trade update
    async fn process_trade(&self, symbol: &str, price_str: &str, qty_str: &str, timestamp_ms: u64) {
        let price = match price_str.parse::<Decimal>() {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to parse price '{}': {}", price_str, e);
                return;
            }
        };
        let quantity = match qty_str.parse::<Decimal>() {
            Ok(q) => Some(q),
            Err(e) => {
                warn!("Failed to parse quantity '{}': {}", qty_str, e);
                None
            }
        };

        let timestamp =
            DateTime::from_timestamp_millis(timestamp_ms as i64).unwrap_or_else(Utc::now);

        // Update cache
        self.price_cache
            .update(symbol, price, quantity, timestamp)
            .await;

        // Record per-symbol freshness for the data plane.
        if let Some(f) = self.freshness.get() {
            f.record_update(crate::data_plane::DataSource::BinanceSpot, symbol);
        }

        // Broadcast update
        let update = PriceUpdate {
            symbol: symbol.to_string(),
            price,
            quantity,
            timestamp,
        };

        // Ignore send errors (no subscribers)
        let _ = self.update_tx.send(update);
    }

    /// Test-only hook: inject a raw WebSocket message into the parser/broadcast path.
    #[cfg(test)]
    pub async fn ingest_test_message(&self, text: &str) {
        self.handle_message(text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // =========================================================================
    // Characterization tests — replay realistic WS JSON and verify output
    // =========================================================================

    /// Replay an aggregated trade JSON and verify PriceUpdate broadcast.
    #[tokio::test]
    async fn characterization_agg_trade_produces_price_update() {
        let ws = BinanceWebSocket::new(vec!["BTCUSDT".into()]);
        let mut rx = ws.subscribe();

        let json = r#"{"e":"aggTrade","E":1700000000000,"s":"BTCUSDT","p":"43250.50","q":"0.123","T":1700000000000}"#;
        ws.handle_message(json).await;

        let update = rx.try_recv().expect("should receive PriceUpdate");
        assert_eq!(update.symbol, "BTCUSDT");
        assert_eq!(update.price, dec!(43250.50));
        assert_eq!(update.quantity, Some(dec!(0.123)));
    }

    /// Replay a regular trade JSON and verify PriceUpdate broadcast.
    #[tokio::test]
    async fn characterization_regular_trade_produces_price_update() {
        let ws = BinanceWebSocket::new(vec!["ETHUSDT".into()]);
        let mut rx = ws.subscribe();

        let json = r#"{"e":"trade","E":1700000000000,"s":"ETHUSDT","p":"2150.75","q":"1.5","T":1700000000000}"#;
        ws.handle_message(json).await;

        let update = rx.try_recv().expect("should receive PriceUpdate");
        assert_eq!(update.symbol, "ETHUSDT");
        assert_eq!(update.price, dec!(2150.75));
    }

    /// Price cache should be updated after processing a trade.
    #[tokio::test]
    async fn characterization_trade_updates_price_cache() {
        let ws = BinanceWebSocket::new(vec!["SOLUSDT".into()]);

        let json = r#"{"e":"aggTrade","E":1700000000000,"s":"SOLUSDT","p":"98.50","q":"10","T":1700000000000}"#;
        ws.handle_message(json).await;

        let cached = ws.price_cache().get("SOLUSDT").await;
        assert!(cached.is_some(), "price cache should be updated");
        assert_eq!(cached.unwrap().price, dec!(98.50));
    }

    /// Freshness tracker should record updates when attached.
    #[tokio::test]
    async fn characterization_freshness_recorded_on_trade() {
        let ws = BinanceWebSocket::new(vec!["BTCUSDT".into()]);
        let freshness = std::sync::Arc::new(crate::data_plane::DataPlaneFreshness::new());
        ws.set_freshness(freshness.clone());

        let json = r#"{"e":"aggTrade","E":1700000000000,"s":"BTCUSDT","p":"43000","q":"0.5","T":1700000000000}"#;
        ws.handle_message(json).await;

        let staleness = freshness.staleness(crate::data_plane::DataSource::BinanceSpot, "BTCUSDT");
        assert!(staleness.is_some(), "freshness should be recorded");
        assert!(staleness.unwrap() < 1.0);
    }

    /// Invalid price string should not crash or produce an update.
    #[tokio::test]
    async fn characterization_invalid_price_no_crash() {
        let ws = BinanceWebSocket::new(vec!["BTCUSDT".into()]);
        let mut rx = ws.subscribe();

        let json = r#"{"e":"aggTrade","E":1700000000000,"s":"BTCUSDT","p":"not_a_number","q":"0.5","T":1700000000000}"#;
        ws.handle_message(json).await;

        assert!(
            rx.try_recv().is_err(),
            "invalid price should not produce update"
        );
    }
}
