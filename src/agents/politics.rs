//! PoliticsTradingAgent — pull-based agent for event edge / politics strategy
//!
//! Polls Arena data on a 5-minute interval, runs EventEdgeCore scan logic,
//! and submits OrderIntents via the coordinator.

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::agents::{AgentContext, TradingAgent};
use crate::coordinator::CoordinatorCommand;
use crate::error::Result;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::event_edge::data_source::{ArenaTextSource, EventDataSource};

/// Configuration for the PoliticsTradingAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoliticsTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for PoliticsTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "politics".into(),
            name: "Event Edge".into(),
            poll_interval_secs: 300, // 5 minutes
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

/// Pull-based politics/event trading agent wrapping EventEdgeCore
pub struct PoliticsTradingAgent {
    config: PoliticsTradingConfig,
    core: EventEdgeCore,
    data_source: Box<dyn EventDataSource>,
}

impl PoliticsTradingAgent {
    pub fn new(config: PoliticsTradingConfig, core: EventEdgeCore) -> Self {
        Self {
            config,
            core,
            data_source: Box::new(ArenaTextSource::default()),
        }
    }

    pub fn with_data_source(mut self, ds: Box<dyn EventDataSource>) -> Self {
        self.data_source = ds;
        self
    }
}

#[async_trait]
impl TradingAgent for PoliticsTradingAgent {
    fn id(&self) -> &str {
        &self.config.agent_id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn domain(&self) -> Domain {
        Domain::Politics
    }

    fn risk_params(&self) -> AgentRiskParams {
        self.config.risk_params.clone()
    }

    async fn run(mut self, mut ctx: AgentContext) -> Result<()> {
        info!(agent = self.config.agent_id, "politics agent starting");
        let config_hash = {
            let payload = serde_json::to_vec(&self.config).unwrap_or_default();
            let mut hasher = Sha256::new();
            hasher.update(payload);
            format!("{:x}", hasher.finalize())
        };

        if self.core.targets_empty() {
            warn!(
                agent = self.config.agent_id,
                "no event targets configured, exiting"
            );
            return Ok(());
        }

        let mut status = AgentStatus::Running;
        let mut position_count: usize = 0;
        let mut total_exposure = Decimal::ZERO;
        let daily_pnl = Decimal::ZERO;

        let poll_dur = tokio::time::Duration::from_secs(self.config.poll_interval_secs);
        let heartbeat_dur = tokio::time::Duration::from_secs(self.config.heartbeat_interval_secs);
        let mut poll_tick = tokio::time::interval(poll_dur);
        let mut heartbeat_tick = tokio::time::interval(heartbeat_dur);
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                // --- Arena/event scan cycle ---
                _ = poll_tick.tick() => {
                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }

                    // Fetch latest data snapshot
                    let snapshot = match self.data_source.fetch_snapshot().await {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(agent = self.config.agent_id, error = %e, "data fetch failed");
                            continue;
                        }
                    };

                    // Check if data has changed since last scan
                    if !self.data_source.has_changed(&snapshot, &self.core.state.last_arena_updated) {
                        debug!(agent = self.config.agent_id, "no data change, skipping scan");
                        continue;
                    }

                    let arena = snapshot.arena.clone();

                    // Resolve event IDs to scan
                    let event_ids = match self.core.resolve_event_ids().await {
                        Ok(ids) => ids,
                        Err(e) => {
                            warn!(agent = self.config.agent_id, error = %e, "failed to resolve events");
                            continue;
                        }
                    };

                    // Scan each event for trading opportunities
                    for event_id in &event_ids {
                        match self.core.scan_and_decide(event_id, arena.clone()).await {
                            Ok(Some(decision)) => {
                                let intent = OrderIntent::new(
                                    &self.config.agent_id,
                                    Domain::Politics,
                                    &decision.market_slug,
                                    &decision.token_id,
                                    decision.side,
                                    true,
                                    decision.shares,
                                    decision.limit_price,
                                )
                                .with_priority(OrderPriority::Normal)
                                .with_metadata("strategy", "event_edge")
                                .with_metadata("event_id", &decision.event_id)
                                .with_metadata("outcome", &decision.outcome)
                                .with_metadata("edge", &decision.edge.to_string())
                                .with_metadata("p_true", &decision.p_true.to_string())
                                .with_metadata("signal_type", "event_edge_entry")
                                .with_metadata("signal_confidence", &decision.p_true.to_string())
                                .with_metadata("signal_fair_value", &decision.p_true.to_string())
                                .with_metadata("signal_market_price", &decision.limit_price.to_string())
                                .with_metadata("signal_edge", &decision.edge.to_string())
                                .with_metadata("config_hash", &config_hash);

                                info!(
                                    agent = self.config.agent_id,
                                    event_id,
                                    outcome = %decision.outcome,
                                    edge = %decision.edge,
                                    "signal detected, submitting order"
                                );

                                if let Err(e) = ctx.submit_order(intent).await {
                                    warn!(agent = self.config.agent_id, error = %e, "submit failed");
                                } else {
                                    position_count += 1;
                                    total_exposure += Decimal::from(decision.shares) * decision.limit_price;
                                    self.core.record_trade(&decision.token_id, Decimal::from(decision.shares) * decision.limit_price);
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!(agent = self.config.agent_id, event_id, error = %e, "scan failed");
                            }
                        }
                    }

                    self.core.state.last_arena_updated = snapshot.last_updated;
                }

                // --- Coordinator commands ---
                cmd = ctx.command_rx().recv() => {
                    match cmd {
                        Some(CoordinatorCommand::Pause) => {
                            info!(agent = self.config.agent_id, "pausing");
                            status = AgentStatus::Paused;
                        }
                        Some(CoordinatorCommand::Resume) => {
                            info!(agent = self.config.agent_id, "resuming");
                            status = AgentStatus::Running;
                        }
                        Some(CoordinatorCommand::Shutdown) | None => {
                            info!(agent = self.config.agent_id, "shutting down");
                            break;
                        }
                        Some(CoordinatorCommand::ForceClose) => {
                            warn!(agent = self.config.agent_id, "force close");
                            break;
                        }
                        Some(CoordinatorCommand::HealthCheck(tx)) => {
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: self.config.name.clone(),
                                domain: Domain::Politics,
                                status,
                                position_count,
                                exposure: total_exposure,
                                daily_pnl,
                                unrealized_pnl: Decimal::ZERO,
                                last_heartbeat: Utc::now(),
                                error_message: None,
                            };
                            let _ = tx.send(crate::coordinator::AgentHealthResponse {
                                snapshot,
                                is_healthy: matches!(status, AgentStatus::Running),
                                uptime_secs: 0,
                                orders_submitted: position_count as u64,
                                orders_filled: 0,
                            });
                        }
                    }
                }

                // --- Heartbeat ---
                _ = heartbeat_tick.tick() => {
                    let _ = ctx.report_state(
                        &self.config.name,
                        status,
                        position_count,
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
                        None,
                    ).await;
                }
            }
        }

        info!(agent = self.config.agent_id, "politics agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = PoliticsTradingConfig::default();
        assert_eq!(cfg.agent_id, "politics");
        assert_eq!(cfg.poll_interval_secs, 300);
    }
}
