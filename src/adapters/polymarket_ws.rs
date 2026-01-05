use crate::domain::{Quote, Side};
use crate::error::{PloyError, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{interval, timeout, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use url::Url;

/// Circuit breaker state for WebSocket connections
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitBreakerState {
    /// Normal operation - connection attempts allowed
    Closed,
    /// Circuit tripped - blocking connection attempts
    Open,
    /// Testing if connection can be restored
    HalfOpen,
}

/// Circuit breaker configuration
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening circuit
    pub failure_threshold: u32,
    /// Time to wait before trying half-open (seconds)
    pub open_timeout_secs: u64,
    /// Number of successful operations to close circuit
    pub success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_timeout_secs: 60,
            success_threshold: 2,
        }
    }
}

/// Circuit breaker for WebSocket connections
pub struct CircuitBreaker {
    state: RwLock<CircuitBreakerState>,
    consecutive_failures: AtomicU32,
    consecutive_successes: AtomicU32,
    last_failure_time: RwLock<Option<Instant>>,
    config: CircuitBreakerConfig,
    /// Total number of times circuit was opened
    open_count: AtomicU64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: RwLock::new(CircuitBreakerState::Closed),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
            last_failure_time: RwLock::new(None),
            config,
            open_count: AtomicU64::new(0),
        }
    }

    /// Check if operation should be allowed
    pub async fn should_allow(&self) -> bool {
        let state = *self.state.read().await;

        match state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open => {
                // Check if we should transition to half-open
                if let Some(last_failure) = *self.last_failure_time.read().await {
                    if last_failure.elapsed() >= Duration::from_secs(self.config.open_timeout_secs) {
                        // Transition to half-open
                        *self.state.write().await = CircuitBreakerState::HalfOpen;
                        self.consecutive_successes.store(0, Ordering::SeqCst);
                        info!("Circuit breaker transitioning to half-open state");
                        return true;
                    }
                }
                false
            }
            CircuitBreakerState::HalfOpen => true,
        }
    }

    /// Get current state
    pub async fn get_state(&self) -> CircuitBreakerState {
        *self.state.read().await
    }

    /// Record a successful operation
    pub async fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;

        let current_state = *self.state.read().await;

        if current_state == CircuitBreakerState::HalfOpen
            && successes >= self.config.success_threshold
        {
            *self.state.write().await = CircuitBreakerState::Closed;
            info!("Circuit breaker closed after {} successful operations", successes);
        }
    }

    /// Record a failed operation
    pub async fn record_failure(&self) {
        self.consecutive_successes.store(0, Ordering::SeqCst);
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_failure_time.write().await = Some(Instant::now());

        let current_state = *self.state.read().await;

        // In half-open, any failure trips back to open
        if current_state == CircuitBreakerState::HalfOpen {
            *self.state.write().await = CircuitBreakerState::Open;
            self.open_count.fetch_add(1, Ordering::SeqCst);
            warn!("Circuit breaker re-opened from half-open state");
            return;
        }

        // In closed, check threshold
        if current_state == CircuitBreakerState::Closed
            && failures >= self.config.failure_threshold
        {
            *self.state.write().await = CircuitBreakerState::Open;
            self.open_count.fetch_add(1, Ordering::SeqCst);
            warn!(
                "Circuit breaker opened after {} consecutive failures",
                failures
            );
        }
    }

    /// Get the number of times circuit was opened
    pub fn open_count(&self) -> u64 {
        self.open_count.load(Ordering::Relaxed)
    }

    /// Get consecutive failures
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Force reset the circuit breaker
    pub async fn reset(&self) {
        *self.state.write().await = CircuitBreakerState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
        *self.last_failure_time.write().await = None;
        info!("Circuit breaker manually reset");
    }
}

/// Order book message from WebSocket
#[derive(Debug, Clone, Deserialize)]
pub struct BookMessage {
    pub asset_id: String,
    pub market: String,
    #[serde(default)]
    pub bids: Vec<PriceLevel>,
    #[serde(default)]
    pub asks: Vec<PriceLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

/// Price change message from WebSocket
#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangesMessage {
    pub market: String,
    pub price_changes: Vec<PriceChangeItem>,
}

