//! Event Router - 領域事件路由
//!
//! 將領域事件分發給適當的 Agent 處理。
//! 支持基於領域和市場的訂閱過濾。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::traits::DomainAgent;
use super::types::{Domain, DomainEvent, OrderIntent};
use crate::error::Result;

/// Agent 訂閱配置
#[derive(Debug, Clone)]
pub struct AgentSubscription {
    /// Agent ID
    pub agent_id: String,
    /// 訂閱的領域 (空 = 全部)
    pub domains: HashSet<Domain>,
    /// 訂閱的市場 (空 = 全部)
    pub markets: HashSet<String>,
    /// 是否處理 Tick 事件
    pub receive_ticks: bool,
}

impl AgentSubscription {
    /// 創建訂閱所有事件
    pub fn all(agent_id: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            domains: HashSet::new(),
            markets: HashSet::new(),
            receive_ticks: true,
        }
    }

    /// 創建訂閱特定領域
    pub fn for_domain(agent_id: &str, domain: Domain) -> Self {
        let mut domains = HashSet::new();
        domains.insert(domain);
        Self {
            agent_id: agent_id.to_string(),
            domains,
            markets: HashSet::new(),
            receive_ticks: true,
        }
    }

    /// 添加領域
    pub fn with_domain(mut self, domain: Domain) -> Self {
        self.domains.insert(domain);
        self
    }

    /// 添加市場
    pub fn with_market(mut self, market: &str) -> Self {
        self.markets.insert(market.to_string());
        self
    }

    /// 是否接收 Tick
    pub fn with_ticks(mut self, receive: bool) -> Self {
        self.receive_ticks = receive;
        self
    }

    /// 檢查是否應該接收此事件
    fn should_receive(&self, event: &DomainEvent) -> bool {
        // Tick 事件
        if matches!(event, DomainEvent::Tick(_)) {
            return self.receive_ticks;
        }

        // 領域過濾
        if !self.domains.is_empty() && !self.domains.contains(&event.domain()) {
            return false;
        }

        // 市場過濾 (如果有設定)
        if !self.markets.is_empty() {
            let market = match event {
                DomainEvent::Sports(e) => Some(&e.market_slug),
                DomainEvent::Crypto(e) => e.round_slug.as_ref(),
                DomainEvent::Politics(e) => Some(&e.market_slug),
                DomainEvent::QuoteUpdate(e) => Some(&e.market_slug),
                DomainEvent::OrderUpdate(_) => None, // 訂單更新總是接收
                DomainEvent::Tick(_) => None,
            };

            if let Some(m) = market {
                if !self.markets.contains(m) {
                    return false;
                }
            }
        }

        true
    }
}

/// 路由統計
#[derive(Debug, Default, Clone)]
pub struct RouterStats {
    /// 收到的事件總數
    pub events_received: u64,
    /// 成功分發數
    pub events_dispatched: u64,
    /// 產生的訂單意圖數
    pub intents_generated: u64,
    /// 按領域統計
    pub events_by_domain: HashMap<Domain, u64>,
    /// 按 Agent 統計
    pub events_by_agent: HashMap<String, u64>,
}

/// 事件路由器
///
/// 負責將領域事件分發給已註冊的 Agent。
pub struct EventRouter {
    /// 已註冊的 Agent (agent_id -> Agent)
    agents: Arc<RwLock<HashMap<String, Box<dyn DomainAgent>>>>,
    /// Agent 訂閱配置
    subscriptions: Arc<RwLock<HashMap<String, AgentSubscription>>>,
    /// 統計
    stats: Arc<RwLock<RouterStats>>,
}

