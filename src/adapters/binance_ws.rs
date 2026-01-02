//! Binance WebSocket adapter for real-time spot price data
//!
//! Connects to Binance's WebSocket API to receive live trade updates for
//! BTC, ETH, and SOL. Maintains rolling price windows for momentum calculation.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio::time::interval;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use url::Url;

use crate::error::{PloyError, Result};

/// Binance WebSocket URL for spot market streams
const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";

/// How often to send ping frames
const PING_INTERVAL_SECS: u64 = 30;

/// Maximum reconnection delay
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Price update broadcast channel capacity
const CHANNEL_CAPACITY: usize = 1000;

/// How many price samples to keep per symbol
const MAX_PRICE_HISTORY: usize = 60;

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
    pub timestamp: DateTime<Utc>,
}

/// Spot price with historical data for momentum calculation
#[derive(Debug, Clone)]
pub struct SpotPrice {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    /// Price history for momentum calculation (newest first)
    history: VecDeque<(Decimal, DateTime<Utc>)>,
}

impl SpotPrice {
    pub fn new(price: Decimal, timestamp: DateTime<Utc>) -> Self {
        let mut history = VecDeque::with_capacity(MAX_PRICE_HISTORY);
        history.push_front((price, timestamp));
        Self {
            price,
            timestamp,
            history,
        }
    }

    /// Update with new price, maintaining history
    pub fn update(&mut self, price: Decimal, timestamp: DateTime<Utc>) {
        self.price = price;
        self.timestamp = timestamp;
        self.history.push_front((price, timestamp));

        // Keep bounded history
        while self.history.len() > MAX_PRICE_HISTORY {
            self.history.pop_back();
        }
    }

    /// Get price from N seconds ago
    pub fn price_secs_ago(&self, secs: u64) -> Option<Decimal> {
        let target_time = self.timestamp - chrono::Duration::seconds(secs as i64);

        // Find the closest price at or before target time
        for (price, ts) in &self.history {
            if *ts <= target_time {
                return Some(*price);
            }
        }

        // If no exact match, return oldest available
        self.history.back().map(|(p, _)| *p)
    }

    /// Calculate momentum over N seconds: (current - past) / past
    pub fn momentum(&self, lookback_secs: u64) -> Option<Decimal> {
        let past_price = self.price_secs_ago(lookback_secs)?;
        if past_price.is_zero() {
            return None;
        }
        Some((self.price - past_price) / past_price)
    }

    /// Get price 1 second ago
    pub fn price_1s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(1)
    }

    /// Get price 5 seconds ago
    pub fn price_5s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(5)
    }

    /// Get price 15 seconds ago
    pub fn price_15s_ago(&self) -> Option<Decimal> {
        self.price_secs_ago(15)
    }
}

/// Thread-safe cache for spot prices
#[derive(Debug, Clone, Default)]
pub struct PriceCache {
    prices: Arc<RwLock<HashMap<String, SpotPrice>>>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update price for a symbol
    pub async fn update(&self, symbol: &str, price: Decimal, timestamp: DateTime<Utc>) {
        let mut prices = self.prices.write().await;

        if let Some(spot) = prices.get_mut(symbol) {
            spot.update(price, timestamp);
        } else {
            prices.insert(symbol.to_string(), SpotPrice::new(price, timestamp));
        }
    }

    /// Get current spot price for a symbol
    pub async fn get(&self, symbol: &str) -> Option<SpotPrice> {
        let prices = self.prices.read().await;
        prices.get(symbol).cloned()
    }

    /// Get all current prices
    pub async fn get_all(&self) -> HashMap<String, SpotPrice> {
        let prices = self.prices.read().await;
        prices.clone()
    }

    /// Calculate momentum for a symbol
    pub async fn momentum(&self, symbol: &str, lookback_secs: u64) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.momentum(lookback_secs)
    }

    /// Get number of tracked symbols
    pub async fn len(&self) -> usize {
        let prices = self.prices.read().await;
        prices.len()
    }

    /// Check if cache is empty
    pub async fn is_empty(&self) -> bool {
        let prices = self.prices.read().await;
        prices.is_empty()
    }
}