/// Individual price change item
#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangeItem {
    pub asset_id: String,
    pub price: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangeEntry {
    pub interval: String,
    pub change: String,
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
struct DynamicSubscribeRequest {
    assets_ids: Vec<String>,
    operation: String,
}

/// Simplified quote for display
#[derive(Debug, Clone)]
pub struct DisplayQuote {
    pub best_bid: Decimal,
    pub best_ask: Decimal,
    pub bid_size: Decimal,
    pub ask_size: Decimal,
    pub timestamp: chrono::DateTime<Utc>,
}

/// Quote TTL in seconds (30 seconds)
const QUOTE_TTL_SECS: i64 = 30;

/// Market quote cache (thread-safe) with TTL support
#[derive(Debug, Clone, Default)]
pub struct QuoteCache {
    quotes: Arc<RwLock<HashMap<String, Quote>>>,
}

impl QuoteCache {
    pub fn new() -> Self {
        Self {
            quotes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a quote is stale (older than TTL)
    fn is_stale(quote: &Quote) -> bool {
        let age = Utc::now() - quote.timestamp;
        age.num_seconds() > QUOTE_TTL_SECS
    }

    /// Update quote for a token
    pub async fn update(&self, token_id: &str, side: Side, bid: Option<Decimal>, ask: Option<Decimal>, bid_size: Option<Decimal>, ask_size: Option<Decimal>) {
        let mut quotes = self.quotes.write().await;
        let quote = quotes.entry(token_id.to_string()).or_insert_with(|| Quote {
            side,
            best_bid: None,
            best_ask: None,
            bid_size: None,
            ask_size: None,
            timestamp: Utc::now(),
        });

        if bid.is_some() {
            quote.best_bid = bid;
            quote.bid_size = bid_size;
        }
        if ask.is_some() {
            quote.best_ask = ask;
            quote.ask_size = ask_size;
        }
        quote.timestamp = Utc::now();
    }

    /// Get quote for a token (returns None if stale)
    pub async fn get(&self, token_id: &str) -> Option<Quote> {
        let quotes = self.quotes.read().await;
        quotes.get(token_id).filter(|q| !Self::is_stale(q)).cloned()
    }

    /// Get all non-stale quotes
    pub async fn get_all(&self) -> HashMap<String, Quote> {
        let quotes = self.quotes.read().await;
        quotes
            .iter()
            .filter(|(_, q)| !Self::is_stale(q))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Clean up stale quotes (call periodically)
    pub async fn cleanup_stale(&self) -> usize {
        let mut quotes = self.quotes.write().await;
        let before = quotes.len();
        quotes.retain(|_, q| !Self::is_stale(q));
        before - quotes.len()
    }

    /// Get current cache size
    pub async fn len(&self) -> usize {
        self.quotes.read().await.len()
    }

    /// Clear all quotes
    pub async fn clear(&self) {
        self.quotes.write().await.clear();
    }

    /// Get UP and DOWN quotes
    pub async fn get_quotes(&self) -> (Option<DisplayQuote>, Option<DisplayQuote>) {
        let quotes = self.quotes.read().await;
        let mut up_quote = None;
        let mut down_quote = None;

        for quote in quotes.values() {
            let display = DisplayQuote {
                best_bid: quote.best_bid.unwrap_or_default(),
                best_ask: quote.best_ask.unwrap_or_default(),
                bid_size: quote.bid_size.unwrap_or_default(),
                ask_size: quote.ask_size.unwrap_or_default(),
                timestamp: quote.timestamp,
            };

            match quote.side {
                Side::Up => up_quote = Some(display),
                Side::Down => down_quote = Some(display),
            }
        }

        (up_quote, down_quote)
    }
}

/// Polymarket WebSocket client with circuit breaker
pub struct PolymarketWebSocket {
    ws_url: String,
    quote_cache: QuoteCache,
    token_to_side: Arc<RwLock<HashMap<String, Side>>>,
    update_tx: broadcast::Sender<QuoteUpdate>,
    reconnect_delay: Duration,
    max_reconnect_attempts: u32,
    circuit_breaker: Arc<CircuitBreaker>,
}

/// Quote update notification
#[derive(Debug, Clone)]
pub struct QuoteUpdate {
    pub token_id: String,
    pub side: Side,
    pub quote: Quote,
}

impl PolymarketWebSocket {
    /// Create a new WebSocket client
    pub fn new(ws_url: &str) -> Self {
        Self::with_circuit_breaker(ws_url, CircuitBreakerConfig::default())
    }

    /// Create a new WebSocket client with custom circuit breaker config
    pub fn with_circuit_breaker(ws_url: &str, cb_config: CircuitBreakerConfig) -> Self {
        let (update_tx, _) = broadcast::channel(1000);

        Self {
            ws_url: ws_url.to_string(),
            quote_cache: QuoteCache::new(),
            token_to_side: Arc::new(RwLock::new(HashMap::new())),
            update_tx,
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: 10,
            circuit_breaker: Arc::new(CircuitBreaker::new(cb_config)),
        }
    }

    /// Get the circuit breaker (for external monitoring)
    pub fn circuit_breaker(&self) -> Arc<CircuitBreaker> {
        Arc::clone(&self.circuit_breaker)
    }

    /// Get a receiver for quote updates
    pub fn subscribe_updates(&self) -> broadcast::Receiver<QuoteUpdate> {
        self.update_tx.subscribe()
    }

    /// Get the quote cache
    pub fn quote_cache(&self) -> &QuoteCache {
        &self.quote_cache
    }

    /// Register token ID to side mapping
    pub async fn register_tokens(&self, up_token_id: &str, down_token_id: &str) {
        let mut mapping = self.token_to_side.write().await;
        mapping.insert(up_token_id.to_string(), Side::Up);
        mapping.insert(down_token_id.to_string(), Side::Down);
        info!("Registered tokens: UP={}, DOWN={}", up_token_id, down_token_id);
    }

    /// Register a single token with its side
    pub async fn register_token(&self, token_id: &str, side: Side) {
        let mut mapping = self.token_to_side.write().await;
        mapping.insert(token_id.to_string(), side);
        debug!("Registered token: {} as {:?}", token_id, side);
    }

    /// Get side for a token ID
    async fn get_side(&self, token_id: &str) -> Option<Side> {
        let mapping = self.token_to_side.read().await;
        mapping.get(token_id).copied()
    }

    /// Connect and run the WebSocket client with circuit breaker and infinite reconnection
    pub async fn run(&self, token_ids: Vec<String>) -> Result<()> {
        let mut attempt: u32 = 0;
        let max_delay = Duration::from_secs(60); // Cap at 60 seconds
        let circuit_open_delay = Duration::from_secs(5); // Check circuit breaker every 5s when open

        loop {
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

            match self.connect_and_subscribe(&token_ids).await {
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
                    let base_delay = self.reconnect_delay * attempt.min(10);
                    let delay = base_delay.min(max_delay);

                    // Add jitter: ±25% randomization
                    let jitter_range = delay.as_millis() as u64 / 4;
                    let jitter = if jitter_range > 0 {
                        use std::time::{SystemTime, UNIX_EPOCH};
                        let seed = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64;
                        Duration::from_millis((seed % jitter_range) as u64)
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
        let url = Url::parse(&self.ws_url)
            .map_err(|e| PloyError::Internal(format!("Invalid WebSocket URL: {}", e)))?;

        info!("Connecting to WebSocket: {}", url);

        let (ws_stream, _) = timeout(Duration::from_secs(10), connect_async(url.as_str()))
            .await
            .map_err(|_| PloyError::WebSocket(tokio_tungstenite::tungstenite::Error::ConnectionClosed))??;

        info!("WebSocket connected");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to MARKET channel for order book updates
        // Polymarket WebSocket expects "type": "MARKET" not "subscribe"
        let subscribe_msg = SubscribeRequest {
            msg_type: "MARKET".to_string(),
            assets_ids: token_ids.to_vec(),
        };

        let msg_json = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(msg_json)).await?;
        info!("Subscribed to {} tokens", token_ids.len());

        // Set up ping interval
        let mut ping_interval = interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                // Handle incoming messages
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_message(&text).await;
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
                    write.send(Message::Ping(vec![])).await?;
                    debug!("Sent ping");
                }
            }
        }

        Ok(())
    }

    /// Handle an incoming WebSocket message
    async fn handle_message(&self, text: &str) {
        // Log first few chars for debugging
        let preview = &text[..text.len().min(200)];
        debug!("WS message received: {}", preview);

        // Try to parse as array of book messages (order book snapshots)
        if let Ok(books) = serde_json::from_str::<Vec<BookMessage>>(text) {
            info!("Received {} book updates", books.len());
            for book in books {
                self.process_book_message(book).await;
            }
            return;
        }

        // Try to parse as price changes message
        if let Ok(price_msg) = serde_json::from_str::<PriceChangesMessage>(text) {
            debug!("Received price changes for market: {}", price_msg.market);
            self.process_price_changes(price_msg).await;
            return;
        }

        // Try to parse as single book message
        if let Ok(book) = serde_json::from_str::<BookMessage>(text) {
            debug!("Received single book update for: {}", book.asset_id);
            self.process_book_message(book).await;
            return;
        }

        // Unknown format - log for debugging (include more of message)
        warn!("Unknown WS message format: {}", preview);
    }

    /// Process an order book message
    async fn process_book_message(&self, book: BookMessage) {
        let asset_id = &book.asset_id;

        // Polymarket order books are sorted:
        // - Bids: ascending (lowest to highest), so best bid = last element
        // - Asks: descending (highest to lowest), so best ask = last element
        let best_bid = book.bids
            .last()
            .and_then(|p| p.price.parse::<Decimal>().ok());
        let best_ask = book.asks
            .last()
            .and_then(|p| p.price.parse::<Decimal>().ok());
        let bid_size = book.bids
            .last()
            .and_then(|p| p.size.parse::<Decimal>().ok());
        let ask_size = book.asks
            .last()
            .and_then(|p| p.size.parse::<Decimal>().ok());

        if let Some(side) = self.get_side(asset_id).await {
            self.quote_cache
                .update(asset_id, side, best_bid, best_ask, bid_size, ask_size)
                .await;

            // Notify subscribers
            if let Some(quote) = self.quote_cache.get(asset_id).await {
                let update = QuoteUpdate {
                    token_id: asset_id.clone(),
                    side,
                    quote,
                };
                let _ = self.update_tx.send(update);
            }

            debug!(
                "Book update {}: bid={:?} ask={:?}",
                side, best_bid, best_ask
            );
        }
    }

    /// Process price changes message
    async fn process_price_changes(&self, msg: PriceChangesMessage) {
        for change in msg.price_changes {
            if let (Some(side), Ok(price)) = (
                self.get_side(&change.asset_id).await,
                change.price.parse::<Decimal>(),
            ) {
                debug!("Price change {}: {}", side, price);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_quote_cache() {
        let cache = QuoteCache::new();

        cache
            .update(
                "token1",
                Side::Up,
                Some(Decimal::from(45) / Decimal::from(100)),
                Some(Decimal::from(46) / Decimal::from(100)),
                Some(Decimal::from(100)),
                Some(Decimal::from(50)),
            )
            .await;

        let quote = cache.get("token1").await.unwrap();
        assert_eq!(quote.side, Side::Up);
        assert!(quote.best_bid.is_some());
        assert!(quote.best_ask.is_some());
    }

    #[tokio::test]
    async fn test_circuit_breaker_initial_state() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.get_state().await, CircuitBreakerState::Closed);
        assert!(cb.should_allow().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            open_timeout_secs: 60,
            success_threshold: 2,
        };
        let cb = CircuitBreaker::new(config);

        // Record failures up to threshold
        cb.record_failure().await;
        assert_eq!(cb.get_state().await, CircuitBreakerState::Closed);

        cb.record_failure().await;
        assert_eq!(cb.get_state().await, CircuitBreakerState::Closed);

        cb.record_failure().await;
        assert_eq!(cb.get_state().await, CircuitBreakerState::Open);
        assert!(!cb.should_allow().await);
        assert_eq!(cb.open_count(), 1);
    }

    #[tokio::test]
    async fn test_circuit_breaker_success_resets_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            open_timeout_secs: 60,
            success_threshold: 2,
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure().await;
        cb.record_failure().await;
        assert_eq!(cb.consecutive_failures(), 2);

        // Success should reset failures
        cb.record_success().await;
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.get_state().await, CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_manual_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            open_timeout_secs: 60,
            success_threshold: 1,
        };
        let cb = CircuitBreaker::new(config);

        // Trip the circuit
        cb.record_failure().await;
        cb.record_failure().await;
        assert_eq!(cb.get_state().await, CircuitBreakerState::Open);

        // Manual reset
        cb.reset().await;
        assert_eq!(cb.get_state().await, CircuitBreakerState::Closed);
        assert!(cb.should_allow().await);
    }
}
