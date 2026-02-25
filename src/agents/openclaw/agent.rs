//! OpenClaw meta-agent — Layer 3 orchestrator
//!
//! Implements `TradingAgent` but never trades directly. Instead, it:
//! 1. Detects market regime from BinanceWebSocket volatility data
//! 2. Tracks per-agent performance (Sharpe, win rate, drawdown)
//! 3. Dynamically adjusts capital allocation via governance policy metadata
//! 4. Detects and resolves cross-agent position conflicts
//! 5. Coordinates temporal straddle positions

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use crate::adapters::binance_ws::BinanceWebSocket;
use crate::agents::context::AgentContext;
use crate::agents::traits::TradingAgent;
use crate::coordinator::CoordinatorCommand;
use crate::platform::{AgentRiskParams, AgentStatus, Domain};

use super::allocator::DynamicAllocator;
use super::config::OpenClawConfig;
use super::conflict::ConflictDetector;
use super::performance::PerformanceTracker;
use super::regime::{RegimeDetector, RegimeSnapshot};
use super::straddle::StraddleManager;

/// OpenClaw meta-agent: observes, governs, and allocates — never trades directly.
pub struct OpenClawAgent {
    config: OpenClawConfig,
    binance_ws: Arc<BinanceWebSocket>,
}

impl OpenClawAgent {
    pub fn new(config: OpenClawConfig, binance_ws: Arc<BinanceWebSocket>) -> Self {
        Self { config, binance_ws }
    }
}

#[async_trait]
impl TradingAgent for OpenClawAgent {
    fn id(&self) -> &str {
        &self.config.agent_id
    }

    fn name(&self) -> &str {
        "OpenClaw Meta-Agent"
    }

    fn domain(&self) -> Domain {
        Domain::Custom(0)
    }

    fn risk_params(&self) -> AgentRiskParams {
        // Meta-agent never trades — zero risk params
        AgentRiskParams {
            max_order_value: Decimal::ZERO,
            max_total_exposure: Decimal::ZERO,
            max_unhedged_positions: 0,
            max_daily_loss: Decimal::ZERO,
            allow_overnight: false,
            allowed_markets: vec![],
        }
    }

