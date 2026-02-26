//! Chainlink RTDS WebSocket adapter for real-time oracle price data
//!
//! Connects to Polymarket's Chainlink Real-Time Data Stream (RTDS) WebSocket
//! to receive live oracle price updates for BTC, ETH, SOL, and XRP.
//! Maintains rolling price windows for momentum and volatility calculation.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use url::Url;

use crate::error::{PloyError, Result};

/// Chainlink RTDS WebSocket endpoint (Polymarket-hosted)
const CHAINLINK_RTDS_WS_URL: &str = "wss://ws-live-data.polymarket.com";

/// Target host for proxy CONNECT
const CHAINLINK_WS_HOST: &str = "ws-live-data.polymarket.com";
const CHAINLINK_WS_PORT: u16 = 443;

/// Server requires frequent pings to maintain connection
const PING_INTERVAL_SECS: u64 = 5;

/// Maximum reconnection delay with exponential backoff
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Broadcast channel capacity for price updates
const CHANNEL_CAPACITY: usize = 1000;

/// Maximum price samples per symbol (1 sample/sec = ~83 min window)
const MAX_PRICE_HISTORY: usize = 5_000;

// ---------------------------------------------------------------------------
// Proxy helpers (mirrors binance_ws.rs pattern)
// ---------------------------------------------------------------------------

/// Get proxy URL from environment variables
fn get_proxy_url() -> Option<String> {
    std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .or_else(|_| std::env::var("http_proxy"))
        .or_else(|_| std::env::var("ALL_PROXY"))
        .or_else(|_| std::env::var("all_proxy"))
        .ok()
}

/// Parse proxy URL into host and port
fn parse_proxy_url(proxy_url: &str) -> Option<(String, u16)> {
    let url = if proxy_url.contains("://") {
        Url::parse(proxy_url).ok()?
    } else {
        Url::parse(&format!("http://{}", proxy_url)).ok()?
    };

    let host = url.host_str()?.to_string();
    let port = url.port().unwrap_or(8080);
    Some((host, port))
}

/// Connect to target host through HTTP CONNECT proxy
async fn connect_via_proxy(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    debug!(
        "Connecting to {}:{} via proxy {}:{}",
        target_host, target_port, proxy_host, proxy_port
    );

    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&proxy_addr))
        .await
        .map_err(|_| PloyError::Internal(format!("Proxy connection timeout: {}", proxy_addr)))?
        .map_err(|e| PloyError::Internal(format!("Failed to connect to proxy: {}", e)))?;

    let connect_request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\n\r\n",
        target_host, target_port, target_host, target_port
    );

    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(connect_request.as_bytes())
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to send CONNECT: {}", e)))?;

    let mut buf_reader = BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to read proxy response: {}", e)))?;

    if !response_line.contains("200") {
        return Err(PloyError::Internal(format!(
            "Proxy CONNECT failed: {}",
            response_line.trim()
        )));
    }

    // Consume remaining headers
    loop {
        let mut line = String::new();
        buf_reader
            .read_line(&mut line)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to read proxy headers: {}", e)))?;
        if line.trim().is_empty() {
            break;
        }
    }

    let reader = buf_reader.into_inner();
    let stream = reader
        .reunite(writer)
        .map_err(|e| PloyError::Internal(format!("Failed to reunite stream: {}", e)))?;

    debug!(
        "Proxy tunnel established to {}:{}",
        target_host, target_port
    );
    Ok(stream)
}

