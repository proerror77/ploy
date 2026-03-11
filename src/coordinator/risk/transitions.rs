use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tracing::{debug, error, info, warn};

use super::stats::DailyStats;
use super::{Domain, DrawdownSnapshot, PlatformRiskState, RiskGate};

impl RiskGate {
    /// 記錄成功執行
    pub async fn record_success(&self, agent_id: &str, pnl: Decimal) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        self.consecutive_failures.store(0, Ordering::SeqCst);

        {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.consecutive_failures = 0;
            stats.realized_pnl += pnl;
        }

        let mut halt_reason: Option<String> = None;
        {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl += pnl;
            if let Some(domain) = domain {
                *daily.domain_pnl.entry(domain).or_insert(Decimal::ZERO) += pnl;
                if let Some(domain_limit) = self.config.domain_daily_loss_limit(domain) {
                    let domain_pnl = daily
                        .domain_pnl
                        .get(&domain)
                        .copied()
                        .unwrap_or(Decimal::ZERO);
                    if domain_pnl < Decimal::ZERO && domain_pnl.abs() >= domain_limit {
                        halt_reason = Some(format!(
                            "{} daily loss limit exceeded (pnl={}, limit={})",
                            domain, domain_pnl, domain_limit
                        ));
                    }
                }
            }
            daily.order_count += 1;
            daily.success_count += 1;

            if halt_reason.is_none()
                && daily.total_pnl < Decimal::ZERO
                && daily.total_pnl.abs() >= self.config.daily_loss_limit
            {
                halt_reason = Some(format!(
                    "Daily loss limit exceeded (pnl={}, limit={})",
                    daily.total_pnl, self.config.daily_loss_limit
                ));
            }
        }

        if let Some(reason) = self.apply_realized_pnl_to_drawdown(pnl).await {
            halt_reason.get_or_insert(reason);
        }

        if let Some(reason) = halt_reason {
            self.trigger_circuit_breaker(&reason).await;
            return;
        }