    async fn run(self, mut ctx: AgentContext) -> crate::error::Result<()> {
        info!(
            agent_id = %self.config.agent_id,
            regime_tick = self.config.regime_tick_secs,
            perf_tick = self.config.perf_tick_secs,
            alloc_tick = self.config.alloc_tick_secs,
            "OpenClaw meta-agent starting"
        );

        // Report initial state
        ctx.report_state(
            "OpenClaw Meta-Agent",
            AgentStatus::Initializing,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            None,
        )
        .await?;

        // Initialize sub-components
        let mut regime_detector = RegimeDetector::new(
            self.config.regime.clone(),
            self.config.btc_symbol.clone(),
            self.binance_ws.clone(),
        );
        let mut perf_tracker = PerformanceTracker::new(
            self.config.allocator.clone(),
            self.config.perf_window_secs,
        );
        let mut allocator = DynamicAllocator::new(self.config.allocator.clone());
        let mut straddle_mgr = StraddleManager::new(self.config.straddle.clone());

        let mut paused = false;
        let mut last_regime_snapshot: Option<RegimeSnapshot> = None;
        let mut paused_agents: Vec<String> = Vec::new();

        // Timer intervals
        let mut regime_tick = tokio::time::interval(tokio::time::Duration::from_secs(
            self.config.regime_tick_secs,
        ));
        let mut perf_tick = tokio::time::interval(tokio::time::Duration::from_secs(
            self.config.perf_tick_secs,
        ));
        let mut alloc_tick = tokio::time::interval(tokio::time::Duration::from_secs(
            self.config.alloc_tick_secs,
        ));
        let mut heartbeat_tick =
            tokio::time::interval(tokio::time::Duration::from_secs(5));

        regime_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        perf_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        alloc_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Report running
        ctx.report_state(
            "OpenClaw Meta-Agent",
            AgentStatus::Running,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            None,
        )
        .await?;

        loop {
            tokio::select! {
                // --- Regime detection ---
                _ = regime_tick.tick() => {
                    if paused { continue; }

                    let (snapshot, changed) = regime_detector.tick().await;
                    if changed {
                        info!(
                            regime = %snapshot.regime,
                            confidence = format!("{:.2}", snapshot.confidence),
                            vol_ratio = snapshot.vol_ratio.map(|v| format!("{:.3}", v)),
                            "OpenClaw: regime changed"
                        );
                    }
                    last_regime_snapshot = Some(snapshot);
                }

                // --- Performance tracking + conflict detection ---
                _ = perf_tick.tick() => {
                    if paused { continue; }

                    let state = ctx.read_global_state().await;
                    perf_tracker.update(&state);

                    // Conflict detection
                    let conflicts = ConflictDetector::detect(&state);
                    if !conflicts.is_empty() {
                        let performances = perf_tracker.all();
                        let resolutions = ConflictDetector::resolve(&conflicts, performances);
                        for resolution in resolutions {
                            // Skip if already tracked as paused
                            if paused_agents.contains(&resolution.pause_agent_id) {
                                continue;
                            }
                            // Use control command to pause conflicting agent
                            if let Err(e) = ctx.submit_pause_agent(&resolution.pause_agent_id).await {
                                warn!(
                                    agent = %resolution.pause_agent_id,
                                    error = %e,
                                    "failed to pause conflicting agent"
                                );
                            } else {
                                if !paused_agents.contains(&resolution.pause_agent_id) {
                                    paused_agents.push(resolution.pause_agent_id.clone());
                                }
                            }
                        }
                    }

                    // Straddle tick (if enabled)
                    if straddle_mgr.is_enabled() {
                        if let Some(spot) = self.binance_ws.price_cache()
                            .get(&self.config.btc_symbol).await
                        {
                            let _signals = straddle_mgr.tick(spot.price);
                            // Signals would be pushed as governance metadata for crypto agent
                        }
                    }
                }

                // --- Capital reallocation ---
                _ = alloc_tick.tick() => {
                    if paused { continue; }

                    let regime = regime_detector.current();
                    let performances = perf_tracker.all();

                    if performances.is_empty() {
                        debug!("OpenClaw: no agent performance data yet, skipping allocation");
                        continue;
                    }

                    let update = allocator.decide(regime, performances, &paused_agents);

                    // Build governance policy update with metadata
                    // Read current snapshot first, then merge to avoid overwriting non-OpenClaw metadata
                    let policy_snapshot = ctx.read_governance_policy().await;
                    let mut all_metadata = policy_snapshot.metadata.clone();
                    all_metadata.extend(update.metadata);

                    // Merge straddle metadata
                    if straddle_mgr.is_enabled() {
                        all_metadata.extend(straddle_mgr.governance_metadata());
                    }

                    let gov_update = crate::coordinator::GovernancePolicyUpdate {
                        block_new_intents: policy_snapshot.block_new_intents,
                        blocked_domains: policy_snapshot.blocked_domains.clone(),
                        max_intent_notional_usd: policy_snapshot.max_intent_notional_usd,
                        max_total_notional_usd: policy_snapshot.max_total_notional_usd,
                        updated_by: "openclaw".to_string(),
                        reason: Some(format!("regime={}, agents={}", regime, performances.len())),
                        metadata: all_metadata,
                    };

                    if let Err(e) = ctx.update_governance_policy(gov_update).await {
                        warn!(error = %e, "OpenClaw: failed to update governance policy");
                    } else {
                        debug!(
                            regime = %regime,
                            "OpenClaw: governance policy updated"
                        );
                    }

                    // Execute pause/resume
                    for agent_id in &update.agents_to_pause {
                        if let Err(e) = ctx.submit_pause_agent(agent_id).await {
                            warn!(agent = %agent_id, error = %e, "OpenClaw: failed to pause agent");
                        } else if !paused_agents.contains(agent_id) {
                            paused_agents.push(agent_id.clone());
                        }
                    }
                    for agent_id in &update.agents_to_resume {
                        if let Err(e) = ctx.submit_resume_agent(agent_id).await {
                            warn!(agent = %agent_id, error = %e, "OpenClaw: failed to resume agent");
                        } else {
                            paused_agents.retain(|id| id != agent_id);
                        }
                    }
                }

                // --- Heartbeat ---
                _ = heartbeat_tick.tick() => {
                    let status = if paused { AgentStatus::Paused } else { AgentStatus::Running };
                    let mut metrics = HashMap::new();

                    if let Some(ref snap) = last_regime_snapshot {
                        metrics.insert("regime".to_string(), snap.regime.to_string());
                        metrics.insert("regime_confidence".to_string(), format!("{:.2}", snap.confidence));
                        if let Some(vr) = snap.vol_ratio {
                            metrics.insert("vol_ratio".to_string(), format!("{:.3}", vr));
                        }
                    }

                    metrics.insert("tracked_agents".to_string(), perf_tracker.all().len().to_string());
                    metrics.insert("paused_agents".to_string(), paused_agents.len().to_string());
                    metrics.insert(
                        "active_straddles".to_string(),
                        straddle_mgr.active_straddles().len().to_string(),
                    );

                    if let Err(e) = ctx.report_state_with_metrics(
                        "OpenClaw Meta-Agent",
                        status,
                        0,
                        Decimal::ZERO,
                        Decimal::ZERO,
                        Decimal::ZERO,
                        metrics,
                        None,
                    ).await {
                        warn!(error = %e, "OpenClaw: failed to report heartbeat");
                    }
                }

                // --- Coordinator commands ---
                Some(cmd) = ctx.command_rx().recv() => {
                    match cmd {
                        CoordinatorCommand::Pause => {
                            info!("OpenClaw: paused");
                            paused = true;
                        }
                        CoordinatorCommand::Resume => {
                            info!("OpenClaw: resumed");
                            paused = false;
                        }
                        CoordinatorCommand::Shutdown => {
                            info!("OpenClaw: shutting down");
                            break;
                        }
                        CoordinatorCommand::ForceClose => {
                            info!("OpenClaw: force close (meta-agent has no positions)");
                            break;
                        }
                        CoordinatorCommand::HealthCheck(tx) => {
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: "OpenClaw Meta-Agent".to_string(),
                                domain: Domain::Custom(0),
                                status: if paused { AgentStatus::Paused } else { AgentStatus::Running },
                                position_count: 0,
                                exposure: Decimal::ZERO,
                                daily_pnl: Decimal::ZERO,
                                unrealized_pnl: Decimal::ZERO,
                                metrics: HashMap::new(),
                                last_heartbeat: chrono::Utc::now(),
                                error_message: None,
                            };
                            let _ = tx.send(crate::coordinator::command::AgentHealthResponse {
                                snapshot,
                                is_healthy: true,
                                uptime_secs: 0,
                                orders_submitted: 0,
                                orders_filled: 0,
                            });
                        }
                    }
                }
            }
        }

        // Final state report
        let _ = ctx
            .report_state(
                "OpenClaw Meta-Agent",
                AgentStatus::Stopped,
                0,
                Decimal::ZERO,
                Decimal::ZERO,
                Decimal::ZERO,
                None,
            )
            .await;

        info!("OpenClaw meta-agent stopped");
        Ok(())
    }
}
