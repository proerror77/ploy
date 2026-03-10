use super::*;
use tracing::info;

impl PolymarketWebSocket {
    /// Get a receiver for quote updates
    pub fn subscribe_updates(&self) -> broadcast::Receiver<QuoteUpdate> {
        self.update_tx.subscribe()
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
    pub(super) async fn report_subscription_count(&self) {
        if let Some(f) = self.freshness.get() {
            let sides = self.token_to_side.read().await.len();
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

    /// Get side for a token ID
    pub(super) async fn get_side(&self, token_id: &str) -> Option<Side> {
        let mapping = self.token_to_side.read().await;
        mapping.get(token_id).copied()
    }
}
