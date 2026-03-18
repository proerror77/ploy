//! Binance Order Book (LOB) depth stream collector
//!
//! Collects real-time order book data via @depth@100ms stream
//! for lead-lag analysis with Polymarket.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info};
use url::Url;

use crate::data_plane::{DataPlaneFreshness, DataSource};
use crate::error::{PloyError, Result};

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/stream?streams=";
const PING_INTERVAL_SECS: u64 = 30;
const MAX_RECONNECT_DELAY_SECS: u64 = 60;
const CHANNEL_CAPACITY: usize = 10000;
const MAX_DEPTH_LEVELS: usize = 20;

/// Binance partial-depth snapshot payload.
#[derive(Debug, Deserialize)]
pub struct DepthSnapshotPayload {
    #[serde(rename = "lastUpdateId", alias = "u")]
    pub update_id: i64,
    #[serde(rename = "E")]
    pub event_time: Option<i64>,
    #[serde(rename = "s")]
    pub symbol: Option<String>,
    #[serde(rename = "bids", alias = "b")]
    pub bids: Vec<(String, String)>,
    #[serde(rename = "asks", alias = "a")]
    pub asks: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
struct CombinedDepthMessage {
    stream: String,
    data: DepthSnapshotPayload,
}

/// Order book state for a symbol
#[derive(Debug, Clone, Default)]
pub struct OrderBookState {
    pub bids: BTreeMap<i64, Decimal>, // price_cents -> qty
    pub asks: BTreeMap<i64, Decimal>,
    pub last_update_id: i64,
    pub last_update_time: Option<DateTime<Utc>>,
}

impl OrderBookState {
    /// Calculate Order Book Imbalance (OBI)
    /// OBI = (bid_volume - ask_volume) / (bid_volume + ask_volume)
    /// Range: -1 (all asks) to +1 (all bids)
    pub fn calculate_obi(&self, levels: usize) -> Option<Decimal> {
        let bid_sum: Decimal = self.bids.iter().rev().take(levels).map(|(_, q)| *q).sum();
        let ask_sum: Decimal = self.asks.iter().take(levels).map(|(_, q)| *q).sum();
        let total = bid_sum + ask_sum;

        if total.is_zero() {
            return None;
        }

        Some((bid_sum - ask_sum) / total)
    }

    /// Get best bid price
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids
            .keys()
            .next_back()
            .map(|&p| Decimal::from(p) / Decimal::from(100))
    }

    /// Get best ask price
    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks
            .keys()
            .next()
            .map(|&p| Decimal::from(p) / Decimal::from(100))
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::from(2)),
            _ => None,
        }
    }

    /// Get spread in basis points
    pub fn spread_bps(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if !bid.is_zero() => {
                Some((ask - bid) / bid * Decimal::from(10000))
            }
            _ => None,
        }
    }
}

/// LOB snapshot for storage/analysis
#[derive(Debug, Clone, Serialize)]
pub struct LobSnapshot {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
    pub mid_price: Decimal,
    pub spread_bps: Decimal,
    pub obi_1: Decimal,  // OBI with top 1 level
    pub obi_2: Decimal,  // OBI with top 2 levels
    pub obi_3: Decimal,  // OBI with top 3 levels
    pub obi_5: Decimal,  // OBI with top 5 levels
    pub obi_10: Decimal, // OBI with top 10 levels
    pub obi_20: Decimal, // OBI with top 20 levels
    pub bid_volume_5: Decimal,
    pub ask_volume_5: Decimal,
    pub update_id: i64,
}

/// LOB update event broadcast to subscribers
#[derive(Debug, Clone)]
pub struct LobUpdate {
    pub symbol: String,
    pub snapshot: LobSnapshot,
    pub raw_state: OrderBookState,
}

/// Thread-safe LOB cache
#[derive(Debug, Clone, Default)]
pub struct LobCache {
    books: Arc<RwLock<std::collections::HashMap<String, OrderBookState>>>,
}