/// Binance WebSocket client for real-time price data
pub struct BinanceWebSocket {
    ws_url: String,
    price_cache: PriceCache,
    update_tx: broadcast::Sender<PriceUpdate>,
    symbols: Vec<String>,
    reconnect_delay: Duration,
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
        }
    }

    /// Get a reference to the price cache
    pub fn price_cache(&self) -> &PriceCache {
        &self.price_cache
    }

    /// Subscribe to price updates
    pub fn subscribe(&self) -> broadcast::Receiver<PriceUpdate> {
        self.update_tx.subscribe()
    }

    /// Build the WebSocket URL with stream subscriptions
    fn build_url(&self) -> String {
        // Use aggregated trades for efficiency
        let streams: Vec<String> = self
            .symbols
            .iter()
            .map(|s| format!("{}@aggTrade", s.to_lowercase()))
            .collect();

        format!("{}/{}", self.ws_url, streams.join("/"))
    }

    /// Run the WebSocket connection with automatic reconnection
    pub async fn run(&self) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(MAX_RECONNECT_DELAY_SECS);

        info!("Starting Binance WebSocket for symbols: {:?}", self.symbols);

        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    info!("Binance WebSocket connection closed normally");
                    attempt = 0;
                }
                Err(e) => {
                    attempt += 1;
                    error!(
                        "Binance WebSocket error (attempt {}): {}",
                        attempt, e
                    );
                }
            }

            // Calculate backoff with jitter
            let base_delay = self.reconnect_delay * attempt.min(10);
            let delay = base_delay.min(max_delay);

            // Add jitter: ±25%
            let jitter_range = delay.as_millis() as u64 / 4;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let jitter = Duration::from_millis(seed % jitter_range.max(1));
            let final_delay = delay + jitter;

            info!(
                "Reconnecting to Binance in {:?} (attempt {})",
                final_delay, attempt + 1
            );
            tokio::time::sleep(final_delay).await;
        }
    }

    /// Connect and stream price data
    async fn connect_and_stream(&self) -> Result<()> {
        let url = self.build_url();
        let url = Url::parse(&url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to Binance WebSocket: {}", url);

        let (ws_stream, _) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async(&url),
        )
        .await
        .map_err(|_| PloyError::Internal("Binance WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

        info!("Connected to Binance WebSocket");

        let (mut write, mut read) = ws_stream.split();
        let mut ping_interval = interval(Duration::from_secs(PING_INTERVAL_SECS));

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
                            info!("Received close frame from Binance");
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(PloyError::WebSocket(e));
                        }
                        None => {
                            info!("Binance WebSocket stream ended");
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
                    debug!("Sent ping to Binance");
                }
            }
        }

        Ok(())
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, text: &str) {
        // Try parsing as aggregated trade
        if let Ok(trade) = serde_json::from_str::<BinanceAggTrade>(text) {
            self.process_trade(&trade.symbol, &trade.price, trade.trade_time).await;
            return;
        }

        // Try parsing as regular trade
        if let Ok(trade) = serde_json::from_str::<BinanceTrade>(text) {
            self.process_trade(&trade.symbol, &trade.price, trade.trade_time).await;
            return;
        }

        // Log unrecognized messages
        debug!("Unrecognized Binance message: {}", &text[..text.len().min(100)]);
    }

    /// Process a trade update
    async fn process_trade(&self, symbol: &str, price_str: &str, timestamp_ms: u64) {
        let price = match price_str.parse::<Decimal>() {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to parse price '{}': {}", price_str, e);
                return;
            }
        };

        let timestamp = DateTime::from_timestamp_millis(timestamp_ms as i64)
            .unwrap_or_else(Utc::now);

        // Update cache
        self.price_cache.update(symbol, price, timestamp).await;

        // Broadcast update
        let update = PriceUpdate {
            symbol: symbol.to_string(),
            price,
            timestamp,
        };

        // Ignore send errors (no subscribers)
        let _ = self.update_tx.send(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_spot_price_momentum() {
        let now = Utc::now();
        let mut spot = SpotPrice::new(dec!(100), now - chrono::Duration::seconds(10));

        // Add price history
        spot.update(dec!(101), now - chrono::Duration::seconds(5));
        spot.update(dec!(102), now);

        // Current price should be latest
        assert_eq!(spot.price, dec!(102));

        // Momentum should be positive (price increased)
        let momentum = spot.momentum(5);
        assert!(momentum.is_some());
        // (102 - 101) / 101 ≈ 0.0099
        let m = momentum.unwrap();
        assert!(m > Decimal::ZERO);
    }

    #[test]
    fn test_price_history_bounded() {
        let now = Utc::now();
        let mut spot = SpotPrice::new(dec!(100), now);

        // Add more than MAX_PRICE_HISTORY entries
        for i in 0..MAX_PRICE_HISTORY + 10 {
            spot.update(
                Decimal::from(100 + i as i64),
                now + chrono::Duration::seconds(i as i64),
            );
        }

        // History should be bounded
        assert!(spot.history.len() <= MAX_PRICE_HISTORY);
    }

    #[tokio::test]
    async fn test_price_cache() {
        let cache = PriceCache::new();
        let now = Utc::now();

        cache.update("BTCUSDT", dec!(50000), now).await;
        cache.update("ETHUSDT", dec!(3000), now).await;

        let btc = cache.get("BTCUSDT").await;
        assert!(btc.is_some());
        assert_eq!(btc.unwrap().price, dec!(50000));

        let eth = cache.get("ETHUSDT").await;
        assert!(eth.is_some());
        assert_eq!(eth.unwrap().price, dec!(3000));

        assert_eq!(cache.len().await, 2);
    }
}
