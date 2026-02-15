//! Binance Order Book (LOB) depth stream collector
//!
//! Collects real-time order book data via @depth@100ms stream
//! for lead-lag analysis with Polymarket.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info};
use url::Url;

use crate::error::{PloyError, Result};

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";
const PING_INTERVAL_SECS: u64 = 30;
const MAX_RECONNECT_DELAY_SECS: u64 = 60;
const CHANNEL_CAPACITY: usize = 10000;
const MAX_DEPTH_LEVELS: usize = 20;

/// Binance depth update message
#[derive(Debug, Deserialize)]
pub struct DepthUpdate {
    #[serde(rename = "e")]
    pub event_type: String,
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "u")]
    pub final_update_id: i64,
    #[serde(rename = "b")]
    pub bids: Vec<(String, String)>,
    #[serde(rename = "a")]
    pub asks: Vec<(String, String)>,
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
    pub obi_5: Decimal,  // OBI with top 5 levels
    pub obi_10: Decimal, // OBI with top 10 levels
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
        let obi_5 = book.calculate_obi(5)?;
        let obi_10 = book.calculate_obi(10)?;

        let bid_volume_5: Decimal = book.bids.iter().rev().take(5).map(|(_, q)| *q).sum();
        let ask_volume_5: Decimal = book.asks.iter().take(5).map(|(_, q)| *q).sum();

        Some(LobSnapshot {
            timestamp: book.last_update_time.unwrap_or_else(Utc::now),
            symbol: symbol.to_string(),
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_5,
            obi_10,
            bid_volume_5,
            ask_volume_5,
            update_id: book.last_update_id,
        })
    }

    /// Update order book from depth update
    async fn apply_depth_update(&self, update: &DepthUpdate) -> Option<LobSnapshot> {
        let mut books = self.books.write().await;
        let book = books.entry(update.symbol.clone()).or_default();

        let ts = DateTime::from_timestamp_millis(update.event_time).unwrap_or_else(Utc::now);

        // Apply bid updates
        for (price_str, qty_str) in &update.bids {
            if let (Ok(price), Ok(qty)) = (price_str.parse::<f64>(), qty_str.parse::<f64>()) {
                let price_cents = (price * 100.0).round() as i64;
                let qty_dec = Decimal::try_from(qty).unwrap_or_default();

                if qty == 0.0 {
                    book.bids.remove(&price_cents);
                } else {
                    book.bids.insert(price_cents, qty_dec);
                }
            }
        }

        // Apply ask updates
        for (price_str, qty_str) in &update.asks {
            if let (Ok(price), Ok(qty)) = (price_str.parse::<f64>(), qty_str.parse::<f64>()) {
                let price_cents = (price * 100.0).round() as i64;
                let qty_dec = Decimal::try_from(qty).unwrap_or_default();

                if qty == 0.0 {
                    book.asks.remove(&price_cents);
                } else {
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

        book.last_update_id = update.final_update_id;
        book.last_update_time = Some(ts);

        // Generate snapshot
        let best_bid = book.best_bid()?;
        let best_ask = book.best_ask()?;
        let mid_price = book.mid_price()?;
        let spread_bps = book.spread_bps()?;
        let obi_5 = book.calculate_obi(5)?;
        let obi_10 = book.calculate_obi(10)?;

        let bid_volume_5: Decimal = book.bids.iter().rev().take(5).map(|(_, q)| *q).sum();
        let ask_volume_5: Decimal = book.asks.iter().take(5).map(|(_, q)| *q).sum();

        Some(LobSnapshot {
            timestamp: ts,
            symbol: update.symbol.clone(),
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_5,
            obi_10,
            bid_volume_5,
            ask_volume_5,
            update_id: update.final_update_id,
        })
    }
}

/// Binance LOB WebSocket client
pub struct BinanceDepthStream {
    symbols: Vec<String>,
    cache: LobCache,
    update_tx: broadcast::Sender<LobUpdate>,
}

impl BinanceDepthStream {
    /// Create a new depth stream client
    pub fn new(symbols: Vec<String>) -> Self {
        let (update_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

        Self {
            symbols,
            cache: LobCache::new(),
            update_tx,
        }
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
            .map(|s| format!("{}@depth@100ms", s.to_lowercase()))
            .collect();

        format!("{}/{}", BINANCE_WS_URL, streams.join("/"))
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

        let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(&url))
            .await
            .map_err(|_| PloyError::Internal("Binance WebSocket connection timeout".to_string()))?
            .map_err(PloyError::WebSocket)?;

        info!("Connected to Binance depth stream");

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
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(PloyError::WebSocket(e));
                        }
                        None => {
                            info!("Stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                    debug!("Sent ping");
                }
            }
        }

        Ok(())
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, text: &str) {
        // Parse wrapper format: { "stream": "...", "data": {...} }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            let payload = value.get("data").cloned().unwrap_or(value);

            if let Ok(update) = serde_json::from_value::<DepthUpdate>(payload) {
                if update.event_type == "depthUpdate" {
                    if let Some(snapshot) = self.cache.apply_depth_update(&update).await {
                        let lob_update = LobUpdate {
                            symbol: update.symbol.clone(),
                            snapshot,
                            raw_state: self.cache.get(&update.symbol).await.unwrap_or_default(),
                        };

                        // Broadcast to subscribers
                        let _ = self.update_tx.send(lob_update);
                    }
                }
            }
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
}