        {
            let mut state = self.state.write().await;
            if *state == PlatformRiskState::Elevated {
                *state = PlatformRiskState::Normal;
                info!("Risk state normalized after successful execution");
            }
        }
    }

    /// 記錄失敗
    pub async fn record_failure(&self, agent_id: &str, reason: &str) {
        let global_failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;

        let agent_failures = {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.consecutive_failures += 1;
            stats.consecutive_failures
        };

        {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.order_count += 1;
            daily.failure_count += 1;
        }

        warn!(
            "Agent {} failed: {}. Failures: agent={}, global={}",
            agent_id, reason, agent_failures, global_failures
        );

        if global_failures >= self.config.max_consecutive_failures {
            self.trigger_circuit_breaker("Too many consecutive failures")
                .await;
        } else if global_failures >= self.config.max_consecutive_failures / 2 {
            *self.state.write().await = PlatformRiskState::Elevated;
            warn!("Platform risk elevated due to failures");
        }
    }

    /// 記錄損失
    pub async fn record_loss(&self, agent_id: &str, loss: Decimal) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        self.consecutive_failures.store(0, Ordering::SeqCst);

        {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            stats.consecutive_failures = 0;
            stats.realized_pnl -= loss.abs();
        }

        let mut halt_reason = {
            let mut daily = self.daily_stats.write().await;
            self.ensure_daily_reset(&mut daily);
            daily.total_pnl -= loss.abs();
            if let Some(domain) = domain {
                *daily.domain_pnl.entry(domain).or_insert(Decimal::ZERO) -= loss.abs();
            }
            daily.order_count += 1;
            if daily.total_pnl.abs() >= self.config.daily_loss_limit {
                Some("Daily loss limit exceeded".to_string())
            } else {
                None
            }
        };

        if let Some(reason) = self.apply_realized_pnl_to_drawdown(-loss.abs()).await {
            halt_reason.get_or_insert(reason);
        }

        if let Some(reason) = halt_reason {
            self.trigger_circuit_breaker(&reason).await;
        }
    }

    /// 觸發熔斷
    pub async fn trigger_circuit_breaker(&self, reason: &str) {
        let mut state = self.state.write().await;
        if *state == PlatformRiskState::Halted {
            return;
        }
        error!("CIRCUIT BREAKER TRIGGERED: {}", reason);
        *state = PlatformRiskState::Halted;
        drop(state);

        *self.halted_at.write().await = Some(Utc::now());
        self.push_circuit_event(reason.to_string(), PlatformRiskState::Halted)
            .await;
    }

    /// 重置熔斷
    pub async fn reset_circuit_breaker(&self) {
        self.reset_circuit_breaker_with_reason("reset".to_string())
            .await;
    }

    /// Restore runtime counters after coordinator cold-start replay.
    pub async fn restore_runtime_counters(
        &self,
        date: NaiveDate,
        total_pnl: Decimal,
        domain_pnl: HashMap<Domain, Decimal>,
        order_count: u32,
        success_count: u32,
        failure_count: u32,
        consecutive_failures: u32,
        agent_realized_pnl: HashMap<String, Decimal>,
        agent_consecutive_failures: HashMap<String, u32>,
        last_risk_event_at: Option<DateTime<Utc>>,
    ) {
        {
            let mut daily = self.daily_stats.write().await;
            *daily = DailyStats {
                date: Some(date),
                total_pnl,
                domain_pnl,
                order_count,
                success_count,
                failure_count,
            };
        }

        self.consecutive_failures
            .store(consecutive_failures, Ordering::SeqCst);

        {
            let mut stats_map = self.agent_stats.write().await;
            for (agent_id, realized_pnl) in agent_realized_pnl {
                let stats = stats_map.entry(agent_id).or_default();
                stats.realized_pnl = realized_pnl;
            }
            for (agent_id, failures) in agent_consecutive_failures {
                let stats = stats_map.entry(agent_id).or_default();
                stats.consecutive_failures = failures;
            }
        }

        let failure_limit = self.config.max_consecutive_failures.max(1);
        let daily_loss_exceeded =
            total_pnl < Decimal::ZERO && total_pnl.abs() >= self.config.daily_loss_limit;
        let next_state = if daily_loss_exceeded || consecutive_failures >= failure_limit {
            PlatformRiskState::Halted
        } else if consecutive_failures >= (failure_limit / 2).max(1) {
            PlatformRiskState::Elevated
        } else {
            PlatformRiskState::Normal
        };

        {
            let mut state = self.state.write().await;
            *state = next_state;
        }

        {
            let mut halted_at = self.halted_at.write().await;
            *halted_at = if next_state == PlatformRiskState::Halted {
                Some(last_risk_event_at.unwrap_or_else(Utc::now))
            } else {
                None
            };
        }

        debug!(
            date = %date,
            total_pnl = %total_pnl,
            order_count,
            success_count,
            failure_count,
            consecutive_failures,
            state = ?next_state,
            "restored risk gate runtime counters"
        );
    }

    /// Restore drawdown state from persisted snapshot.
    pub async fn restore_drawdown_snapshot(&self, snapshot: DrawdownSnapshot) {
        let mut drawdown = self.drawdown_stats.write().await;

        let current_equity = snapshot.current_equity;
        let equity_peak = snapshot.equity_peak.max(current_equity);
        let current_drawdown = (equity_peak - current_equity).max(Decimal::ZERO);
        let max_drawdown_observed = snapshot.max_drawdown_observed.max(current_drawdown);

        drawdown.current_equity = current_equity;
        drawdown.equity_peak = equity_peak;
        drawdown.current_drawdown = current_drawdown;
        drawdown.max_drawdown_observed = max_drawdown_observed;
    }

    /// Restore today's realized PnL (for daily loss-limit continuity after restart).
    pub async fn restore_daily_pnl_for_today(&self, total_pnl: Decimal) {
        let mut daily = self.daily_stats.write().await;
        *daily = DailyStats {
            date: Some(Utc::now().date_naive()),
            total_pnl,
            ..Default::default()
        };
    }

    fn ensure_daily_reset(&self, daily: &mut DailyStats) {
        let today = Utc::now().date_naive();
        if daily.date != Some(today) {
            *daily = DailyStats {
                date: Some(today),
                ..Default::default()
            };
        }
    }

    async fn apply_realized_pnl_to_drawdown(&self, pnl_delta: Decimal) -> Option<String> {
        let mut drawdown = self.drawdown_stats.write().await;
        drawdown.current_equity += pnl_delta;

        if drawdown.current_equity > drawdown.equity_peak {
            drawdown.equity_peak = drawdown.current_equity;
        }

        drawdown.current_drawdown =
            (drawdown.equity_peak - drawdown.current_equity).max(Decimal::ZERO);
        if drawdown.current_drawdown > drawdown.max_drawdown_observed {
            drawdown.max_drawdown_observed = drawdown.current_drawdown;
        }

        match self.config.max_drawdown_limit {
            Some(limit) if limit > Decimal::ZERO && drawdown.current_drawdown >= limit => {
                Some(format!(
                    "Drawdown limit exceeded (drawdown={}, limit={})",
                    drawdown.current_drawdown, limit
                ))
            }
            _ => None,
        }
    }

    pub(super) async fn try_auto_recover_circuit_breaker(&self) {
        if !self.config.circuit_breaker_auto_recover {
            return;
        }
        if *self.state.read().await != PlatformRiskState::Halted {
            return;
        }

        let halted_at = *self.halted_at.read().await;
        let Some(halted_at) = halted_at else {
            self.reset_circuit_breaker_with_reason(
                "auto-recover: missing halted timestamp".to_string(),
            )
            .await;
            return;
        };

        let elapsed_secs = Utc::now()
            .signed_duration_since(halted_at)
            .num_seconds()
            .max(0) as u64;
        if elapsed_secs < self.config.circuit_breaker_cooldown_secs {
            return;
        }

        self.reset_circuit_breaker_with_reason(format!(
            "auto-recover after cooldown ({}s >= {}s)",
            elapsed_secs, self.config.circuit_breaker_cooldown_secs
        ))
        .await;
    }

    async fn reset_circuit_breaker_with_reason(&self, reason: String) {
        info!("Circuit breaker reset: {}", reason);
        *self.state.write().await = PlatformRiskState::Normal;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.halted_at.write().await = None;

        let mut stats_map = self.agent_stats.write().await;
        for stats in stats_map.values_mut() {
            stats.consecutive_failures = 0;
        }
        drop(stats_map);

        self.push_circuit_event(reason, PlatformRiskState::Normal)
            .await;
    }

    async fn push_circuit_event(&self, reason: String, state: PlatformRiskState) {
        let mut events = self.circuit_events.write().await;
        events.push(super::CircuitBreakerEvent {
            timestamp: Utc::now(),
            reason,
            state,
        });
        if events.len() > 100 {
            let drain = events.len() - 100;
            events.drain(0..drain);
        }
    }
}
