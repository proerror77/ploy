use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Duration;
use tracing::{error, info, warn};

/// Deterministic interval-based runner. Standalone mode (no platform).
pub struct EventEdgeAgent {
    core: EventEdgeCore,
}

impl EventEdgeAgent {
    pub fn new(client: PolymarketClient, cfg: EventEdgeAgentConfig) -> Self {
        Self {
            core: EventEdgeCore::new(client, cfg),
        }
    }

    pub async fn run_forever(mut self) -> Result<()> {
        if !self.core.cfg.enabled {
            warn!("EventEdgeAgent disabled in config");
            return Ok(());
        }
        if self.core.targets_empty() {
            warn!("EventEdgeAgent enabled but no targets configured");
            return Ok(());
        }

        let interval = Duration::from_secs(self.core.cfg.interval_secs.max(5));
        info!(
            "EventEdgeAgent started (interval={}s trade={} cooldown={}s max_daily_spend=${})",
            interval.as_secs(),
            self.core.cfg.trade,
            self.core.cfg.cooldown_secs,
            self.core.cfg.max_daily_spend_usd
        );

        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;

            let arena = match fetch_arena_text_snapshot().await {
                Ok(a) => a,
                Err(e) => {
                    warn!("EventEdgeAgent: failed to fetch Arena snapshot: {}", e);
                    continue;
                }
            };

            let event_ids = self.core.resolve_event_ids().await?;

            for event_id in event_ids {
                match self.scan_and_maybe_trade(&event_id, arena.clone()).await {
                    Ok(()) => {}
                    Err(e) => warn!("EventEdgeAgent: scan/trade failed for {}: {}", event_id, e),
                }
            }
        }
    }

    async fn scan_and_maybe_trade(
        &mut self,
        event_id: &str,
        arena: crate::strategy::event_models::arena_text::ArenaTextSnapshot,
    ) -> Result<()> {
        let scan = self.core.scan_event(event_id, Some(arena)).await?;
        info!(
            "EventEdgeAgent: event={} title=\"{}\" end={} conf={:.2}",
            scan.event_id,
            scan.event_title,
            scan.end_time.to_rfc3339(),
            scan.confidence
        );

        self.core.reset_daily_if_needed();

        if let Some(d) = self.core.pick_best_trade(&scan) {
            let notional = Decimal::from(d.shares) * d.limit_price;
            let order =
                OrderRequest::buy_limit(d.token_id.clone(), Side::Up, d.shares, d.limit_price);

            match self.core.client.submit_order(&order).await {
                Ok(resp) => {
                    info!(
                        "EventEdgeAgent: BUY outcome=\"{}\" shares={} ask={:.2}¢ edge={:.1}pp order_id={} status={}",
                        d.outcome, d.shares,
                        d.limit_price * dec!(100),
                        d.edge * dec!(100),
                        resp.id, resp.status
                    );
                    self.core.record_trade(&d.token_id, notional);
                }
                Err(e) => {
                    error!("EventEdgeAgent: order submit failed: {}", e);
                }
            }
        }

        Ok(())
    }
}
