use chrono::Utc;
use tracing::warn;

use crate::agent_runtime::AgentStatus;
use crate::domain::Domain;

use super::super::command::{
    GovernanceAgentSnapshot, GovernancePolicyHistoryEntry, GovernancePolicySnapshot,
    GovernancePolicyUpdate, GovernanceStatusSnapshot,
};
use super::super::governance::{
    governance_domain_snapshot_label, load_governance_policy_history, persist_governance_policy,
    GovernancePolicy,
};
use super::super::state::{AgentSnapshot, GlobalState, QueueStatsSnapshot};
use super::{Coordinator, CoordinatorHandle};

impl CoordinatorHandle {
    /// Read the current global state (non-blocking snapshot)
    pub async fn read_state(&self) -> GlobalState {
        self.global_state.read().await.clone()
    }

    /// Read current account-level governance policy.
    pub async fn governance_policy(&self) -> GovernancePolicySnapshot {
        self.governance.policy_snapshot().await
    }

    /// Read account-level governance policy change history (latest first).
    pub async fn governance_policy_history(
        &self,
        limit: usize,
    ) -> crate::error::Result<Vec<GovernancePolicyHistoryEntry>> {
        let Some(pool) = self.governance_store_pool.as_ref() else {
            return Err(crate::error::PloyError::Validation(
                "governance history store is unavailable in this runtime".to_string(),
            ));
        };
        load_governance_policy_history(pool, &self.account_id, limit).await
    }

    /// Replace account-level governance policy (control-plane managed).
    pub async fn update_governance_policy(
        &self,
        update: GovernancePolicyUpdate,
    ) -> crate::error::Result<GovernancePolicySnapshot> {
        let next = GovernancePolicy::try_from_update(update)
            .map_err(crate::error::PloyError::Validation)?;
        if let Some(pool) = self.governance_store_pool.as_ref() {
            persist_governance_policy(pool, &self.account_id, &next).await?;
        }
        Ok(self.governance.replace_policy(next).await)
    }

    /// Read runtime governance + risk + capital ledger snapshot.
    pub async fn governance_status(&self) -> GovernanceStatusSnapshot {
        let ingress_mode = self.governance.ingress_mode_label().await;
        let domain_ingress_modes = self.governance.domain_ingress_snapshot_rows().await;
        let policy = self.governance.policy_snapshot().await;
        let risk_state = self.risk_gate.state().await;
        let platform_exposure_usd = self.risk_gate.total_exposure().await;
        let (daily_pnl_usd, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit_usd = self.risk_gate.daily_loss_limit();
        let (queue, other_pending_buy_notional_usd) = {
            let queue = self.order_queue.read().await;
            (
                QueueStatsSnapshot::from(queue.stats()),
                queue.pending_buy_notional_excluding_domains(&[
                    Domain::Crypto,
                    Domain::Sports,
                    Domain::Politics,
                    Domain::Economics,
                ]),
            )
        };
        let agents = {
            let global = self.global_state.read().await;
            let mut rows = global
                .agents
                .values()
                .map(|snap| GovernanceAgentSnapshot {
                    agent_id: snap.agent_id.clone(),
                    name: snap.name.clone(),
                    domain: governance_domain_snapshot_label(snap.domain),
                    status: snap.status.to_string().to_ascii_lowercase(),
                    exposure: snap.exposure,
                    daily_pnl: snap.daily_pnl,
                    last_heartbeat: snap.last_heartbeat,
                    error_message: snap.error_message.clone(),
                })
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| a.domain.cmp(&b.domain).then_with(|| a.name.cmp(&b.name)));
            rows
        };

        let (allocators, deployments, allocator_open_notional, allocator_pending_notional) =
            self.capital_policy.ledger_rows().await;
        let open_notional_usd = platform_exposure_usd.max(allocator_open_notional);
        let account_notional_usd =
            open_notional_usd + allocator_pending_notional + other_pending_buy_notional_usd;

        GovernanceStatusSnapshot {
            account_id: self.account_id.clone(),
            ingress_mode,
            domain_ingress_modes,
            policy,
            account_notional_usd,
            platform_exposure_usd,
            risk_state,
            daily_pnl_usd,
            daily_loss_limit_usd,
            queue,
            agents,
            allocators,
            deployments,
            updated_at: Utc::now(),
        }
    }
}

impl Coordinator {
    /// Update agent snapshot in global state
    pub(super) async fn handle_state_update(&self, snapshot: AgentSnapshot) {
        let agent_id = snapshot.agent_id.clone();
        let mut state = self.global_state.write().await;
        state.agents.insert(agent_id, snapshot);
    }

    pub(super) async fn persist_risk_runtime_state(&self) {
        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let drawdown = self.risk_gate.drawdown_snapshot().await;
        let daily_date = Utc::now().date_naive();

        self.journal
            .persist_risk_runtime_state(
                format!("{:?}", risk_state),
                daily_date,
                daily_pnl,
                daily_loss_limit,
                drawdown,
            )
            .await;
    }

