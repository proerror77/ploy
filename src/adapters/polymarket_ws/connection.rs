use super::PolymarketWebSocket;
use crate::error::{PloyError, Result};
use crate::services::HealthState;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{interval, timeout, Instant};
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async, tungstenite::Message, MaybeTlsStream,
    WebSocketStream,
};
use tracing::{debug, error, info, warn};
use url::Url;

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
    let host = url
        .host_str()
        .ok_or_else(|| PloyError::Internal("No host in URL".to_string()))?;
    let port = url.port().unwrap_or(443);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!(
                "Using proxy {}:{} for Polymarket WebSocket",
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

    let (ws_stream, _) = timeout(Duration::from_secs(10), connect_async(url.as_str()))
        .await
        .map_err(|_| PloyError::Internal("WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}

/// Initial subscription request
#[derive(Debug, Clone, Serialize)]
struct SubscribeRequest {
    #[serde(rename = "type")]
    msg_type: String,
    assets_ids: Vec<String>,
}

/// Dynamic subscription/unsubscription request
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
struct DynamicSubscribeRequest {
    assets_ids: Vec<String>,
    operation: String,
}

impl PolymarketWebSocket {
    /// Connect and run the WebSocket client with circuit breaker and infinite reconnection
    pub async fn run(&self, token_ids: Vec<String>) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(60); // Cap at 60 seconds
        let circuit_open_delay = Duration::from_secs(5); // Check circuit breaker every 5s when open

        loop {
            let subscription_ids = self.build_subscription_list(&token_ids).await;
            if subscription_ids.is_empty() {
                warn!("No token subscriptions registered yet; waiting before reconnect attempt");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }

            // Check circuit breaker before attempting connection
            if !self.circuit_breaker.should_allow().await {
                let cb_state = self.circuit_breaker.get_state().await;
                warn!(
                    "Circuit breaker is {:?}, waiting {:?} before retry check",
                    cb_state, circuit_open_delay
                );
                tokio::time::sleep(circuit_open_delay).await;
                continue;
            }

            match self.connect_and_subscribe(&subscription_ids).await {
                Ok(()) => {
                    // Connection closed normally - still counts as success for circuit breaker
                    self.circuit_breaker.record_success().await;
                    info!("WebSocket connection closed, reconnecting...");
                    attempt = 0;
                }
                Err(e) => {
                    self.circuit_breaker.record_failure().await;
                    attempt = attempt.saturating_add(1);
                    error!(
                        "WebSocket error (attempt {}, circuit failures {}): {}",
                        attempt,
                        self.circuit_breaker.consecutive_failures(),
                        e
                    );

                    // Exponential backoff with jitter, capped at max_delay
                    let capped_attempt = attempt.min(self.max_reconnect_attempts.max(1));
                    let base_delay = self.reconnect_delay * capped_attempt;
                    let delay = base_delay.min(max_delay);

                    // Add jitter: ±25% randomization
                    let jitter_range = delay.as_millis() as u64 / 4;
                    let jitter = if jitter_range > 0 {
                        use std::time::{SystemTime, UNIX_EPOCH};
                        let seed = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64;
                        Duration::from_millis(seed % jitter_range)
                    } else {
                        Duration::ZERO
                    };

                    let final_delay = delay + jitter;
                    let cb_state = self.circuit_breaker.get_state().await;
                    warn!(
                        "Reconnecting in {:?} (attempt {}, circuit: {:?})",
                        final_delay, attempt, cb_state
                    );
                    tokio::time::sleep(final_delay).await;
                }
            }
        }
    }

    /// Connect and subscribe to token updates
    async fn connect_and_subscribe(&self, token_ids: &[String]) -> Result<()> {
        let health = self.health_state.get().cloned();
        let freshness = self.freshness.get().cloned();

        struct WsHealthGuard(Option<Arc<HealthState>>);
        impl Drop for WsHealthGuard {
            fn drop(&mut self) {
                if let Some(ref h) = self.0 {
                    h.set_ws_connected(false);
                }
            }
        }

        struct WsFreshnessGuard(Option<Arc<crate::platform::DataPlaneFreshness>>);
        impl Drop for WsFreshnessGuard {
            fn drop(&mut self) {
                if let Some(ref f) = self.0 {
                    f.set_source_connected(crate::platform::DataSource::PolymarketWs, false);
                }
            }
        }

        let _guard = WsHealthGuard(health.clone());
        let _fresh_guard = WsFreshnessGuard(freshness.clone());

        let url = Url::parse(&self.ws_url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to WebSocket: {}", url);

        let ws_stream = connect_websocket_with_proxy(&url).await?;

        info!("WebSocket connected");
        if let Some(ref h) = health {
            h.set_ws_connected(true);
        }
        if let Some(ref f) = freshness {
            f.set_source_connected(crate::platform::DataSource::PolymarketWs, true);
        }

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to MARKET channel for order book updates
        // Polymarket WebSocket expects "type": "MARKET" not "subscribe"
        let subscribe_msg = SubscribeRequest {
            msg_type: "MARKET".to_string(),
            assets_ids: token_ids.to_vec(),
        };

        let msg_json = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(msg_json.into())).await?;
        info!("Subscribed to {} tokens", token_ids.len());

        // Set up ping interval
        let mut ping_interval = interval(Duration::from_secs(30));
        let mut health_interval = interval(Duration::from_secs(15));
        let mut last_market_data = Instant::now();
        let stale_timeout = Duration::from_secs(90);

        loop {
            tokio::select! {
                // Handle incoming messages
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if self.handle_message(&text).await {
                                last_market_data = Instant::now();
                                if let Some(ref h) = health {
                                    h.record_ws_message().await;
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            write.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("Received close frame");
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(e.into());
                        }
                        None => {
                            break;
                        }
                        _ => {}
                    }
                }
                // Send periodic pings
                _ = ping_interval.tick() => {
                    write.send(Message::Ping(vec![].into())).await?;
                    debug!("Sent ping");
                }
                // Connection health / resubscribe checks
                _ = health_interval.tick() => {
                    if self.resubscribe_requested.swap(false, Ordering::SeqCst) {
                        info!("Resubscribe requested; reconnecting WebSocket session");
                        break;
                    }

                    if last_market_data.elapsed() > stale_timeout {
                        return Err(PloyError::Internal(format!(
                            "No market data received for {:?}; forcing reconnect",
                            stale_timeout
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}
