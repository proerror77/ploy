use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info};
use url::Url;

use super::{
    BinanceWebSocket, BINANCE_WS_HOST, BINANCE_WS_PORT, MAX_RECONNECT_DELAY_SECS,
    PING_INTERVAL_SECS,
};
use crate::error::{PloyError, Result};

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
    let host = url.host_str().unwrap_or(BINANCE_WS_HOST);
    let port = url.port().unwrap_or(BINANCE_WS_PORT);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!(
                "Using proxy {}:{} for WebSocket connection",
                proxy_host, proxy_port
            );

            let tcp_stream = connect_via_proxy(&proxy_host, proxy_port, host, port).await?;

            let (ws_stream, _response) =
                client_async_tls_with_config(url.as_str(), tcp_stream, None, None)
                    .await
                    .map_err(|e| {
                        PloyError::Internal(format!("WebSocket handshake failed: {}", e))
                    })?;

            return Ok(ws_stream);
        }
    }

    let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(url.as_str()))
        .await
        .map_err(|_| PloyError::Internal("WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}

impl BinanceWebSocket {
    /// Build the WebSocket URL with stream subscriptions
    fn build_url(&self) -> String {
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
                    error!("Binance WebSocket error (attempt {}): {}", attempt, e);
                }
            }

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
                "Reconnecting to Binance in {:?} (attempt {})",
                final_delay,
                attempt + 1
            );
            tokio::time::sleep(final_delay).await;
        }
    }

    /// Connect and stream price data
    async fn connect_and_stream(&self) -> Result<()> {
        struct ConnectionGuard<'a>(&'a BinanceWebSocket);
        impl Drop for ConnectionGuard<'_> {
            fn drop(&mut self) {
                if let Some(f) = self.0.freshness.get() {
                    f.set_source_connected(crate::platform::DataSource::BinanceSpot, false);
                }
            }
        }
        let _guard = ConnectionGuard(self);

        let url = self.build_url();
        let url = Url::parse(&url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to Binance WebSocket: {}", url);

        let ws_stream = connect_websocket_with_proxy(&url).await?;

        info!("Connected to Binance WebSocket");
        if let Some(f) = self.freshness.get() {
            f.set_source_connected(crate::platform::DataSource::BinanceSpot, true);
        }

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
                    if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                    debug!("Sent ping to Binance");
                }
            }
        }

        Ok(())
    }
}
