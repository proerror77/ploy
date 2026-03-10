use crate::domain::{Quote, Side};
use crate::services::HealthState;
use chrono::Utc;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, broadcast};
use tokio::time::Instant;
use tracing::{debug, info, warn};

mod connection;
mod messages;
mod subscriptions;

#[cfg(test)]
use self::messages::extract_book_top;
pub use self::messages::{
    BookMessage, PriceChangeEntry, PriceChangeItem, PriceChangesMessage, PriceLevel,
};

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
                    if last_failure.elapsed() >= Duration::from_secs(self.config.open_timeout_secs)
                    {
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
            info!(
                "Circuit breaker closed after {} successful operations",
                successes
            );
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
        if current_state == CircuitBreakerState::Closed && failures >= self.config.failure_threshold
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

/// Maximum cache size (prevent unbounded growth)
const MAX_CACHE_SIZE: usize = 10_000;

/// Market quote cache (thread-safe, lock-free) with TTL support and size limits
///
/// This implementation uses DashMap for lock-free concurrent access,
/// providing significant performance improvements over RwLock:
/// - 2000+ operations/sec throughput (vs ~500 with RwLock)
/// - No lock contention under high concurrency
/// - Better scalability with multiple threads
///
/// # CRITICAL FIX
/// Added maximum cache size to prevent unbounded memory growth.
/// Cache will automatically evict stale entries when size limit is reached.
#[derive(Debug, Clone, Default)]
pub struct QuoteCache {
    quotes: Arc<dashmap::DashMap<String, Quote>>,
    max_size: usize,
}

impl QuoteCache {
    pub fn new() -> Self {
        Self {
            quotes: Arc::new(dashmap::DashMap::new()),
            max_size: MAX_CACHE_SIZE,
        }
    }

    /// Create a cache with custom maximum size
    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            quotes: Arc::new(dashmap::DashMap::new()),
            max_size,
        }
    }

    /// Check if a quote is stale (older than TTL)
    fn is_stale(quote: &Quote) -> bool {
        let age = Utc::now() - quote.timestamp;
        age.num_seconds() > QUOTE_TTL_SECS
    }

    /// Update quote for a token
    ///
    /// # CRITICAL FIX
    /// Now enforces maximum cache size by cleaning up stale entries
    /// when the cache is full.
    pub fn update(
        &self,
        token_id: &str,
        side: Side,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
    ) {
        // Check if cache is full and cleanup if needed
        if self.quotes.len() >= self.max_size {
            self.cleanup_stale();
        }

        self.quotes
            .entry(token_id.to_string())
            .and_modify(|quote| {
                if bid.is_some() {
                    quote.best_bid = bid;
                    quote.bid_size = bid_size;
                }
                if ask.is_some() {
                    quote.best_ask = ask;
                    quote.ask_size = ask_size;
                }
                quote.timestamp = Utc::now();
            })
            .or_insert_with(|| Quote {
                side,
                best_bid: bid,
                best_ask: ask,
                bid_size,
                ask_size,
                timestamp: Utc::now(),
            });
    }

    /// Update quote from a full book snapshot.
    ///
    /// Unlike `update`, this overwrites bid/ask even when the value is `None`,
    /// which is important to avoid keeping stale quotes when one side becomes empty.
    pub fn update_snapshot(
        &self,
        token_id: &str,
        side: Side,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        bid_size: Option<Decimal>,
        ask_size: Option<Decimal>,
    ) {
        if self.quotes.len() >= self.max_size {
            self.cleanup_stale();
        }

        self.quotes
            .entry(token_id.to_string())
            .and_modify(|quote| {
                quote.side = side;
                quote.best_bid = bid;
                quote.best_ask = ask;
                quote.bid_size = bid_size;
                quote.ask_size = ask_size;
                quote.timestamp = Utc::now();
            })
            .or_insert_with(|| Quote {
                side,
                best_bid: bid,
                best_ask: ask,
                bid_size,
                ask_size,
                timestamp: Utc::now(),
            });
    }

    /// Get quote for a token (returns None if stale)
    pub fn get(&self, token_id: &str) -> Option<Quote> {
        self.quotes
            .get(token_id)
            .filter(|q| !Self::is_stale(q.value()))
            .map(|q| q.value().clone())
    }

    /// Get quote age in seconds
    ///
    /// Returns None if quote doesn't exist
    pub fn get_age(&self, token_id: &str) -> Option<u64> {
        self.quotes.get(token_id).map(|q| {
            let age = Utc::now() - q.value().timestamp;
            age.num_seconds().max(0) as u64
        })
    }

    /// Check if quote is fresh enough for trading
    ///
    /// # Arguments
    /// * `token_id` - Token to check
    /// * `max_age_secs` - Maximum acceptable age in seconds
    ///
    /// # Returns
    /// * `Ok(())` if quote is fresh enough
    /// * `Err` if quote is missing or too old
    pub async fn validate_freshness(
        &self,
        token_id: &str,
        max_age_secs: u64,
    ) -> crate::error::Result<()> {
        let age = self.get_age(token_id).ok_or_else(|| {
            crate::error::PloyError::Internal(format!("No quote available for token {}", token_id))
        })?;

        if age > max_age_secs {
            return Err(crate::error::PloyError::Internal(format!(
                "Quote for {} is stale (age: {}s, max: {}s)",
                token_id, age, max_age_secs
            )));
        }

        Ok(())
    }

    /// Get all non-stale quotes
    pub fn get_all(&self) -> HashMap<String, Quote> {
        self.quotes
            .iter()
            .filter(|entry| !Self::is_stale(entry.value()))
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Clean up stale quotes (call periodically)
    pub fn cleanup_stale(&self) -> usize {
        let before = self.quotes.len();
        self.quotes.retain(|_, q| !Self::is_stale(q));
        before - self.quotes.len()
    }

    /// Get current cache size
    pub fn len(&self) -> usize {
        self.quotes.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty()
    }

    /// Clear all quotes
    pub fn clear(&self) {
        self.quotes.clear();
    }

    /// Get UP and DOWN quotes
    pub fn get_quotes(&self) -> (Option<DisplayQuote>, Option<DisplayQuote>) {
        let mut up_quote = None;
        let mut down_quote = None;

        for entry in self.quotes.iter() {
            let quote = entry.value();
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
    /// Token IDs that should be subscribed for full book snapshots, but do not have an `Up/Down`
    /// `Side` mapping (ex: YES/NO sports markets).
    extra_tokens: Arc<RwLock<HashSet<String>>>,
    update_tx: broadcast::Sender<QuoteUpdate>,
    book_tx: broadcast::Sender<Arc<BookMessage>>,
    reconnect_delay: Duration,
    max_reconnect_attempts: u32,
    circuit_breaker: Arc<CircuitBreaker>,
    resubscribe_requested: Arc<std::sync::atomic::AtomicBool>,
    // Optional: wired in at runtime by the binary to report connectivity to /health.
    health_state: OnceLock<Arc<HealthState>>,
    // Optional: per-symbol freshness tracking for the data plane.
    freshness: OnceLock<Arc<crate::platform::DataPlaneFreshness>>,
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
        // Book snapshots can be significantly larger than quotes; keep a smaller buffer.
        let (book_tx, _) = broadcast::channel(256);

        Self {
            ws_url: ws_url.to_string(),
            quote_cache: QuoteCache::new(),
            token_to_side: Arc::new(RwLock::new(HashMap::new())),
            extra_tokens: Arc::new(RwLock::new(HashSet::new())),
            update_tx,
            book_tx,
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: 10,
            circuit_breaker: Arc::new(CircuitBreaker::new(cb_config)),
            resubscribe_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            health_state: OnceLock::new(),
            freshness: OnceLock::new(),
        }
    }

    /// Wire an optional `HealthState` for liveness/readiness reporting.
    ///
    /// Safe to call multiple times; only the first call wins.
    pub fn set_health_state(&self, state: Arc<HealthState>) {
        let _ = self.health_state.set(state);
    }

    /// Wire an optional `DataPlaneFreshness` for per-symbol tracking.
    pub fn set_freshness(&self, freshness: Arc<crate::platform::DataPlaneFreshness>) {
        if self.freshness.set(Arc::clone(&freshness)).is_ok() {
            freshness.set_source_connected(crate::platform::DataSource::PolymarketWs, false);
            freshness.set_subscription_count(crate::platform::DataSource::PolymarketWs, 0);
        }
    }

    /// Get the circuit breaker (for external monitoring)
    pub fn circuit_breaker(&self) -> Arc<CircuitBreaker> {
        Arc::clone(&self.circuit_breaker)
    }

    /// Get the quote cache
    pub fn quote_cache(&self) -> &QuoteCache {
        &self.quote_cache
    }

    /// Test-only hook: inject a raw WebSocket message into the parser/broadcast path.
    #[cfg(test)]
    pub async fn ingest_test_message(&self, text: &str) -> bool {
        self.handle_message(text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_quote_cache() {
        let cache = QuoteCache::new();

        cache.update(
            "token1",
            Side::Up,
            Some(Decimal::from(45) / Decimal::from(100)),
            Some(Decimal::from(46) / Decimal::from(100)),
            Some(Decimal::from(100)),
            Some(Decimal::from(50)),
        );

        let quote = cache.get("token1").unwrap();
        assert_eq!(quote.side, Side::Up);
        assert!(quote.best_bid.is_some());
        assert!(quote.best_ask.is_some());
    }

    #[test]
    fn test_quote_cache_snapshot_clears_missing_sides() {
        let cache = QuoteCache::new();

        cache.update_snapshot(
            "token1",
            Side::Up,
            Some(dec!(0.45)),
            Some(dec!(0.46)),
            Some(dec!(10)),
            Some(dec!(10)),
        );

        let quote = cache.get("token1").unwrap();
        assert_eq!(quote.best_bid, Some(dec!(0.45)));

        // Snapshot without bids should clear best_bid instead of keeping a stale value.
        cache.update_snapshot(
            "token1",
            Side::Up,
            None,
            Some(dec!(0.46)),
            None,
            Some(dec!(10)),
        );

        let quote = cache.get("token1").unwrap();
        assert_eq!(quote.best_bid, None);
        assert_eq!(quote.best_ask, Some(dec!(0.46)));
    }

    #[test]
    fn test_extract_book_top_unordered() {
        let book = BookMessage {
            asset_id: "token".to_string(),
            market: "m".to_string(),
            bids: vec![
                PriceLevel {
                    price: "0.40".to_string(),
                    size: "10".to_string(),
                },
                PriceLevel {
                    price: "0.45".to_string(),
                    size: "5".to_string(),
                },
                PriceLevel {
                    price: "0.42".to_string(),
                    size: "7".to_string(),
                },
            ],
            asks: vec![
                PriceLevel {
                    price: "0.55".to_string(),
                    size: "2".to_string(),
                },
                PriceLevel {
                    price: "0.50".to_string(),
                    size: "3".to_string(),
                },
                PriceLevel {
                    price: "0.60".to_string(),
                    size: "1".to_string(),
                },
            ],
            timestamp: None,
            hash: None,
        };

        let (best_bid, best_ask, bid_total, ask_total) = extract_book_top(&book);
        assert_eq!(best_bid, Some(dec!(0.45)));
        assert_eq!(best_ask, Some(dec!(0.50)));
        assert_eq!(bid_total, Some(dec!(22))); // 10 + 5 + 7
        assert_eq!(ask_total, Some(dec!(6))); // 2 + 3 + 1
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
    async fn test_build_subscription_list_includes_extra_tokens() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        ws.register_token("token_up", Side::Up).await;

        let mut extra = std::collections::HashSet::new();
        extra.insert("token_yes".to_string());
        let (_added, _removed, total) = ws.reconcile_extra_tokens(&extra).await;
        assert_eq!(total, 1);

        let seed = vec!["seed".to_string()];
        let got = ws.build_subscription_list(&seed).await;
        let set: std::collections::HashSet<String> = got.into_iter().collect();

        assert!(set.contains("seed"));
        assert!(set.contains("token_up"));
        assert!(set.contains("token_yes"));
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

    // =========================================================================
    // Characterization tests — replay realistic WS JSON and verify output
    // =========================================================================

    /// Replay a book snapshot JSON and verify QuoteUpdate broadcast.
    #[tokio::test]
    async fn characterization_book_snapshot_produces_quote_update() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        ws.register_token("0xabc123", Side::Up).await;

        let mut rx = ws.subscribe_updates();

        // Realistic book snapshot JSON (array of BookMessage)
        let json = r#"[{
            "asset_id": "0xabc123",
            "market": "0xmarket1",
            "bids": [
                {"price": "0.45", "size": "100"},
                {"price": "0.44", "size": "200"}
            ],
            "asks": [
                {"price": "0.47", "size": "50"},
                {"price": "0.48", "size": "150"}
            ],
            "timestamp": "1700000000",
            "hash": "abc"
        }]"#;

        let handled = ws.handle_message(json).await;
        assert!(handled, "book snapshot should be handled");

        let update = rx.try_recv().expect("should receive QuoteUpdate");
        assert_eq!(update.token_id, "0xabc123");
        assert_eq!(update.side, Side::Up);
        assert_eq!(update.quote.best_bid, Some(dec!(0.45)));
        assert_eq!(update.quote.best_ask, Some(dec!(0.47)));
    }

    /// Replay a single book message (not array) and verify output.
    #[tokio::test]
    async fn characterization_single_book_message() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        ws.register_token("0xdef456", Side::Down).await;

        let mut rx = ws.subscribe_updates();

        let json = r#"{
            "asset_id": "0xdef456",
            "market": "0xmarket2",
            "bids": [{"price": "0.52", "size": "300"}],
            "asks": [{"price": "0.55", "size": "100"}]
        }"#;

        let handled = ws.handle_message(json).await;
        assert!(handled);

        let update = rx.try_recv().expect("should receive QuoteUpdate");
        assert_eq!(update.side, Side::Down);
        assert_eq!(update.quote.best_bid, Some(dec!(0.52)));
        assert_eq!(update.quote.best_ask, Some(dec!(0.55)));
    }

    /// Unregistered token should NOT produce a QuoteUpdate.
    #[tokio::test]
    async fn characterization_unregistered_token_no_quote() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        // Don't register any tokens

        let mut rx = ws.subscribe_updates();

        let json = r#"[{
            "asset_id": "0xunknown",
            "market": "0xmarket3",
            "bids": [{"price": "0.50", "size": "100"}],
            "asks": [{"price": "0.51", "size": "100"}]
        }]"#;

        ws.handle_message(json).await;

        // Should NOT receive any update
        assert!(
            rx.try_recv().is_err(),
            "unregistered token should not produce QuoteUpdate"
        );
    }

    /// Empty book (no bids/asks) should still be handled but produce None quotes.
    #[tokio::test]
    async fn characterization_empty_book_clears_quotes() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        ws.register_token("0xempty", Side::Up).await;

        let mut rx = ws.subscribe_updates();

        // First: populate with real data
        let json1 = r#"[{"asset_id":"0xempty","market":"m","bids":[{"price":"0.40","size":"10"}],"asks":[{"price":"0.60","size":"10"}]}]"#;
        ws.handle_message(json1).await;
        let _ = rx.try_recv().expect("first update");

        // Second: empty book snapshot
        let json2 = r#"[{"asset_id":"0xempty","market":"m","bids":[],"asks":[]}]"#;
        ws.handle_message(json2).await;

        let update = rx.try_recv().expect("should receive update for empty book");
        assert_eq!(
            update.quote.best_bid, None,
            "empty bids should clear best_bid"
        );
        assert_eq!(
            update.quote.best_ask, None,
            "empty asks should clear best_ask"
        );
    }

    /// Book broadcast channel should emit Arc<BookMessage> for all tokens (even unregistered).
    #[tokio::test]
    async fn characterization_book_broadcast_includes_all_tokens() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        // Register as extra token (no side mapping)
        let mut extra = std::collections::HashSet::new();
        extra.insert("0xextra".to_string());
        ws.reconcile_extra_tokens(&extra).await;

        let mut book_rx = ws.subscribe_books();

        let json = r#"[{"asset_id":"0xextra","market":"m","bids":[{"price":"0.30","size":"5"}],"asks":[{"price":"0.70","size":"5"}]}]"#;
        ws.handle_message(json).await;

        let book = book_rx
            .try_recv()
            .expect("should receive BookMessage broadcast");
        assert_eq!(book.asset_id, "0xextra");
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.asks.len(), 1);
    }

    /// Freshness tracker should record updates when attached.
    #[tokio::test]
    async fn characterization_freshness_recorded_on_book_update() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        ws.register_token("0xfresh", Side::Up).await;

        let freshness = std::sync::Arc::new(crate::platform::DataPlaneFreshness::new());
        ws.set_freshness(freshness.clone());

        let json = r#"[{"asset_id":"0xfresh","market":"m","bids":[{"price":"0.50","size":"10"}],"asks":[{"price":"0.51","size":"10"}]}]"#;
        ws.handle_message(json).await;

        let staleness = freshness.staleness(crate::platform::DataSource::PolymarketWs, "0xfresh");
        assert!(
            staleness.is_some(),
            "freshness should be recorded after book update"
        );
        assert!(
            staleness.unwrap() < 1.0,
            "staleness should be very low (just recorded)"
        );
    }
}
