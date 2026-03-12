use crate::domain::{Quote, Side};
use crate::error::{PloyError, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

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
            freshness: OnceLock::new(),
        }
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

    /// Request a WebSocket resubscription cycle.
    ///
    /// The current connection loop will reconnect and apply the latest token set.
    pub fn request_resubscribe(&self) {
        self.resubscribe_requested.store(true, Ordering::SeqCst);
    }

    /// Register token ID to side mapping
    pub async fn register_tokens(&self, up_token_id: &str, down_token_id: &str) {
        let mut mapping = self.token_to_side.write().await;
        mapping.insert(up_token_id.to_string(), Side::Up);
        mapping.insert(down_token_id.to_string(), Side::Down);
        drop(mapping);
        self.report_subscription_count().await;
        info!(
            "Registered tokens: UP={}, DOWN={}",
            up_token_id, down_token_id
        );
    }

    /// Register a single token with its side
    pub async fn register_token(&self, token_id: &str, side: Side) {
        let mut mapping = self.token_to_side.write().await;
        mapping.insert(token_id.to_string(), side);
        drop(mapping);
        self.report_subscription_count().await;
        debug!("Registered token: {} as {:?}", token_id, side);
    }

    /// Reconcile the internal token->side mapping to exactly match `desired`.
    ///
    /// This is used by data-collection workloads to keep the WebSocket subscription set bounded,
    /// instead of growing without limit as new markets rotate throughout the day.
    ///
    /// Returns `(added, removed, updated, total)`.
    pub async fn reconcile_token_sides(
        &self,
        desired: &HashMap<String, Side>,
    ) -> (usize, usize, usize, usize) {
        let mut mapping = self.token_to_side.write().await;

        let mut added: usize = 0;
        let mut updated: usize = 0;
        for (token_id, side) in desired {
            match mapping.get(token_id) {
                None => {
                    mapping.insert(token_id.clone(), *side);
                    added = added.saturating_add(1);
                }
                Some(prev) if prev != side => {
                    mapping.insert(token_id.clone(), *side);
                    updated = updated.saturating_add(1);
                }
                _ => {}
            }
        }

        let mut removed: usize = 0;
        mapping.retain(|token_id, _| {
            let keep = desired.contains_key(token_id);
            if !keep {
                removed = removed.saturating_add(1);
            }
            keep
        });

        let total = mapping.len();
        self.report_subscription_count().await;
        (added, removed, updated, total)
    }

    /// Reconcile extra token subscriptions to exactly match `desired`.
    ///
    /// These tokens are included in the WS subscription set, but won't emit `QuoteUpdate`s (no
    /// `Side` mapping exists). They *will* emit full `BookMessage`s via `subscribe_books()`.
    ///
    /// Returns `(added, removed, total)`.
    pub async fn reconcile_extra_tokens(&self, desired: &HashSet<String>) -> (usize, usize, usize) {
        let mut extra = self.extra_tokens.write().await;

        let mut added: usize = 0;
        for token_id in desired {
            if extra.insert(token_id.clone()) {
                added = added.saturating_add(1);
            }
        }

        let mut removed: usize = 0;
        extra.retain(|token_id| {
            let keep = desired.contains(token_id);
            if !keep {
                removed = removed.saturating_add(1);
            }
            keep
        });

        let total = extra.len();
        drop(extra);
        self.report_subscription_count().await;
        (added, removed, total)
    }

    /// Report the current subscription count to the freshness tracker.
    async fn report_subscription_count(&self) {
        if let Some(f) = self.freshness.get() {
            let sides = self.token_to_side.read().await.len();
            let extras = self.extra_tokens.read().await.len();
            f.set_subscription_count(
                crate::platform::DataSource::PolymarketWs,
                (sides + extras) as u64,
            );
        }
    }

    /// Get side for a token ID
    async fn get_side(&self, token_id: &str) -> Option<Side> {
        let mapping = self.token_to_side.read().await;
        mapping.get(token_id).copied()
    }

    /// Build the current token subscription set from startup seed + dynamic registrations.
    async fn build_subscription_list(&self, seed_tokens: &[String]) -> Vec<String> {
        let mut set = HashSet::new();

        for token in seed_tokens {
            if !token.trim().is_empty() {
                set.insert(token.clone());
            }
        }

        {
            let mapping = self.token_to_side.read().await;
            for token in mapping.keys() {
                set.insert(token.clone());
            }
        }

        {
            let extra = self.extra_tokens.read().await;
            for token in extra.iter() {
                set.insert(token.clone());
            }
        }

        set.into_iter().collect()
    }


    /// Handle an incoming WebSocket message
    ///
    /// Returns `true` when the message contained market data updates.
    async fn handle_message(&self, text: &str) -> bool {
        // Log first few chars for debugging
        let preview = &text[..text.len().min(200)];
        debug!("WS message received: {}", preview);

        // Try to parse as array of book messages (order book snapshots)
        if let Ok(books) = serde_json::from_str::<Vec<BookMessage>>(text) {
            if books.is_empty() {
                debug!("Received empty book updates array");
                return false;
            }
            debug!("Received {} book updates", books.len());
            for book in books {
                self.process_book_message(book).await;
            }
            return true;
        }

        // Try to parse as price changes message
        if let Ok(price_msg) = serde_json::from_str::<PriceChangesMessage>(text) {
            debug!("Received price changes for market: {}", price_msg.market);
            let has_data = !price_msg.price_changes.is_empty();
            self.process_price_changes(price_msg).await;
            return has_data;
        }

        // Try to parse as single book message
        if let Ok(book) = serde_json::from_str::<BookMessage>(text) {
            debug!("Received single book update for: {}", book.asset_id);
            self.process_book_message(book).await;
            return true;
        }

        // Unknown format - log for debugging (include more of message)
        warn!("Unknown WS message format: {}", preview);
        false
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