impl LobCache {
    pub fn new() -> Self {
        Self {
            books: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Get order book state for a symbol
    pub async fn get(&self, symbol: &str) -> Option<OrderBookState> {
        let books = self.books.read().await;
        books.get(symbol).cloned()
    }

    /// Get OBI for a symbol
    pub async fn get_obi(&self, symbol: &str, levels: usize) -> Option<Decimal> {
        let books = self.books.read().await;
        books.get(symbol)?.calculate_obi(levels)
    }

    /// Get snapshot for a symbol
    pub async fn get_snapshot(&self, symbol: &str) -> Option<LobSnapshot> {
        let books = self.books.read().await;
        let book = books.get(symbol)?;

        let best_bid = book.best_bid()?;
        let best_ask = book.best_ask()?;
        let mid_price = book.mid_price()?;
        let spread_bps = book.spread_bps()?;
        let obi_1 = book.calculate_obi(1)?;
        let obi_2 = book.calculate_obi(2)?;
        let obi_3 = book.calculate_obi(3)?;
        let obi_5 = book.calculate_obi(5)?;
        let obi_10 = book.calculate_obi(10)?;
        let obi_20 = book.calculate_obi(20)?;

        let bid_volume_5: Decimal = book.bids.iter().rev().take(5).map(|(_, q)| *q).sum();
        let ask_volume_5: Decimal = book.asks.iter().take(5).map(|(_, q)| *q).sum();

        Some(LobSnapshot {
            timestamp: book.last_update_time.unwrap_or_else(Utc::now),
            symbol: symbol.to_string(),
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_1,
            obi_2,
            obi_3,
            obi_5,
            obi_10,
            obi_20,
            bid_volume_5,
            ask_volume_5,
            update_id: book.last_update_id,
        })
    }

    /// Replace order book from a partial depth snapshot.
    async fn apply_depth_snapshot(
        &self,
        symbol: &str,
        update_id: i64,
        event_time_ms: Option<i64>,
        bids: &[(String, String)],
        asks: &[(String, String)],
    ) -> Option<LobSnapshot> {
        let mut books = self.books.write().await;
        let book = books.entry(symbol.to_string()).or_default();

        let ts = event_time_ms
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or_else(Utc::now);
        book.bids.clear();
        book.asks.clear();

        // Apply bid updates
        for (price_str, qty_str) in bids {
            if let Some((price_cents, qty_dec)) = parse_depth_level(price_str, qty_str) {
                if !qty_dec.is_zero() {
                    book.bids.insert(price_cents, qty_dec);
                }
            }
        }

        // Apply ask updates
        for (price_str, qty_str) in asks {
            if let Some((price_cents, qty_dec)) = parse_depth_level(price_str, qty_str) {
                if !qty_dec.is_zero() {
                    book.asks.insert(price_cents, qty_dec);
                }
            }
        }

        // Trim to max levels
        while book.bids.len() > MAX_DEPTH_LEVELS * 2 {
            if let Some(k) = book.bids.keys().next().cloned() {
                book.bids.remove(&k);
            }
        }
        while book.asks.len() > MAX_DEPTH_LEVELS * 2 {
            if let Some(k) = book.asks.keys().next_back().cloned() {
                book.asks.remove(&k);
            }
        }

        book.last_update_id = update_id;
        book.last_update_time = Some(ts);

        // Generate snapshot
        let best_bid = book.best_bid()?;
        let best_ask = book.best_ask()?;
        let mid_price = book.mid_price()?;
        let spread_bps = book.spread_bps()?;
        let obi_1 = book.calculate_obi(1)?;
        let obi_2 = book.calculate_obi(2)?;
        let obi_3 = book.calculate_obi(3)?;
        let obi_5 = book.calculate_obi(5)?;
        let obi_10 = book.calculate_obi(10)?;
        let obi_20 = book.calculate_obi(20)?;

        let bid_volume_5: Decimal = book.bids.iter().rev().take(5).map(|(_, q)| *q).sum();
        let ask_volume_5: Decimal = book.asks.iter().take(5).map(|(_, q)| *q).sum();

        Some(LobSnapshot {
            timestamp: ts,
            symbol: symbol.to_string(),
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_1,
            obi_2,
            obi_3,
            obi_5,
            obi_10,
            obi_20,
            bid_volume_5,
            ask_volume_5,
            update_id,
        })
    }
}

fn parse_depth_level(price_str: &str, qty_str: &str) -> Option<(i64, Decimal)> {
    let price = Decimal::from_str(price_str).ok()?;
    let qty = Decimal::from_str(qty_str).ok()?;
    let price_cents = (price * Decimal::from(100)).round().to_i64()?;
    Some((price_cents, qty))
}

fn symbol_from_stream_name(stream: &str) -> Option<String> {
    let symbol = stream.split('@').next()?.trim();
    if symbol.is_empty() {
        None
    } else {
        Some(symbol.to_ascii_uppercase())
    }
}

/// Binance LOB WebSocket client
pub struct BinanceDepthStream {
    symbols: Vec<String>,
    cache: LobCache,
    update_tx: broadcast::Sender<LobUpdate>,
    freshness: Option<Arc<DataPlaneFreshness>>,
}

impl BinanceDepthStream {
    /// Create a new depth stream client
    pub fn new(symbols: Vec<String>) -> Self {
        let (update_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

        Self {
            symbols,
            cache: LobCache::new(),
            update_tx,
            freshness: None,
        }
    }

    pub fn with_freshness(mut self, freshness: Arc<DataPlaneFreshness>) -> Self {
        self.freshness = Some(freshness);
        self
    }

    /// Get reference to LOB cache
    pub fn cache(&self) -> &LobCache {
        &self.cache
    }

    /// Subscribe to LOB updates
    pub fn subscribe(&self) -> broadcast::Receiver<LobUpdate> {
        self.update_tx.subscribe()
    }

    /// Build WebSocket URL with streams
    fn build_url(&self) -> String {
        let streams: Vec<String> = self
            .symbols
            .iter()
            .map(|s| format!("{}@depth{}@100ms", s.to_lowercase(), MAX_DEPTH_LEVELS))
            .collect();

        format!("{}{}", BINANCE_WS_URL, streams.join("/"))
    }

    /// Run the WebSocket connection with auto-reconnect
    pub async fn run(&self) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(MAX_RECONNECT_DELAY_SECS);

        info!("Starting Binance depth stream for: {:?}", self.symbols);

        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    info!("Binance depth stream closed normally");
                    attempt = 0;
                }
                Err(e) => {
                    attempt += 1;
                    error!("Binance depth stream error (attempt {}): {}", attempt, e);
                }
            }

            let delay = Duration::from_secs(1) * attempt.min(10);
            let delay = delay.min(max_delay);

            info!("Reconnecting in {:?}...", delay);
            tokio::time::sleep(delay).await;
        }
    }

    /// Connect and stream depth data
    async fn connect_and_stream(&self) -> Result<()> {
        let url = self.build_url();
        let url = Url::parse(&url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to Binance depth stream: {}", url);

        let (ws_stream, _) =
            tokio::time::timeout(Duration::from_secs(10), connect_async(url.as_str()))
                .await
                .map_err(|_| {
                    PloyError::Internal("Binance WebSocket connection timeout".to_string())
                })?
                .map_err(PloyError::WebSocket)?;

        info!("Connected to Binance depth stream");
        if let Some(freshness) = &self.freshness {
            freshness.set_source_connected(DataSource::BinanceLob, true);
            freshness.set_subscription_count(DataSource::BinanceLob, self.symbols.len() as u64);
        }

        let (mut write, mut read) = ws_stream.split();
        let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));

        use futures_util::{SinkExt, StreamExt};

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_message(&text).await;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            if let Err(e) = write.send(Message::Pong(data)).await {
                                error!("Failed to send pong: {}", e);
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("Received close frame");
                            if let Some(freshness) = &self.freshness {
                                freshness.set_source_connected(DataSource::BinanceLob, false);
                            }
                            break;
                        }
                        Some(Err(e)) => {
                            if let Some(freshness) = &self.freshness {
                                freshness.set_source_connected(DataSource::BinanceLob, false);
                            }
                            return Err(PloyError::WebSocket(e));
                        }
                        None => {
                            info!("Stream ended");
                            if let Some(freshness) = &self.freshness {
                                freshness.set_source_connected(DataSource::BinanceLob, false);
                            }
                            break;
                        }
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                    debug!("Sent ping");
                }
            }
        }

        if let Some(freshness) = &self.freshness {
            freshness.set_source_connected(DataSource::BinanceLob, false);
        }

        Ok(())
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, text: &str) {
        if let Ok(message) = serde_json::from_str::<CombinedDepthMessage>(text) {
            let symbol = message
                .data
                .symbol
                .clone()
                .or_else(|| symbol_from_stream_name(&message.stream));
            if let Some(symbol) = symbol {
                self.process_snapshot(
                    &symbol,
                    message.data.update_id,
                    message.data.event_time,
                    &message.data.bids,
                    &message.data.asks,
                )
                .await;
            }
            return;
        }

        if let Ok(payload) = serde_json::from_str::<DepthSnapshotPayload>(text) {
            if let Some(symbol) = payload.symbol.clone() {
                self.process_snapshot(
                    &symbol,
                    payload.update_id,
                    payload.event_time,
                    &payload.bids,
                    &payload.asks,
                )
                .await;
            }
        }
    }

    async fn process_snapshot(
        &self,
        symbol: &str,
        update_id: i64,
        event_time: Option<i64>,
        bids: &[(String, String)],
        asks: &[(String, String)],
    ) {
        if let Some(snapshot) = self
            .cache
            .apply_depth_snapshot(symbol, update_id, event_time, bids, asks)
            .await
        {
            if let Some(freshness) = &self.freshness {
                freshness.record_update(DataSource::BinanceLob, symbol);
            }
            let lob_update = LobUpdate {
                symbol: symbol.to_string(),
                snapshot,
                raw_state: self.cache.get(symbol).await.unwrap_or_default(),
            };
            let _ = self.update_tx.send(lob_update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_obi_calculation() {
        let mut book = OrderBookState::default();

        // Add some bids (price in cents)
        book.bids.insert(10000, dec!(100)); // $100.00, qty 100
        book.bids.insert(9990, dec!(50)); // $99.90, qty 50

        // Add some asks
        book.asks.insert(10010, dec!(80)); // $100.10, qty 80
        book.asks.insert(10020, dec!(40)); // $100.20, qty 40

        // OBI = (150 - 120) / (150 + 120) = 30 / 270 = 0.111...
        let obi = book.calculate_obi(2).unwrap();
        assert!(obi > dec!(0.1) && obi < dec!(0.12));
    }

    #[test]
    fn test_mid_price() {
        let mut book = OrderBookState::default();
        book.bids.insert(10000, dec!(100)); // $100.00
        book.asks.insert(10010, dec!(80)); // $100.10

        let mid = book.mid_price().unwrap();
        assert_eq!(mid, dec!(100.05));
    }

    #[test]
    fn test_build_url_uses_combined_partial_depth_streams() {
        let stream = BinanceDepthStream::new(vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
        assert_eq!(
            stream.build_url(),
            "wss://stream.binance.com:9443/stream?streams=btcusdt@depth20@100ms/ethusdt@depth20@100ms"
        );
    }

    #[tokio::test]
    async fn test_apply_depth_snapshot_replaces_book_state() {
        let cache = LobCache::new();

        cache
            .apply_depth_snapshot(
                "BTCUSDT",
                10,
                None,
                &[
                    ("100.00".to_string(), "2.0".to_string()),
                    ("99.90".to_string(), "1.0".to_string()),
                ],
                &[
                    ("100.10".to_string(), "3.0".to_string()),
                    ("100.20".to_string(), "1.0".to_string()),
                ],
            )
            .await
            .expect("first snapshot should build");

        let snapshot = cache
            .apply_depth_snapshot(
                "BTCUSDT",
                11,
                None,
                &[("101.00".to_string(), "4.0".to_string())],
                &[("101.10".to_string(), "5.0".to_string())],
            )
            .await
            .expect("replacement snapshot should build");

        assert_eq!(snapshot.best_bid, dec!(101));
        assert_eq!(snapshot.best_ask, dec!(101.1));
        assert_eq!(snapshot.obi_1, dec!(-1) / dec!(9));
        assert_eq!(snapshot.obi_2, dec!(-1) / dec!(9));
        assert_eq!(snapshot.obi_3, dec!(-1) / dec!(9));
        assert_eq!(snapshot.obi_20, dec!(-1) / dec!(9));
        let state = cache.get("BTCUSDT").await.expect("book state");
        assert_eq!(state.bids.len(), 1);
        assert_eq!(state.asks.len(), 1);
    }
}
