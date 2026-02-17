//! Binance WebSocket adapter for real-time kline (candlestick) data.
//!
//! Primary use-case: receive `kline_5m` / `kline_15m` close events with minimal latency
//! for strategies that operate on candle boundaries.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use url::Url;

use crate::error::{PloyError, Result};

/// Binance host (used for CONNECT + TLS)
const BINANCE_WS_HOST: &str = "stream.binance.com";
const BINANCE_WS_PORT: u16 = 9443;

/// How often to send ping frames
const PING_INTERVAL_SECS: u64 = 30;

/// Maximum reconnection delay
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Update broadcast channel capacity
const CHANNEL_CAPACITY: usize = 1000;

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

    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&proxy_addr))
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

    Ok(stream)
}

/// Connect WebSocket, using proxy if available.
async fn connect_websocket_with_proxy(
    url: &Url,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let host = url.host_str().unwrap_or(BINANCE_WS_HOST);
    let port = url.port().unwrap_or(BINANCE_WS_PORT);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!(
                "Using proxy {}:{} for WebSocket connection",
                proxy_host, proxy_port
            );

            let tcp_stream = connect_via_proxy(&proxy_host, proxy_port, host, port).await?;

            // Establish TLS over the tunnel
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

    // No proxy or invalid proxy URL - connect directly
    let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(url))
        .await
        .map_err(|_| PloyError::Internal("WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}

#[derive(Debug, Clone)]
pub struct BinanceKlineBar {
    pub open_time: DateTime<Utc>,
    pub close_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub is_closed: bool,
}

#[derive(Debug, Clone)]
pub struct KlineUpdate {
    pub symbol: String,
    pub interval: String,
    pub kline: BinanceKlineBar,
    pub event_time: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct CombinedStream<T> {
    stream: String,
    data: T,
}

#[derive(Debug, Deserialize)]
struct BinanceKlineEvent {
    #[serde(rename = "e")]
    _event_type: String,
    #[serde(rename = "E")]
    event_time: u64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "k")]
    kline: BinanceKlineData,
}

#[derive(Debug, Deserialize)]
struct BinanceKlineData {
    #[serde(rename = "t")]
    open_time: u64,
    #[serde(rename = "T")]
    close_time: u64,
    #[serde(rename = "i")]
    interval: String,
    #[serde(rename = "o")]
    open: String,
    #[serde(rename = "c")]
    close: String,
    #[serde(rename = "h")]
    high: String,
    #[serde(rename = "l")]
    low: String,
    #[serde(rename = "v")]
    volume: String,
    #[serde(rename = "x")]
    is_closed: bool,
}

/// Binance WebSocket client for real-time kline data.
pub struct BinanceKlineWebSocket {
    ws_url: String,
    update_tx: broadcast::Sender<KlineUpdate>,
    symbols: Vec<String>,
    intervals: Vec<String>,
    closed_only: bool,
    reconnect_delay: Duration,
}

impl BinanceKlineWebSocket {
    /// Create a new Binance kline WebSocket client.
    ///
    /// # Arguments
    /// * `symbols` - Trading pairs like ["BTCUSDT", "ETHUSDT"]
    /// * `intervals` - Binance intervals like ["5m", "15m"]
    /// * `closed_only` - If true, only emit closed klines (`x == true`)
    pub fn new(symbols: Vec<String>, intervals: Vec<String>, closed_only: bool) -> Self {
        let (update_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            ws_url: format!(
                "wss://{}:{}/stream?streams=",
                BINANCE_WS_HOST, BINANCE_WS_PORT
            ),
            update_tx,
            symbols,
            intervals,
            closed_only,
            reconnect_delay: Duration::from_secs(1),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<KlineUpdate> {
        self.update_tx.subscribe()
    }

    fn build_url(&self) -> String {
        let mut streams: Vec<String> = Vec::new();
        for s in &self.symbols {
            let sym = s.to_lowercase();
            for i in &self.intervals {
                streams.push(format!("{}@kline_{}", sym, i));
            }
        }
        format!("{}{}", self.ws_url, streams.join("/"))
    }

    pub async fn run(&self) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(MAX_RECONNECT_DELAY_SECS);

        info!(
            "Starting Binance kline WS for symbols={:?} intervals={:?} closed_only={}",
            self.symbols, self.intervals, self.closed_only
        );

        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    info!("Binance kline WS connection closed normally");
                    attempt = 0;
                }
                Err(e) => {
                    attempt = attempt.saturating_add(1);
                    error!("Binance kline WS error (attempt {}): {}", attempt, e);
                }
            }

            // Exponential-ish backoff with jitter (similar to BinanceWebSocket).
            let base_delay = self.reconnect_delay * attempt.min(10);
            let delay = base_delay.min(max_delay);

            let jitter_range = delay.as_millis() as u64 / 4;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let jitter = Duration::from_millis(seed % jitter_range.max(1));
            let final_delay = delay + jitter;

            info!(
                "Reconnecting to Binance kline WS in {:?} (attempt {})",
                final_delay,
                attempt + 1
            );
            tokio::time::sleep(final_delay).await;
        }
    }

    async fn connect_and_stream(&self) -> Result<()> {
        let url = self.build_url();
        let url = Url::parse(&url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to Binance kline WS: {}", url);

        let ws_stream = connect_websocket_with_proxy(&url).await?;
        info!("Connected to Binance kline WS");

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
                            info!("Binance kline WS stream ended");
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
                    debug!("Sent ping to Binance (kline WS)");
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&self, text: &str) {
        // Combined-stream wrapper.
        if let Ok(wrapper) = serde_json::from_str::<CombinedStream<BinanceKlineEvent>>(text) {
            self.process_event(wrapper.data).await;
            return;
        }

        // Raw event (some endpoints can deliver without wrapper).
        if let Ok(ev) = serde_json::from_str::<BinanceKlineEvent>(text) {
            self.process_event(ev).await;
            return;
        }

        debug!(
            "Unrecognized Binance kline message: {}",
            &text[..text.len().min(120)]
        );
    }

    async fn process_event(&self, ev: BinanceKlineEvent) {
        if self.closed_only && !ev.kline.is_closed {
            return;
        }

        let open = match ev.kline.open.parse::<Decimal>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse kline open '{}': {}", ev.kline.open, e);
                return;
            }
        };
        let close = match ev.kline.close.parse::<Decimal>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse kline close '{}': {}", ev.kline.close, e);
                return;
            }
        };
        let high = match ev.kline.high.parse::<Decimal>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse kline high '{}': {}", ev.kline.high, e);
                return;
            }
        };
        let low = match ev.kline.low.parse::<Decimal>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse kline low '{}': {}", ev.kline.low, e);
                return;
            }
        };
        let volume = match ev.kline.volume.parse::<Decimal>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse kline volume '{}': {}", ev.kline.volume, e);
                return;
            }
        };

        let open_time =
            DateTime::from_timestamp_millis(ev.kline.open_time as i64).unwrap_or_else(Utc::now);
        let close_time =
            DateTime::from_timestamp_millis(ev.kline.close_time as i64).unwrap_or_else(Utc::now);
        let event_time =
            DateTime::from_timestamp_millis(ev.event_time as i64).unwrap_or_else(Utc::now);

        let update = KlineUpdate {
            symbol: ev.symbol,
            interval: ev.kline.interval,
            kline: BinanceKlineBar {
                open_time,
                close_time,
                open,
                high,
                low,
                close,
                volume,
                is_closed: ev.kline.is_closed,
            },
            event_time,
        };

        let _ = self.update_tx.send(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_combined_stream_closed_kline() {
        let msg = r#"{
            "stream":"btcusdt@kline_5m",
            "data":{
                "e":"kline",
                "E":1700000000000,
                "s":"BTCUSDT",
                "k":{
                    "t":1700000000000,
                    "T":1700000299999,
                    "s":"BTCUSDT",
                    "i":"5m",
                    "f":0,
                    "L":0,
                    "o":"100.0",
                    "c":"101.0",
                    "h":"102.0",
                    "l":"99.0",
                    "v":"123.4",
                    "n":0,
                    "x":true,
                    "q":"0",
                    "V":"0",
                    "Q":"0",
                    "B":"0"
                }
            }
        }"#;

        let wrapper: CombinedStream<BinanceKlineEvent> = serde_json::from_str(msg).unwrap();
        assert_eq!(wrapper.data.symbol, "BTCUSDT");
        assert_eq!(wrapper.data.kline.interval, "5m");
        assert!(wrapper.data.kline.is_closed);
    }
}
