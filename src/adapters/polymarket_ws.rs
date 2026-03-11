use crate::domain::{Quote, Side};
use crate::services::HealthState;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::debug;

mod connection;
mod messages;
mod runtime_support;
mod subscriptions;

#[cfg(test)]
use self::messages::extract_book_top;
pub use self::messages::{
    BookMessage, PriceChangeEntry, PriceChangeItem, PriceChangesMessage, PriceLevel,
};
pub use self::runtime_support::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerState, DisplayQuote, QuoteCache,
};

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
