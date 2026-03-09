use chrono::Utc;
use rust_decimal::Decimal;
use tracing::debug;

use super::{Domain, RiskGate};
use crate::agent_runtime::AgentRiskParams;

impl RiskGate {
    /// 註冊 Agent 的風控參數
    pub async fn register_agent(&self, agent_id: &str, params: AgentRiskParams) {
        let mut params_map = self.agent_params.write().await;
        params_map.insert(agent_id.to_string(), params);
        debug!("Registered risk params for agent {}", agent_id);
    }

    /// 註冊 Agent 的風控參數 (含 domain)
    pub async fn register_agent_with_domain(
        &self,
        agent_id: &str,
        domain: Domain,
        params: AgentRiskParams,
    ) {
        self.register_agent(agent_id, params).await;
        self.agent_domains
            .write()
            .await
            .insert(agent_id.to_string(), domain);
    }

    /// 取消註冊 Agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        let removed_domain = self.agent_domains.write().await.remove(agent_id);
        if let Some(domain) = removed_domain {
            let old_exposure = self.agent_exposure(agent_id).await;
            self.apply_domain_exposure_change(domain, old_exposure, Decimal::ZERO)
                .await;
        }

        self.agent_params.write().await.remove(agent_id);
        self.agent_stats.write().await.remove(agent_id);
        debug!("Unregistered agent {}", agent_id);
    }

    /// 更新 Agent 暴露
    pub async fn update_agent_exposure(
        &self,
        agent_id: &str,
        exposure: Decimal,
        unrealized_pnl: Decimal,
        position_count: usize,
        unhedged_count: u32,
    ) {
        let domain = self.agent_domains.read().await.get(agent_id).copied();

        let old_exposure = {
            let mut stats_map = self.agent_stats.write().await;
            let stats = stats_map.entry(agent_id.to_string()).or_default();
            let old_exposure = stats.exposure;

            stats.exposure = exposure;
            stats.unrealized_pnl = unrealized_pnl;
            stats.position_count = position_count;
            stats.unhedged_count = unhedged_count;
            stats.last_update = Some(Utc::now());

            old_exposure
        };

        let mut total = self.total_exposure.write().await;
        *total = *total - old_exposure + exposure;
        drop(total);

        if let Some(domain) = domain {
            self.apply_domain_exposure_change(domain, old_exposure, exposure)
                .await;
        }
    }

    async fn agent_exposure(&self, agent_id: &str) -> Decimal {
        self.agent_stats
            .read()
            .await
            .get(agent_id)
            .map(|stats| stats.exposure)
            .unwrap_or(Decimal::ZERO)
    }

    async fn apply_domain_exposure_change(
        &self,
        domain: Domain,
        previous_exposure: Decimal,
        next_exposure: Decimal,
    ) {
        if previous_exposure == Decimal::ZERO && next_exposure == Decimal::ZERO {
            return;
        }

        let mut domain_map = self.domain_exposure.write().await;
        let current = domain_map.entry(domain).or_insert(Decimal::ZERO);
        *current = (*current - previous_exposure + next_exposure).max(Decimal::ZERO);
        if *current == Decimal::ZERO {
            domain_map.remove(&domain);
        }
    }
}
