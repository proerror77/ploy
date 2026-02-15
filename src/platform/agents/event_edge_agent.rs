//! Event Edge Domain Agent - Politics/Event-driven strategy Agent
//!
//! Implements `DomainAgent` for the EventEdge mispricing scanner,
//! delegating core logic to `EventEdgeCore`.

use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::EventEdgeAgentConfig;
use crate::error::Result;
use crate::platform::{
    AgentRiskParams, AgentStatus, Domain, DomainAgent, DomainEvent, ExecutionReport, OrderIntent,
    OrderPriority,
};
use crate::strategy::event_edge::core::{EventEdgeCore, TradeDecision};
use crate::strategy::event_edge::data_source::{ArenaTextSource, EventDataSource};

/// Platform-integrated EventEdge agent.
///
/// Receives `Tick` events from the platform router, fetches Arena data,
/// runs `EventEdgeCore::scan_and_decide`, and emits `OrderIntent`s.
pub struct EventEdgePlatformAgent {
    core: EventEdgeCore,
    data_source: Box<dyn EventDataSource>,
    status: AgentStatus,
    risk_params: AgentRiskParams,
    /// Maps intent_id → TradeDecision for execution callback tracking.
    pending_intents: HashMap<Uuid, TradeDecision>,
    consecutive_failures: u32,
}

impl EventEdgePlatformAgent {
    pub fn new(core: EventEdgeCore) -> Self {
        let risk_params = AgentRiskParams {
            max_order_value: Decimal::from(core.cfg.shares) * core.cfg.max_entry,
            max_total_exposure: core.cfg.max_daily_spend_usd,
            max_unhedged_positions: 10,
            max_daily_loss: core.cfg.max_daily_spend_usd,
            allow_overnight: true,
            allowed_markets: vec![],
        };

        Self {
            core,
            data_source: Box::new(ArenaTextSource::default()),
            status: AgentStatus::Initializing,
            risk_params,
            pending_intents: HashMap::new(),
            consecutive_failures: 0,
        }
    }

    pub fn with_data_source(mut self, ds: Box<dyn EventDataSource>) -> Self {
        self.data_source = ds;
        self
    }

    pub fn from_config(
        client: crate::adapters::PolymarketClient,
        cfg: EventEdgeAgentConfig,
    ) -> Self {
        Self::new(EventEdgeCore::new(client, cfg))
    }

    /// Convert a `TradeDecision` into an `OrderIntent` for the platform.
    fn decision_to_intent(&self, d: &TradeDecision) -> OrderIntent {
        OrderIntent::new(
            "event_edge",
            Domain::Politics,
            &d.market_slug,
            &d.token_id,
            d.side,
            true,
            d.shares,
            d.limit_price,
        )
        .with_priority(OrderPriority::Normal)
        .with_metadata("strategy", "event_edge")
        .with_metadata("event_id", &d.event_id)
        .with_metadata("outcome", &d.outcome)
        .with_metadata("edge", &d.edge.to_string())
        .with_metadata("p_true", &d.p_true.to_string())
        .with_metadata("net_ev", &d.net_ev.to_string())
    }

    /// Run one full scan cycle across all configured events.
    async fn run_scan_cycle(&mut self) -> Result<Vec<OrderIntent>> {
        let snapshot = self.data_source.fetch_snapshot().await?;
        let arena = snapshot.arena.clone();

        if !self
            .data_source
            .has_changed(&snapshot, &self.core.state.last_arena_updated)
        {
            return Ok(vec![]);
        }

        let event_ids = self.core.resolve_event_ids().await?;
        let mut intents = Vec::new();

        for event_id in &event_ids {
            match self.core.scan_and_decide(event_id, arena.clone()).await {
                Ok(Some(decision)) => {
                    let intent = self.decision_to_intent(&decision);
                    self.pending_intents.insert(intent.intent_id, decision);
                    intents.push(intent);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "EventEdgePlatformAgent: scan failed for {}: {}",
                        event_id, e
                    );
                }
            }
        }

        self.core.state.last_arena_updated = snapshot.last_updated;
        Ok(intents)
    }
}

#[async_trait]
impl DomainAgent for EventEdgePlatformAgent {
    fn id(&self) -> &str {
        "event_edge"
    }

    fn name(&self) -> &str {
        "Event Edge Agent"
    }

    fn domain(&self) -> Domain {
        Domain::Politics
    }

    fn status(&self) -> AgentStatus {
        self.status
    }

    fn risk_params(&self) -> &AgentRiskParams {
        &self.risk_params
    }

    async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        if !self.status.can_trade() {
            return Ok(vec![]);
        }

        match event {
            DomainEvent::Tick(_) => self.run_scan_cycle().await,
            _ => Ok(vec![]),
        }
    }

    async fn on_execution(&mut self, report: ExecutionReport) {
        if let Some(decision) = self.pending_intents.remove(&report.intent_id) {
            if report.is_success() {
                self.consecutive_failures = 0;
                let notional = Decimal::from(decision.shares) * decision.limit_price;
                self.core.record_trade(&decision.token_id, notional);
                info!(
                    "EventEdgePlatformAgent: filled {} shares of \"{}\" @ {} (edge={:.1}pp)",
                    report.filled_shares,
                    decision.outcome,
                    decision.limit_price,
                    decision.edge * Decimal::from(100),
                );
            } else {
                self.consecutive_failures += 1;
                warn!(
                    "EventEdgePlatformAgent: order failed for \"{}\": {:?} (failures={})",
                    decision.outcome, report.error_message, self.consecutive_failures
                );
                if self.consecutive_failures >= 5 {
                    warn!("EventEdgePlatformAgent: too many failures, pausing");
                    self.status = AgentStatus::Paused;
                }
            }
        }
    }

    async fn start(&mut self) -> Result<()> {
        if self.core.targets_empty() {
            warn!("EventEdgePlatformAgent: no targets configured");
            return Ok(());
        }
        info!("EventEdgePlatformAgent: starting");
        self.status = AgentStatus::Running;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("EventEdgePlatformAgent: stopping");
        self.status = AgentStatus::Stopped;
        Ok(())
    }

    fn pause(&mut self) {
        info!("EventEdgePlatformAgent: pausing");
        self.status = AgentStatus::Paused;
    }

    fn resume(&mut self) {
        info!("EventEdgePlatformAgent: resuming");
        self.consecutive_failures = 0;
        self.status = AgentStatus::Running;
    }

    fn position_count(&self) -> usize {
        self.pending_intents.len()
    }

    fn total_exposure(&self) -> Decimal {
        self.core.state.daily_spend_usd
    }

    fn daily_pnl(&self) -> Decimal {
        Decimal::ZERO
    }
}
