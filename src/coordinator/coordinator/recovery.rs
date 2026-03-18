use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info};

use crate::domain::Domain;
use crate::error::Result;

use super::super::governance::load_governance_policy;
use super::Coordinator;

impl Coordinator {
    /// Enable DB logging for order execution outcomes (including dry-run).
    pub fn set_execution_log_pool(&mut self, pool: sqlx::PgPool) {
        self.journal.set_pool(pool);
    }

    /// Restore persisted risk runtime state (drawdown + daily pnl continuity).
    pub async fn restore_risk_runtime_state(&self) -> Result<()> {
        let Some(snapshot) = self.journal.load_risk_runtime_state().await? else {
            return Ok(());
        };

        self.risk_gate
            .restore_drawdown_snapshot(snapshot.drawdown)
            .await;

        if snapshot.daily_date == Some(Utc::now().date_naive()) {
            self.risk_gate
                .restore_daily_pnl_for_today(snapshot.daily_pnl)
                .await;
        }

        if snapshot.risk_state_raw.eq_ignore_ascii_case("halted") {
            self.risk_gate
                .trigger_circuit_breaker("restored persisted halted risk state")
                .await;
        }

        info!(
            account_id = %self.account_id,
            daily_pnl = %snapshot.daily_pnl,
            risk_state = %snapshot.risk_state_raw,
            "restored persisted risk runtime state"
        );
        Ok(())
    }

    /// Enable DB persistence for coordinator governance policy.
    pub fn set_governance_store_pool(&mut self, pool: sqlx::PgPool) {
        self.governance_store_pool = Some(pool);
    }

    /// Restore runtime governance policy from DB (if a persisted row exists).
    pub async fn load_persisted_governance_policy(&self) -> Result<()> {
        let Some(pool) = self.governance_store_pool.as_ref() else {
            return Ok(());
        };

        let Some(policy) = load_governance_policy(pool, &self.account_id).await? else {
            return Ok(());
        };

        let snapshot = self.governance.replace_policy(policy).await;
        info!(
            account_id = %self.account_id,
            updated_by = %snapshot.updated_by,
            updated_at = %snapshot.updated_at,
            "restored governance policy from DB"
        );
        Ok(())
    }

    /// Rebuild runtime position/allocator state from persisted execution fills.
    ///
    /// This prevents cold-start underestimation of account exposure when a process restarts.
    pub async fn restore_runtime_state_from_execution_log(&self) -> Result<()> {
        let today = Utc::now().date_naive();
        let window_start = DateTime::<Utc>::from_naive_utc_and_offset(
            today
                .and_hms_opt(0, 0, 0)
                .expect("00:00:00 is always a valid UTC time"),
            Utc,
        );
        let window_end = window_start + ChronoDuration::days(1);
        let dry_run = self.executor.is_dry_run();

        let Some(restore_data) = self
            .journal
            .load_execution_restore_data(dry_run, window_start, window_end)
            .await?
        else {
            return Ok(());
        };
        let fills = restore_data.fills;
        let outcomes_today = restore_data.outcomes_today;

        if fills.is_empty() && outcomes_today.is_empty() {
            return Ok(());
        }

        self.positions.clear().await;
        self.capital_policy.reset_runtime_state().await;

        let restored_fill_count = fills.len();
        let mut restored_agents = HashSet::new();
        let mut daily_total_pnl = Decimal::ZERO;
        let mut daily_domain_pnl: HashMap<Domain, Decimal> = HashMap::new();
        let mut daily_agent_pnl: HashMap<String, Decimal> = HashMap::new();

        for fill in fills {
            let mut intent = crate::coordinator::OrderIntent::new(
                fill.agent_id.clone(),
                fill.domain,
                fill.market_slug.clone(),
                fill.token_id.clone(),
                fill.side,
                fill.is_buy,
                fill.filled_shares,
                fill.fill_price,
            );
            intent.intent_id = fill.intent_id;
            intent.metadata = fill.metadata.clone();
            if let Some(client_order_id) = intent
                .metadata
                .get("client_order_id")
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                intent.client_order_id = client_order_id.to_string();
            } else {
                intent.client_order_id = format!("intent:{}", fill.intent_id);
            }
            intent.created_at = fill.executed_at;

            self.settle_domain_success(&intent, fill.filled_shares, fill.fill_price)
                .await;

            if fill.is_buy {
                let position_id = self
                    .positions
                    .open_position(
                        &fill.agent_id,
                        fill.domain,
                        &fill.market_slug,
                        &fill.token_id,
                        fill.side,
                        fill.filled_shares,
                        fill.fill_price,
                    )
                    .await;
                debug!(
                    agent_id = %fill.agent_id,
                    intent_id = %fill.intent_id,
                    %position_id,
                    shares = fill.filled_shares,
                    fill_price = %fill.fill_price,
                    "restored tracked BUY position"
                );
            } else {
                let realized_pnl = self
                    .apply_sell_fill_to_positions(&intent, fill.filled_shares, fill.fill_price)
                    .await;
                if fill.executed_at >= window_start && fill.executed_at < window_end {
                    daily_total_pnl += realized_pnl;
                    *daily_domain_pnl.entry(fill.domain).or_insert(Decimal::ZERO) += realized_pnl;
                    *daily_agent_pnl
                        .entry(fill.agent_id.clone())
                        .or_insert(Decimal::ZERO) += realized_pnl;
                }
            }
            restored_agents.insert(fill.agent_id);
        }

        let mut daily_order_count: u32 = 0;
        let mut daily_success_count: u32 = 0;
        let mut daily_failure_count: u32 = 0;
        let mut global_consecutive_failures: u32 = 0;
        let mut per_agent_consecutive_failures: HashMap<String, u32> = HashMap::new();
        let mut last_risk_event_at: Option<DateTime<Utc>> = None;

        for outcome in outcomes_today {
            daily_order_count = daily_order_count.saturating_add(1);
            last_risk_event_at = Some(outcome.executed_at);
            if outcome.is_failure {
                daily_failure_count = daily_failure_count.saturating_add(1);
                global_consecutive_failures = global_consecutive_failures.saturating_add(1);
                let entry = per_agent_consecutive_failures
                    .entry(outcome.agent_id)
                    .or_insert(0);
                *entry = entry.saturating_add(1);
            } else {
                daily_success_count = daily_success_count.saturating_add(1);
                global_consecutive_failures = 0;
                per_agent_consecutive_failures.insert(outcome.agent_id, 0);
            }
        }

        self.risk_gate
            .restore_runtime_counters(
                today,
                daily_total_pnl,
                daily_domain_pnl,
                daily_order_count,
                daily_success_count,
                daily_failure_count,
                global_consecutive_failures,
                daily_agent_pnl,
                per_agent_consecutive_failures,
                last_risk_event_at,
            )
            .await;

        for agent_id in &restored_agents {
            self.refresh_risk_exposure_for_agent(agent_id).await;
        }
        self.refresh_global_state().await;

        info!(
            account_id = %self.account_id,
            fill_count = restored_fill_count,
            restored_agents = restored_agents.len(),
            daily_order_count,
            daily_success_count,
            daily_failure_count,
            global_consecutive_failures,
            "restored coordinator runtime state from execution log"
        );
        Ok(())
    }
}