impl Default for EventRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventRouter {
    /// 創建新的路由器
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(RouterStats::default())),
        }
    }

    /// 註冊 Agent
    pub async fn register_agent(
        &self,
        agent: Box<dyn DomainAgent>,
        subscription: AgentSubscription,
    ) {
        let agent_id = agent.id().to_string();
        info!(
            "Registering agent {} ({}) for {:?}",
            agent_id,
            agent.name(),
            subscription.domains
        );

        self.agents.write().await.insert(agent_id.clone(), agent);
        self.subscriptions
            .write()
            .await
            .insert(agent_id, subscription);
    }

    /// 取消註冊 Agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        self.agents.write().await.remove(agent_id);
        self.subscriptions.write().await.remove(agent_id);
        info!("Unregistered agent {}", agent_id);
    }

    /// 分發事件到所有匹配的 Agent
    ///
    /// 返回所有 Agent 產生的訂單意圖。
    pub async fn dispatch(&self, event: DomainEvent) -> Result<Vec<OrderIntent>> {
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.events_received += 1;
            *stats.events_by_domain.entry(event.domain()).or_insert(0) += 1;
        }

        let subscriptions = self.subscriptions.read().await.clone();
        let mut all_intents = Vec::new();

        // 找出應該接收此事件的 Agent
        let target_agents: Vec<String> = subscriptions
            .iter()
            .filter(|(_, sub)| sub.should_receive(&event))
            .map(|(id, _)| id.clone())
            .collect();

        debug!(
            "Dispatching {:?} event to {} agents: {:?}",
            event.domain(),
            target_agents.len(),
            target_agents
        );

        // 分發給每個 Agent
        for agent_id in target_agents {
            let mut agents = self.agents.write().await;
            if let Some(agent) = agents.get_mut(&agent_id) {
                // 檢查 Agent 狀態
                if !agent.status().is_active() {
                    debug!("Skipping inactive agent {}", agent_id);
                    continue;
                }

                // 調用 Agent 處理事件
                match agent.on_event(event.clone()).await {
                    Ok(intents) => {
                        let intent_count = intents.len();
                        if intent_count > 0 {
                            debug!("Agent {} generated {} intents", agent_id, intent_count);
                        }

                        // 更新統計
                        {
                            let mut stats = self.stats.write().await;
                            stats.events_dispatched += 1;
                            stats.intents_generated += intent_count as u64;
                            *stats.events_by_agent.entry(agent_id.clone()).or_insert(0) += 1;
                        }

                        all_intents.extend(intents);
                    }
                    Err(e) => {
                        warn!("Agent {} failed to process event: {}", agent_id, e);
                    }
                }
            }
        }

        Ok(all_intents)
    }

    /// 向特定 Agent 發送事件
    pub async fn dispatch_to_agent(
        &self,
        agent_id: &str,
        event: DomainEvent,
    ) -> Result<Vec<OrderIntent>> {
        let mut agents = self.agents.write().await;

        if let Some(agent) = agents.get_mut(agent_id) {
            agent.on_event(event).await
        } else {
            warn!("Agent {} not found for direct dispatch", agent_id);
            Ok(vec![])
        }
    }

    /// 廣播 Tick 事件
    pub async fn broadcast_tick(&self) -> Result<Vec<OrderIntent>> {
        let tick = DomainEvent::Tick(chrono::Utc::now());
        self.dispatch(tick).await
    }

    // ==================== 查詢方法 ====================

    /// 獲取已註冊的 Agent IDs
    pub async fn registered_agents(&self) -> Vec<String> {
        self.agents.read().await.keys().cloned().collect()
    }

    /// 獲取 Agent 數量
    pub async fn agent_count(&self) -> usize {
        self.agents.read().await.len()
    }

    /// 檢查 Agent 是否已註冊
    pub async fn is_registered(&self, agent_id: &str) -> bool {
        self.agents.read().await.contains_key(agent_id)
    }

    /// 獲取路由統計
    pub async fn stats(&self) -> RouterStats {
        self.stats.read().await.clone()
    }

    // ==================== Agent 控制 ====================

    /// 啟動所有 Agent
    pub async fn start_all_agents(&self) -> Result<()> {
        let mut agents = self.agents.write().await;
        for (id, agent) in agents.iter_mut() {
            info!("Starting agent {}", id);
            agent.start().await?;
        }
        Ok(())
    }

    /// 停止所有 Agent
    pub async fn stop_all_agents(&self) -> Result<()> {
        let mut agents = self.agents.write().await;
        for (id, agent) in agents.iter_mut() {
            info!("Stopping agent {}", id);
            agent.stop().await?;
        }
        Ok(())
    }

    /// 暫停特定 Agent
    pub async fn pause_agent(&self, agent_id: &str) {
        if let Some(agent) = self.agents.write().await.get_mut(agent_id) {
            agent.pause();
            info!("Paused agent {}", agent_id);
        }
    }

    /// 恢復特定 Agent
    pub async fn resume_agent(&self, agent_id: &str) {
        if let Some(agent) = self.agents.write().await.get_mut(agent_id) {
            agent.resume();
            info!("Resumed agent {}", agent_id);
        }
    }

    /// 清除所有 Agent
    pub async fn clear(&self) {
        self.agents.write().await.clear();
        self.subscriptions.write().await.clear();
        *self.stats.write().await = RouterStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_filter() {
        // 訂閱所有
        let sub = AgentSubscription::all("agent1");
        let event = DomainEvent::Tick(chrono::Utc::now());
        assert!(sub.should_receive(&event));

        // 訂閱特定領域 (仍然接收 Tick)
        let sub = AgentSubscription::for_domain("agent1", Domain::Crypto);
        assert!(sub.should_receive(&DomainEvent::Tick(chrono::Utc::now())));

        // 禁用 Tick
        let sub = AgentSubscription::all("agent1").with_ticks(false);
        assert!(!sub.should_receive(&DomainEvent::Tick(chrono::Utc::now())));

        // 訂閱特定領域並禁用 Tick
        let sub = AgentSubscription::for_domain("agent1", Domain::Crypto).with_ticks(false);
        assert!(!sub.should_receive(&DomainEvent::Tick(chrono::Utc::now())));
    }
}