    /// Refresh GlobalState from aggregators
    pub(super) async fn refresh_global_state(&self) {
        let portfolio = self.positions.aggregate().await;
        let positions = self.positions.all_positions().await;
        let risk_state = self.risk_gate.state().await;
        let (daily_pnl, _, _) = self.risk_gate.daily_stats().await;
        let daily_loss_limit = self.risk_gate.daily_loss_limit();
        let (current_drawdown, max_drawdown_observed) = self.risk_gate.drawdown_stats().await;
        let max_drawdown_limit = self.risk_gate.max_drawdown_limit();
        let mut circuit_breaker_events = self.risk_gate.circuit_breaker_events().await;
        const MAX_CB_EVENTS: usize = 500;
        if circuit_breaker_events.len() > MAX_CB_EVENTS {
            circuit_breaker_events.drain(..circuit_breaker_events.len() - MAX_CB_EVENTS);
        }
        let queue_stats = self.order_queue.read().await.stats();
        let total_realized = self.positions.total_realized_pnl().await;

        let mut state = self.global_state.write().await;
        state.portfolio = portfolio;
        state.positions = positions;
        state.risk_state = risk_state;
        state.daily_pnl = daily_pnl;
        state.daily_loss_limit = daily_loss_limit;
        state.current_drawdown = current_drawdown;
        state.max_drawdown_observed = max_drawdown_observed;
        state.max_drawdown_limit = max_drawdown_limit;
        state.circuit_breaker_events = circuit_breaker_events;
        state.queue_stats = QueueStatsSnapshot::from(queue_stats);
        state.total_realized_pnl = total_realized;
        state.last_refresh = Utc::now();

        let timeout = chrono::Duration::milliseconds(self.config.heartbeat_timeout_ms as i64);
        let stale_warn_cooldown =
            chrono::Duration::seconds(self.config.heartbeat_stale_warn_cooldown_secs as i64);
        let now = Utc::now();
        let mut stale_warn_at = self.stale_heartbeat_warn_at.write().await;
        for (id, agent) in state.agents.iter_mut() {
            if now - agent.last_heartbeat > timeout && matches!(agent.status, AgentStatus::Running)
            {
                let should_warn = stale_warn_at
                    .get(id)
                    .map(|last_warned_at| now - *last_warned_at >= stale_warn_cooldown)
                    .unwrap_or(true);
                if should_warn {
                    warn!(
                        agent_id = %id,
                        last_heartbeat = %agent.last_heartbeat,
                        stale_ms = (now - agent.last_heartbeat).num_milliseconds(),
                        timeout_ms = self.config.heartbeat_timeout_ms,
                        "agent heartbeat stale"
                    );
                    stale_warn_at.insert(id.clone(), now);
                }
                agent.error_message = Some("heartbeat timeout".into());
            }
        }
        drop(stale_warn_at);
        drop(state);

        self.persist_risk_runtime_state().await;
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as ChronoDuration, Utc};
    use rust_decimal_macros::dec;
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::adapters::PolymarketClient;
    use crate::agent_runtime::AgentStatus;
    use crate::config::ExecutionConfig;
    use crate::domain::Domain;
    use crate::strategy::executor::OrderExecutor;

    use super::super::super::config::CoordinatorConfig;
    use super::super::super::state::AgentSnapshot;
    use super::Coordinator;

    fn make_test_coordinator() -> Coordinator {
        let client = PolymarketClient::new("https://clob.polymarket.com", true)
            .expect("build dry-run polymarket client");
        let executor = Arc::new(OrderExecutor::new(client, ExecutionConfig::default()));
        Coordinator::new(
            CoordinatorConfig::default(),
            executor,
            "acct-test".to_string(),
            HashSet::from([Domain::Crypto, Domain::Sports]),
        )
    }

    #[tokio::test]
    async fn refresh_global_state_marks_stale_running_agents() {
        let coordinator = make_test_coordinator();
        {
            let mut state = coordinator.global_state.write().await;
            state.agents.insert(
                "stale_agent".to_string(),
                AgentSnapshot {
                    agent_id: "stale_agent".to_string(),
                    name: "stale_agent".to_string(),
                    domain: Domain::Crypto,
                    status: AgentStatus::Running,
                    position_count: 0,
                    exposure: dec!(0),
                    daily_pnl: dec!(0),
                    unrealized_pnl: dec!(0),
                    metrics: Default::default(),
                    last_heartbeat: Utc::now() - ChronoDuration::milliseconds(60_000),
                    error_message: None,
                },
            );
        }

        coordinator.refresh_global_state().await;

        let state = coordinator.global_state.read().await;
        let agent = state
            .agents
            .get("stale_agent")
            .expect("stale agent present");
        assert_eq!(agent.error_message.as_deref(), Some("heartbeat timeout"));
    }
}