/// Connect WebSocket, using proxy if available
async fn connect_websocket_with_proxy(
    url: &Url,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let host = url.host_str().unwrap_or(CHAINLINK_WS_HOST);
    let port = url.port().unwrap_or(CHAINLINK_WS_PORT);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!(
                "Using proxy {}:{} for Chainlink RTDS WebSocket",
                proxy_host, proxy_port
            );

            let tcp_stream = connect_via_proxy(&proxy_host, proxy_port, host, port).await?;

            let connector = native_tls::TlsConnector::new()
                .map_err(|e| PloyError::Internal(format!("TLS connector error: {}", e)))?;
            let connector = tokio_native_tls::TlsConnector::from(connector);

            let tls_stream = connector
                .connect(host, tcp_stream)
                .await
                .map_err(|e| PloyError::Internal(format!("TLS handshake failed: {}", e)))?;

            let maybe_tls = MaybeTlsStream::NativeTls(tls_stream);

            let (ws_stream, _response) = tokio_tungstenite::client_async(url.as_str(), maybe_tls)
                .await
                .map_err(|e| PloyError::Internal(format!("WebSocket handshake failed: {}", e)))?;

            return Ok(ws_stream);
        }
    }

    // No proxy — connect directly
    let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(url))
        .await
        .map_err(|_| PloyError::Internal("Chainlink RTDS WebSocket connection timeout".into()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}

// ---------------------------------------------------------------------------
// Symbol mapping: Chainlink RTDS <-> Binance
// ---------------------------------------------------------------------------

/// Convert a Chainlink RTDS symbol (e.g. "btc/usd") to the Binance ticker equivalent
pub fn to_binance_symbol(chainlink: &str) -> Option<&'static str> {
    match chainlink {
        "btc/usd" => Some("BTCUSDT"),
        "eth/usd" => Some("ETHUSDT"),
        "sol/usd" => Some("SOLUSDT"),
        "xrp/usd" => Some("XRPUSDT"),
        _ => None,
    }
}

