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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, RwLock};
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use url::Url;

use crate::error::{PloyError, Result};

/// Binance WebSocket URL for spot market streams
const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws";

/// Binance WebSocket host for proxy CONNECT
const BINANCE_WS_HOST: &str = "stream.binance.com";
const BINANCE_WS_PORT: u16 = 9443;

/// How often to send ping frames
const PING_INTERVAL_SECS: u64 = 30;

/// Maximum reconnection delay
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Price update broadcast channel capacity
const CHANNEL_CAPACITY: usize = 1000;

/// How many price samples to keep per symbol (need 120+ for volatility calculation)
const MAX_PRICE_HISTORY: usize = 300;

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
    // Handle URLs like "http://127.0.0.1:7897" or "127.0.0.1:7897"
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

    // Connect to proxy
    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(&proxy_addr),
    )
    .await
    .map_err(|_| PloyError::Internal(format!("Proxy connection timeout: {}", proxy_addr)))?
    .map_err(|e| PloyError::Internal(format!("Failed to connect to proxy: {}", e)))?;

    // Send HTTP CONNECT request
    let connect_request = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: keep-alive\r\n\r\n",
        target_host, target_port, target_host, target_port
    );

    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(connect_request.as_bytes())
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to send CONNECT: {}", e)))?;

    // Read response
    let mut buf_reader = BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to read proxy response: {}", e)))?;

    // Check for 200 Connection Established
    if !response_line.contains("200") {
        return Err(PloyError::Internal(format!(
            "Proxy CONNECT failed: {}",
            response_line.trim()
        )));
    }

    // Read remaining headers until empty line
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

    // Reunite the stream
    let reader = buf_reader.into_inner();
    let stream = reader
        .reunite(writer)
        .map_err(|e| PloyError::Internal(format!("Failed to reunite stream: {}", e)))?;

    debug!("Proxy tunnel established to {}:{}", target_host, target_port);
    Ok(stream)
}

/// Connect WebSocket, using proxy if available
async fn connect_websocket_with_proxy(
    url: &Url,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let host = url.host_str().unwrap_or(BINANCE_WS_HOST);
    let port = url.port().unwrap_or(BINANCE_WS_PORT);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!("Using proxy {}:{} for WebSocket connection", proxy_host, proxy_port);

            // Connect through proxy
            let tcp_stream = connect_via_proxy(&proxy_host, proxy_port, host, port).await?;

            // Establish TLS over the tunnel
            let connector = native_tls::TlsConnector::new()
                .map_err(|e| PloyError::Internal(format!("TLS connector error: {}", e)))?;
            let connector = tokio_native_tls::TlsConnector::from(connector);

            let tls_stream = connector
                .connect(host, tcp_stream)
                .await
                .map_err(|e| PloyError::Internal(format!("TLS handshake failed: {}", e)))?;

            // Wrap in MaybeTlsStream for consistent type
            let maybe_tls = MaybeTlsStream::NativeTls(tls_stream);

            // Upgrade to WebSocket using client_async
            let (ws_stream, _response) = tokio_tungstenite::client_async(url.as_str(), maybe_tls)
                .await
                .map_err(|e| PloyError::Internal(format!("WebSocket handshake failed: {}", e)))?;

            return Ok(ws_stream);
        }
    }

    // No proxy or invalid proxy URL - connect directly
    let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(url))
        .await
        .map_err(|_| PloyError::Internal("WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}

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

    /// Calculate weighted momentum across multiple timeframes
    /// Formula: 0.2 * mom_10s + 0.3 * mom_30s + 0.5 * mom_60s
    /// Returns None if insufficient history for all timeframes
    pub fn weighted_momentum(&self) -> Option<Decimal> {
        let mom_10s = self.momentum(10)?;
        let mom_30s = self.momentum(30)?;
        let mom_60s = self.momentum(60)?;

        // Weights: short-term 20%, mid-term 30%, longer-term 50%
        let weighted = mom_10s * Decimal::new(2, 1)  // 0.2
            + mom_30s * Decimal::new(3, 1)           // 0.3
            + mom_60s * Decimal::new(5, 1);          // 0.5

        Some(weighted)
    }

    /// Calculate weighted momentum with custom weights
    /// weights: (w_10s, w_30s, w_60s) should sum to 1.0
    pub fn weighted_momentum_custom(
        &self,
        w_10s: Decimal,
        w_30s: Decimal,
        w_60s: Decimal,
    ) -> Option<Decimal> {
        let mom_10s = self.momentum(10)?;
        let mom_30s = self.momentum(30)?;
        let mom_60s = self.momentum(60)?;

        Some(mom_10s * w_10s + mom_30s * w_30s + mom_60s * w_60s)
    }

    /// Calculate rolling volatility (standard deviation of returns) over N seconds
    /// Returns the volatility as a percentage (e.g., 0.01 = 1%)
    pub fn volatility(&self, lookback_secs: u64) -> Option<Decimal> {
        if self.history.len() < 10 {
            return None; // Need sufficient data
        }

        let cutoff_time = self.timestamp - chrono::Duration::seconds(lookback_secs as i64);

        // Collect prices within the lookback window
        let prices: Vec<Decimal> = self
            .history
            .iter()
            .filter(|(_, ts)| *ts >= cutoff_time)
            .map(|(p, _)| *p)
            .collect();

        if prices.len() < 5 {
            return None; // Need at least 5 data points
        }

        // Calculate returns (percentage changes between consecutive prices)
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

        // Calculate mean return
        let sum: Decimal = returns.iter().copied().sum();
        let mean = sum / Decimal::from(returns.len());

        // Calculate variance
        let variance_sum: Decimal = returns
            .iter()
            .map(|r| {
                let diff = *r - mean;
                diff * diff
            })
            .sum();
        let variance = variance_sum / Decimal::from(returns.len());

        // Standard deviation (approximate sqrt using Newton's method)
        let std_dev = decimal_sqrt(variance)?;

        Some(std_dev)
    }

    /// Get the number of price samples in history
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Check if we have enough history for weighted momentum calculation
    pub fn has_sufficient_history(&self) -> bool {
        // Need at least 60 seconds of data
        if self.history.len() < 60 {
            return false;
        }

        // Check if oldest entry is at least 60 seconds old
        if let Some((_, oldest_ts)) = self.history.back() {
            let age = self.timestamp - *oldest_ts;
            return age.num_seconds() >= 60;
        }

        false
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

    // Initial guess
    let mut guess = x / Decimal::TWO;
    let tolerance = Decimal::new(1, 10); // 0.0000000001

    // Newton's method iterations
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

    /// Calculate weighted momentum for a symbol
    pub async fn weighted_momentum(&self, symbol: &str) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.weighted_momentum()
    }

    /// Calculate volatility for a symbol
    pub async fn volatility(&self, symbol: &str, lookback_secs: u64) -> Option<Decimal> {
        let prices = self.prices.read().await;
        prices.get(symbol)?.volatility(lookback_secs)
    }

    /// Check if symbol has sufficient history for weighted momentum
    pub async fn has_sufficient_history(&self, symbol: &str) -> bool {
        let prices = self.prices.read().await;
        prices
            .get(symbol)
            .map(|s| s.has_sufficient_history())
            .unwrap_or(false)
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

        let ws_stream = connect_websocket_with_proxy(&url).await?;

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
