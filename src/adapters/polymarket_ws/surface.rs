use super::{BookMessage, CircuitBreaker, CircuitBreakerConfig, QuoteCache};
use crate::domain::{Quote, Side};
use crate::services::HealthState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

/// Polymarket WebSocket client with circuit breaker
pub struct PolymarketWebSocket {
    pub(super) ws_url: String,
    pub(super) quote_cache: QuoteCache,
    pub(super) token_to_side: Arc<RwLock<HashMap<String, Side>>>,
    /// Token IDs that should be subscribed for full book snapshots, but do not have an `Up/Down`
    /// `Side` mapping (ex: YES/NO sports markets).
    pub(super) extra_tokens: Arc<RwLock<HashSet<String>>>,
    pub(super) update_tx: broadcast::Sender<QuoteUpdate>,
    pub(super) book_tx: broadcast::Sender<Arc<BookMessage>>,
    pub(super) reconnect_delay: Duration,
    pub(super) max_reconnect_attempts: u32,
    pub(super) circuit_breaker: Arc<CircuitBreaker>,
    pub(super) resubscribe_requested: Arc<std::sync::atomic::AtomicBool>,
    // Optional: wired in at runtime by the binary to report connectivity to /health.
    pub(super) health_state: OnceLock<Arc<HealthState>>,
    // Optional: per-symbol freshness tracking for the data plane.
    pub(super) freshness: OnceLock<Arc<crate::data_plane::DataPlaneFreshness>>,
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
    pub fn set_freshness(&self, freshness: Arc<crate::data_plane::DataPlaneFreshness>) {
        if self.freshness.set(Arc::clone(&freshness)).is_ok() {
            freshness.set_source_connected(crate::data_plane::DataSource::PolymarketWs, false);
            freshness.set_subscription_count(crate::data_plane::DataSource::PolymarketWs, 0);
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
}