/// Convert a Binance ticker (e.g. "BTCUSDT") to the Chainlink RTDS symbol
pub fn to_chainlink_symbol(binance: &str) -> Option<&'static str> {
    match binance {
        "BTCUSDT" => Some("btc/usd"),
        "ETHUSDT" => Some("eth/usd"),
        "SOLUSDT" => Some("sol/usd"),
        "XRPUSDT" => Some("xrp/usd"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Wire protocol types
// ---------------------------------------------------------------------------

/// Subscribe message sent to the RTDS WebSocket
#[derive(Debug, serde::Serialize)]
struct SubscribeMessage {
    action: String,
    subscriptions: Vec<Subscription>,
}

#[derive(Debug, serde::Serialize)]
struct Subscription {
    topic: String,
    #[serde(rename = "type")]
    sub_type: String,
    filters: String,
}

/// Inbound price payload from RTDS
#[derive(Debug, Deserialize)]
struct RtdsPayload {
    symbol: String,
    timestamp: u64,
    value: f64,
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Price update event broadcast to subscribers
#[derive(Debug, Clone)]
pub struct ChainlinkUpdate {
    pub symbol: String,
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// Spot price with rolling history for analytics
#[derive(Debug, Clone)]
pub struct ChainlinkSpot {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
    /// Price history (newest first), bounded to MAX_PRICE_HISTORY
    pub history: VecDeque<(Decimal, DateTime<Utc>)>,
}

impl ChainlinkSpot {
    /// Initialize with a first sample
    pub fn new(price: Decimal, timestamp: DateTime<Utc>) -> Self {
        let mut history = VecDeque::with_capacity(MAX_PRICE_HISTORY);
        history.push_front((price, timestamp));
        Self {
            price,
            timestamp,
            history,
        }
    }

    /// Add a new sample, downsampling to at most 1 sample/sec
    pub fn update(&mut self, price: Decimal, timestamp: DateTime<Utc>) {
        self.price = price;
        self.timestamp = timestamp;

        // Downsample: collapse updates within the same second
        if let Some((front_price, front_ts)) = self.history.front_mut() {
            if front_ts.timestamp() == timestamp.timestamp() {
                *front_price = price;
                *front_ts = timestamp;
                return;
            }
        }

        self.history.push_front((price, timestamp));

        while self.history.len() > MAX_PRICE_HISTORY {
            self.history.pop_back();
        }
    }

    /// Get price from N seconds ago (closest sample at or before target time)
    pub fn price_secs_ago(&self, secs: u64) -> Option<Decimal> {
        let target_time = self.timestamp - chrono::Duration::seconds(secs as i64);

        for (price, ts) in &self.history {
            if *ts <= target_time {
                return Some(*price);
            }
        }

        // Fall back to oldest available
        self.history.back().map(|(p, _)| *p)
    }

    /// Momentum: (current - past) / past
    pub fn momentum(&self, lookback_secs: u64) -> Option<Decimal> {
        let past_price = self.price_secs_ago(lookback_secs)?;
        if past_price.is_zero() {
            return None;
        }
        Some((self.price - past_price) / past_price)
    }

    /// Rolling volatility (standard deviation of returns) over N seconds
    pub fn volatility(&self, lookback_secs: u64) -> Option<Decimal> {
        if self.history.len() < 10 {
            return None;
        }

        let cutoff_time = self.timestamp - chrono::Duration::seconds(lookback_secs as i64);

        let prices: Vec<Decimal> = self
            .history
            .iter()
            .filter(|(_, ts)| *ts >= cutoff_time)
            .map(|(p, _)| *p)
            .collect();

        if prices.len() < 5 {
            return None;
        }

        // Returns between consecutive samples (newest to oldest)
        let mut returns = Vec::with_capacity(prices.len() - 1);
        for i in 0..prices.len() - 1 {
            if !prices[i + 1].is_zero() {
                let ret = (prices[i] - prices[i + 1]) / prices[i + 1];
                returns.push(ret);
            }
        }

        if returns.is_empty() {
            return None;
        }

        let sum: Decimal = returns.iter().copied().sum();
        let mean = sum / Decimal::from(returns.len());

        let variance_sum: Decimal = returns
            .iter()
            .map(|r| {
                let diff = *r - mean;
                diff * diff
            })
            .sum();
        let variance = variance_sum / Decimal::from(returns.len());

        decimal_sqrt(variance)
    }
}

/// Approximate square root for Decimal using Newton's method
fn decimal_sqrt(x: Decimal) -> Option<Decimal> {
    if x < Decimal::ZERO {
        return None;
    }
    if x.is_zero() {
        return Some(Decimal::ZERO);
    }

    let mut guess = x / Decimal::TWO;
    let tolerance = Decimal::new(1, 10); // 0.0000000001

    for _ in 0..50 {
        if guess.is_zero() {
            return Some(Decimal::ZERO);
        }
        let next_guess = (guess + x / guess) / Decimal::TWO;
        let diff = if next_guess > guess {
            next_guess - guess
        } else {
            guess - next_guess
        };
        if diff < tolerance {
            return Some(next_guess);
        }
        guess = next_guess;
    }

    Some(guess)
}

// ---------------------------------------------------------------------------
// Price cache
// ---------------------------------------------------------------------------

/// Thread-safe cache of Chainlink oracle spot prices
#[derive(Debug, Clone, Default)]
pub struct ChainlinkPriceCache {
    prices: Arc<RwLock<HashMap<String, ChainlinkSpot>>>,
}

impl ChainlinkPriceCache {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a clone of the current spot for a symbol
    pub async fn get(&self, symbol: &str) -> Option<ChainlinkSpot> {
        let prices = self.prices.read().await;
        prices.get(symbol).cloned()
    }

    /// Insert or update a price sample
    pub async fn update(&self, symbol: &str, price: Decimal, timestamp: DateTime<Utc>) {
        let mut prices = self.prices.write().await;

        if let Some(spot) = prices.get_mut(symbol) {
            spot.update(price, timestamp);
        } else {
            prices.insert(symbol.to_string(), ChainlinkSpot::new(price, timestamp));
        }
    }
}

// ---------------------------------------------------------------------------
// Main client
// ---------------------------------------------------------------------------

/// Chainlink RTDS WebSocket client for real-time oracle price data
pub struct ChainlinkRtds {
    update_tx: broadcast::Sender<ChainlinkUpdate>,
    price_cache: ChainlinkPriceCache,
    symbols: Vec<String>,
}

impl ChainlinkRtds {
    /// Create a new Chainlink RTDS client.
    ///
    /// # Arguments
    /// * `symbols` - Chainlink-style symbols to subscribe to (e.g. `["btc/usd", "eth/usd"]`)
    pub fn new(symbols: Vec<String>) -> Self {
        let (update_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

        Self {
            update_tx,
            price_cache: ChainlinkPriceCache::new(),
            symbols,
        }
    }

    /// Get a reference to the price cache
    pub fn price_cache(&self) -> &ChainlinkPriceCache {
        &self.price_cache
    }

    /// Subscribe to real-time price updates
    pub fn subscribe(&self) -> broadcast::Receiver<ChainlinkUpdate> {
        self.update_tx.subscribe()
    }

    /// Run the WebSocket connection loop with exponential backoff reconnection
    pub async fn run(&self) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(MAX_RECONNECT_DELAY_SECS);
        let base_delay = Duration::from_secs(1);

        info!(
            "Starting Chainlink RTDS WebSocket for symbols: {:?}",
            self.symbols
        );

        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    info!("Chainlink RTDS WebSocket connection closed normally");
                    attempt = 0;
                }
                Err(e) => {
                    attempt += 1;
                    error!(
                        "Chainlink RTDS WebSocket error (attempt {}): {}",
                        attempt, e
                    );
                }
            }

            // Exponential backoff with jitter
            let delay = (base_delay * attempt.min(10)).min(max_delay);

            let jitter_range = delay.as_millis() as u64 / 4;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let jitter = Duration::from_millis(seed % jitter_range.max(1));
            let final_delay = delay + jitter;

            info!(
                "Reconnecting to Chainlink RTDS in {:?} (attempt {})",
                final_delay,
                attempt + 1
            );
            tokio::time::sleep(final_delay).await;
        }
    }

    /// Connect, subscribe, and stream price data
    async fn connect_and_stream(&self) -> Result<()> {
        let url = Url::parse(CHAINLINK_RTDS_WS_URL)
            .map_err(|e| PloyError::Internal(format!("Invalid RTDS WebSocket URL: {}", e)))?;

        info!("Connecting to Chainlink RTDS: {}", url);

        let ws_stream = connect_websocket_with_proxy(&url).await?;

        info!("Connected to Chainlink RTDS WebSocket");

        let (mut write, mut read) = ws_stream.split();

        use futures_util::{SinkExt, StreamExt};

        // Subscribe to each symbol
        for symbol in &self.symbols {
            let msg = SubscribeMessage {
                action: "subscribe".to_string(),
                subscriptions: vec![Subscription {
                    topic: "crypto_prices_chainlink".to_string(),
                    sub_type: "*".to_string(),
                    filters: format!(r#"{{"symbol":"{}"}}"#, symbol),
                }],
            };

            let payload = serde_json::to_string(&msg)
                .map_err(|e| PloyError::Internal(format!("Failed to serialize subscribe: {}", e)))?;

            write
                .send(Message::Text(payload))
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to send subscribe: {}", e)))?;

            info!("Subscribed to Chainlink RTDS: {}", symbol);
        }

        let mut ping_interval = interval(Duration::from_secs(PING_INTERVAL_SECS));

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
                            info!("Received close frame from Chainlink RTDS");
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(PloyError::WebSocket(e));
                        }
                        None => {
                            info!("Chainlink RTDS WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        error!("Failed to send ping to Chainlink RTDS: {}", e);
                        break;
                    }
                    debug!("Sent ping to Chainlink RTDS");
                }
            }
        }

        Ok(())
    }

    /// Handle an incoming text message from the RTDS stream
    async fn handle_message(&self, text: &str) {
        let payload: RtdsPayload = match serde_json::from_str(text) {
            Ok(p) => p,
            Err(_) => {
                debug!(
                    "Unrecognized Chainlink RTDS message: {}",
                    &text[..text.len().min(200)]
                );
                return;
            }
        };

        // Convert f64 value to Decimal
        let price = match Decimal::try_from(payload.value) {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    "Failed to convert Chainlink price {} to Decimal: {}",
                    payload.value, e
                );
                return;
            }
        };

        let timestamp =
            DateTime::from_timestamp_millis(payload.timestamp as i64).unwrap_or_else(Utc::now);

        // Update cache
        self.price_cache
            .update(&payload.symbol, price, timestamp)
            .await;

        // Broadcast update
        let update = ChainlinkUpdate {
            symbol: payload.symbol,
            price,
            timestamp,
        };

        // Ignore send errors (no subscribers)
        let _ = self.update_tx.send(update);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_chainlink_spot_momentum() {
        let now = Utc::now();
        let mut spot = ChainlinkSpot::new(dec!(50000), now - chrono::Duration::seconds(10));

        spot.update(dec!(50500), now - chrono::Duration::seconds(5));
        spot.update(dec!(51000), now);

        assert_eq!(spot.price, dec!(51000));

        let momentum = spot.momentum(5);
        assert!(momentum.is_some());
        // (51000 - 50500) / 50500 ≈ 0.0099
        let m = momentum.unwrap();
        assert!(m > Decimal::ZERO);
    }

    #[test]
    fn test_chainlink_spot_history_bounded() {
        let now = Utc::now();
        let mut spot = ChainlinkSpot::new(dec!(100), now);

        for i in 0..MAX_PRICE_HISTORY + 10 {
            spot.update(
                Decimal::from(100 + i as i64),
                now + chrono::Duration::seconds(i as i64),
            );
        }

        assert!(spot.history.len() <= MAX_PRICE_HISTORY);
    }

    #[test]
    fn test_chainlink_spot_downsample() {
        let now = Utc::now();
        let mut spot = ChainlinkSpot::new(dec!(100), now);

        // Multiple updates within the same second should collapse
        spot.update(dec!(101), now);
        spot.update(dec!(102), now);

        assert_eq!(spot.history.len(), 1);
        assert_eq!(spot.price, dec!(102));
    }

    #[tokio::test]
    async fn test_chainlink_price_cache() {
        let cache = ChainlinkPriceCache::new();
        let now = Utc::now();

        cache.update("btc/usd", dec!(50000), now).await;
        cache.update("eth/usd", dec!(3000), now).await;

        let btc = cache.get("btc/usd").await;
        assert!(btc.is_some());
        assert_eq!(btc.unwrap().price, dec!(50000));

        let eth = cache.get("eth/usd").await;
        assert!(eth.is_some());
        assert_eq!(eth.unwrap().price, dec!(3000));

        assert!(cache.get("sol/usd").await.is_none());
    }

    #[test]
    fn test_symbol_mapping_roundtrip() {
        assert_eq!(to_binance_symbol("btc/usd"), Some("BTCUSDT"));
        assert_eq!(to_binance_symbol("eth/usd"), Some("ETHUSDT"));
        assert_eq!(to_binance_symbol("sol/usd"), Some("SOLUSDT"));
        assert_eq!(to_binance_symbol("xrp/usd"), Some("XRPUSDT"));
        assert_eq!(to_binance_symbol("doge/usd"), None);

        assert_eq!(to_chainlink_symbol("BTCUSDT"), Some("btc/usd"));
        assert_eq!(to_chainlink_symbol("ETHUSDT"), Some("eth/usd"));
        assert_eq!(to_chainlink_symbol("SOLUSDT"), Some("sol/usd"));
        assert_eq!(to_chainlink_symbol("XRPUSDT"), Some("xrp/usd"));
        assert_eq!(to_chainlink_symbol("DOGEUSDT"), None);
    }

    #[test]
    fn test_chainlink_rtds_new() {
        let symbols = vec!["btc/usd".to_string(), "eth/usd".to_string()];
        let rtds = ChainlinkRtds::new(symbols);

        // Verify we can subscribe
        let _rx = rtds.subscribe();
        assert!(rtds.symbols.len() == 2);
    }
}
