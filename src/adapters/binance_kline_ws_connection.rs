use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config, connect_async,
};
use tracing::{debug, info};
use url::Url;

use crate::error::{PloyError, Result};

/// Binance host (used for CONNECT + TLS)
pub(super) const BINANCE_WS_HOST: &str = "stream.binance.com";
pub(super) const BINANCE_WS_PORT: u16 = 9443;

/// Get proxy URL from environment variables.
fn get_proxy_url() -> Option<String> {
    std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .or_else(|_| std::env::var("http_proxy"))
        .or_else(|_| std::env::var("ALL_PROXY"))
        .or_else(|_| std::env::var("all_proxy"))
        .ok()
}

/// Parse proxy URL into host and port.
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

/// Connect to target host through an HTTP CONNECT proxy.
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
    reader
        .reunite(writer)
        .map_err(|e| PloyError::Internal(format!("Failed to reunite stream: {}", e)))
}

/// Connect WebSocket, using proxy if available.
pub(super) async fn connect_websocket_with_proxy(
    url: &str,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let parsed_url = Url::parse(url)
        .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;
    let host = parsed_url.host_str().unwrap_or(BINANCE_WS_HOST);
    let port = parsed_url.port().unwrap_or(BINANCE_WS_PORT);

    if let Some(proxy_url) = get_proxy_url() {
        if let Some((proxy_host, proxy_port)) = parse_proxy_url(&proxy_url) {
            info!(
                "Using proxy {}:{} for WebSocket connection",
                proxy_host, proxy_port
            );

            let tcp_stream = connect_via_proxy(&proxy_host, proxy_port, host, port).await?;
            let (ws_stream, _response) = client_async_tls_with_config(url, tcp_stream, None, None)
                .await
                .map_err(|e| PloyError::Internal(format!("WebSocket handshake failed: {}", e)))?;

            return Ok(ws_stream);
        }
    }

    let (ws_stream, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(url))
        .await
        .map_err(|_| PloyError::Internal("WebSocket connection timeout".to_string()))?
        .map_err(PloyError::WebSocket)?;

    Ok(ws_stream)
}
