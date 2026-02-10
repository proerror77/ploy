//! Background discovery service — scans Polymarket for new events and
//! populates the event registry.

use crate::adapters::postgres::PostgresStore;
use crate::adapters::PolymarketClient;
use crate::config::DiscoveryConfig;
use crate::strategy::registry::EventUpsertRequest;
use std::time::Duration;
use tokio::time;
use tracing::{debug, info, warn};

pub struct DiscoveryService {
    store: PostgresStore,
    client: PolymarketClient,
    cfg: DiscoveryConfig,
}

impl DiscoveryService {
    pub fn new(store: PostgresStore, client: PolymarketClient, cfg: DiscoveryConfig) -> Self {
        Self {
            store,
            client,
            cfg,
        }
    }

    /// Run the discovery loop forever (call from a spawned task).
    pub async fn run_forever(&self) {
        let interval = Duration::from_secs(self.cfg.scan_interval_secs);
        info!(
            "DiscoveryService: starting (interval={}s, sports={:?}, general={:?})",
            self.cfg.scan_interval_secs, self.cfg.sports_keywords, self.cfg.general_keywords
        );

        let mut ticker = time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = self.run_once().await {
                warn!("DiscoveryService: scan cycle failed: {e}");
            }
        }
    }

    /// Execute a single discovery cycle.
    pub async fn run_once(&self) -> crate::error::Result<()> {
        let mut total = 0u64;

        // Scan sports keywords
        for keyword in &self.cfg.sports_keywords {
            match self.scan_keyword(keyword, "sports").await {
                Ok(n) => total += n,
                Err(e) => warn!("DiscoveryService: sports scan '{keyword}' failed: {e}"),
            }
        }

        // Scan general keywords
        for keyword in &self.cfg.general_keywords {
            match self.scan_keyword(keyword, "politics").await {
                Ok(n) => total += n,
                Err(e) => warn!("DiscoveryService: general scan '{keyword}' failed: {e}"),
            }
        }

        // Expire stale events
        let expired = self.store.expire_stale_events().await?;
        if expired > 0 {
            info!("DiscoveryService: expired {expired} stale events");
        }

        debug!("DiscoveryService: cycle complete — upserted {total} events, expired {expired}");
        Ok(())
    }

    /// Search Polymarket for a keyword and upsert results into the registry.
    async fn scan_keyword(&self, keyword: &str, domain: &str) -> crate::error::Result<u64> {
        let markets = self.client.search_markets(keyword).await?;
        let mut count = 0u64;

        for m in &markets {
            // MarketSummary has: condition_id, question (Option), slug (Option), active
            let title = match &m.question {
                Some(q) => q.clone(),
                None => continue,
            };

            let req = EventUpsertRequest {
                title,
                source: "polymarket".to_string(),
                event_id: None, // search_markets doesn't return event_id
                slug: m.slug.clone(),
                domain: domain.to_string(),
                strategy_hint: None,
                status: None, // defaults to "discovered"
                confidence: None,
                settlement_rule: None,
                end_time: None,
                market_slug: m.slug.clone(),
                condition_id: Some(m.condition_id.clone()),
                token_ids: None,
                outcome_prices: None,
                metadata: None,
            };

            match self.store.upsert_event(&req).await {
                Ok(_) => count += 1,
                Err(e) => debug!("DiscoveryService: upsert failed for '{}': {e}", req.title),
            }
        }

        Ok(count)
    }
}
