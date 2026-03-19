//! Binance WebSocket adapter for real-time kline (candlestick) data.
//!
//! Primary use-case: receive `kline_5m` / `kline_15m` close events with minimal latency
//! for strategies that operate on candle boundaries.

#[path = "binance_kline_ws_connection.rs"]
mod connection;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::error::{PloyError, Result};
use connection::{BINANCE_WS_HOST, BINANCE_WS_PORT, connect_websocket_with_proxy};

/// How often to send ping frames
const PING_INTERVAL_SECS: u64 = 30;

/// Maximum reconnection delay
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Update broadcast channel capacity
const CHANNEL_CAPACITY: usize = 1000;

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
    // Present in Binance combined stream payloads; retained for debugging.
    #[allow(dead_code)]
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
    // Optional: per-symbol freshness tracking for the data plane.
    freshness: OnceLock<Arc<crate::data_plane::DataPlaneFreshness>>,
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
            freshness: OnceLock::new(),
        }
    }

    /// Attach a shared freshness tracker for the data plane.
    pub fn set_freshness(&self, freshness: Arc<crate::data_plane::DataPlaneFreshness>) {
        if self.freshness.set(Arc::clone(&freshness)).is_ok() {
            freshness.set_subscription_count(
                crate::data_plane::DataSource::BinanceKline,
                (self.symbols.len() * self.intervals.len()) as u64,
            );
            freshness.set_source_connected(crate::data_plane::DataSource::BinanceKline, false);
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
        struct ConnectionGuard<'a>(&'a BinanceKlineWebSocket);
        impl Drop for ConnectionGuard<'_> {
            fn drop(&mut self) {
                if let Some(f) = self.0.freshness.get() {
                    f.set_source_connected(crate::data_plane::DataSource::BinanceKline, false);
                }
            }
        }
        let _guard = ConnectionGuard(self);

        let url = self.build_url();
        info!("Connecting to Binance kline WS: {}", url);

        let ws_stream = connect_websocket_with_proxy(&url).await?;
        info!("Connected to Binance kline WS");
        if let Some(f) = self.freshness.get() {
            f.set_source_connected(crate::data_plane::DataSource::BinanceKline, true);
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
                            info!("Binance kline WS stream ended");
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

        // Record per-symbol freshness for the data plane (before ev.symbol is moved).
        if let Some(f) = self.freshness.get() {
            f.record_update(crate::data_plane::DataSource::BinanceKline, &ev.symbol);
        }

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

    #[tokio::test]
    async fn characterization_closed_kline_emits_update() {
        let ws = BinanceKlineWebSocket::new(vec!["BTCUSDT".into()], vec!["5m".into()], true);
        let mut rx = ws.subscribe();

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

        ws.ingest_test_message(msg).await;
        let update = rx.try_recv().expect("closed kline should produce update");
        assert_eq!(update.symbol, "BTCUSDT");
        assert_eq!(update.interval, "5m");
        assert_eq!(update.kline.open, dec!(100.0));
        assert_eq!(update.kline.close, dec!(101.0));
        assert!(update.kline.is_closed);
    }

    #[tokio::test]
    async fn characterization_open_kline_skipped_when_closed_only() {
        let ws = BinanceKlineWebSocket::new(vec!["BTCUSDT".into()], vec!["5m".into()], true);
        let mut rx = ws.subscribe();

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
                    "x":false,
                    "q":"0",
                    "V":"0",
                    "Q":"0",
                    "B":"0"
                }
            }
        }"#;

        ws.ingest_test_message(msg).await;
        assert!(
            rx.try_recv().is_err(),
            "open kline should be filtered when closed_only=true"
        );
    }

    #[tokio::test]
    async fn characterization_open_kline_emitted_when_closed_only_disabled() {
        let ws = BinanceKlineWebSocket::new(vec!["BTCUSDT".into()], vec!["5m".into()], false);
        let mut rx = ws.subscribe();

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
                    "c":"100.5",
                    "h":"101.0",
                    "l":"99.5",
                    "v":"45.6",
                    "n":0,
                    "x":false,
                    "q":"0",
                    "V":"0",
                    "Q":"0",
                    "B":"0"
                }
            }
        }"#;

        ws.ingest_test_message(msg).await;
        let update = rx.try_recv().expect("open kline should be emitted");
        assert!(!update.kline.is_closed);
        assert_eq!(update.kline.close, dec!(100.5));
    }

    #[tokio::test]
    async fn characterization_freshness_recorded_on_kline() {
        let ws = BinanceKlineWebSocket::new(vec!["BTCUSDT".into()], vec!["5m".into()], true);
        let freshness = std::sync::Arc::new(crate::data_plane::DataPlaneFreshness::new());
        ws.set_freshness(freshness.clone());

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

        ws.ingest_test_message(msg).await;
        let staleness = freshness.staleness(crate::data_plane::DataSource::BinanceKline, "BTCUSDT");
        assert!(staleness.is_some(), "freshness should be recorded");
        assert!(staleness.unwrap() < 1.0);
    }
}
