use super::*;
use tracing::info;

impl PolymarketWebSocket {
    pub async fn register_tokens_for_owner(
        &self,
        owner: &str,
        up_token_id: &str,
        down_token_id: &str,
    ) {
        let mut registry = self.token_registry.write().await;
        registry.register_token(owner, up_token_id, Side::Up);
        registry.register_token(owner, down_token_id, Side::Down);
        drop(registry);
        self.report_subscription_count().await;
        info!(
            owner,
            "Registered tokens: UP={}, DOWN={}", up_token_id, down_token_id
        );
    }

    /// Get a receiver for quote updates
    pub fn subscribe_updates(&self) -> broadcast::Receiver<QuoteUpdate> {
        self.update_tx.subscribe()
    }

    /// Get a receiver for raw price-change updates.
    pub fn subscribe_price_changes(&self) -> broadcast::Receiver<PriceChangeUpdate> {
        self.price_change_tx.subscribe()
    }

    /// Get a receiver for order book snapshot updates (full bid/ask ladders).
    pub fn subscribe_books(&self) -> broadcast::Receiver<Arc<BookMessage>> {
        self.book_tx.subscribe()
    }

    /// Request a WebSocket resubscription cycle.
    ///
    /// The current connection loop will reconnect and apply the latest token set.
    pub fn request_resubscribe(&self) {
        self.resubscribe_requested.store(true, Ordering::SeqCst);
    }

    /// Register token ID to side mapping
    pub async fn register_tokens(&self, up_token_id: &str, down_token_id: &str) {
        self.register_tokens_for_owner(DEFAULT_TOKEN_OWNER, up_token_id, down_token_id)
            .await;
    }

    /// Register a single token with its side under a logical owner.
    pub async fn register_token_for_owner(&self, owner: &str, token_id: &str, side: Side) {
        let mut registry = self.token_registry.write().await;
        registry.register_token(owner, token_id, side);
        drop(registry);
        self.report_subscription_count().await;
        debug!(owner, "Registered token: {} as {:?}", token_id, side);
    }

    /// Register a single token with its side
    pub async fn register_token(&self, token_id: &str, side: Side) {
        self.register_token_for_owner(DEFAULT_TOKEN_OWNER, token_id, side)
            .await;
    }

    /// Reconcile one logical owner's token mapping without affecting other owners.
    ///
    /// Returns `(added, removed, updated, total_merged_tokens)`.
    pub async fn reconcile_token_sides_for_owner(
        &self,
        owner: &str,
        desired: &HashMap<String, Side>,
    ) -> (usize, usize, usize, usize) {
        let mut registry = self.token_registry.write().await;
        let (added, removed, updated) = registry.reconcile_owner(owner, desired);
        let total = registry.merged_len();
        drop(registry);
        self.report_subscription_count().await;
        (added, removed, updated, total)
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
        self.reconcile_token_sides_for_owner(DEFAULT_TOKEN_OWNER, desired)
            .await
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
    pub(super) async fn report_subscription_count(&self) {
        if let Some(f) = self.freshness.get() {
            let sides = self.token_registry.read().await.merged_len();
            let extras = self.extra_tokens.read().await.len();
            f.set_subscription_count(
                crate::platform::DataSource::PolymarketWs,
                (sides + extras) as u64,
            );
        }
    }

    /// Build the current token subscription set from startup seed + dynamic registrations.
    pub(super) async fn build_subscription_list(&self, seed_tokens: &[String]) -> Vec<String> {
        let mut set = HashSet::new();

        for token in seed_tokens {
            if !token.trim().is_empty() {
                set.insert(token.clone());
            }
        }

        {
            let registry = self.token_registry.read().await;
            for token in registry.merged_tokens() {
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

    /// Get side for a token ID
    pub(super) async fn get_side(&self, token_id: &str) -> Option<Side> {
        let registry = self.token_registry.read().await;
        registry.get_side(token_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{DataPlaneFreshness, DataSource};
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn reconcile_token_sides_does_not_deadlock_while_reporting_subscription_count() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        let freshness = Arc::new(DataPlaneFreshness::new());
        ws.set_freshness(Arc::clone(&freshness));

        let mut desired = HashMap::new();
        desired.insert("tok-up".to_string(), Side::Up);
        desired.insert("tok-down".to_string(), Side::Down);

        let (added, removed, updated, total) = timeout(
            Duration::from_millis(250),
            ws.reconcile_token_sides(&desired),
        )
        .await
        .expect("reconcile_token_sides should not deadlock");

        assert_eq!((added, removed, updated, total), (2, 0, 0, 2));

        let metrics = freshness.prometheus_metrics();
        assert!(
            metrics.contains("ploy_source_subscriptions_total{source=\"polymarket_ws\"} 2"),
            "expected freshness subscription count to be updated, got:\n{metrics}"
        );
        assert_eq!(freshness.source_message_count(DataSource::PolymarketWs), 0);
    }

    #[tokio::test]
    async fn owner_reconcile_preserves_other_owner_tokens() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");

        ws.register_token_for_owner("collector:crypto", "collector-up", Side::Up)
            .await;
        ws.register_token_for_owner("collector:crypto", "collector-down", Side::Down)
            .await;

        let mut desired = HashMap::new();
        desired.insert("strategy-up".to_string(), Side::Up);

        let (added, removed, updated, total) = ws
            .reconcile_token_sides_for_owner("strategy:feed:1", &desired)
            .await;
        assert_eq!((added, removed, updated, total), (1, 0, 0, 3));

        let subscriptions = ws.build_subscription_list(&[]).await;
        assert!(subscriptions.iter().any(|token| token == "collector-up"));
        assert!(subscriptions.iter().any(|token| token == "collector-down"));
        assert!(subscriptions.iter().any(|token| token == "strategy-up"));
        assert_eq!(ws.get_side("collector-up").await, Some(Side::Up));
        assert_eq!(ws.get_side("strategy-up").await, Some(Side::Up));

        let empty = HashMap::new();
        let (added, removed, updated, total) = ws
            .reconcile_token_sides_for_owner("strategy:feed:1", &empty)
            .await;
        assert_eq!((added, removed, updated, total), (0, 1, 0, 2));

        let subscriptions = ws.build_subscription_list(&[]).await;
        assert!(subscriptions.iter().any(|token| token == "collector-up"));
        assert!(subscriptions.iter().any(|token| token == "collector-down"));
        assert!(!subscriptions.iter().any(|token| token == "strategy-up"));
    }

    #[tokio::test]
    async fn subscription_count_dedupes_tokens_across_owners() {
        let ws = PolymarketWebSocket::new("wss://example.invalid");
        let freshness = Arc::new(DataPlaneFreshness::new());
        ws.set_freshness(Arc::clone(&freshness));

        ws.register_token_for_owner("collector:crypto", "shared-token", Side::Up)
            .await;
        ws.register_token_for_owner("strategy:feed:1", "shared-token", Side::Up)
            .await;
        ws.register_token_for_owner("strategy:feed:1", "unique-token", Side::Down)
            .await;

        let subscriptions = ws.build_subscription_list(&[]).await;
        assert_eq!(subscriptions.len(), 2);
        assert!(subscriptions.iter().any(|token| token == "shared-token"));
        assert!(subscriptions.iter().any(|token| token == "unique-token"));

        let metrics = freshness.prometheus_metrics();
        assert!(
            metrics.contains("ploy_source_subscriptions_total{source=\"polymarket_ws\"} 2"),
            "expected deduped subscription count, got:\n{metrics}"
        );
    }
}
