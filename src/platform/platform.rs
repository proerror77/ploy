//! Order Platform - 統一下單平台
//!
//! 整合所有組件提供完整的訂單管理：
//! - 事件路由 → Agent 處理 → 訂單意圖
//! - 風控檢查 → 優先隊列 → 執行
//! - 倉位追蹤 → 執行報告 → Agent 回調

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::config::ExecutionConfig;
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::{PloyError, Result};
use crate::strategy::executor::OrderExecutor;

use super::position::PositionAggregator;
use super::queue::OrderQueue;
use super::risk::{RiskCheckResult, RiskConfig, RiskGate};
use super::router::{AgentSubscription, EventRouter};
use super::traits::{AgentRiskParams, DomainAgent};
use super::types::{DomainEvent, ExecutionReport, OrderIntent};

/// 平台配置
#[derive(Debug, Clone)]
pub struct PlatformConfig {
    /// 訂單隊列大小
    pub queue_size: usize,
    /// 風控配置
    pub risk_config: RiskConfig,
    /// 執行配置
    pub execution_config: ExecutionConfig,
    /// 隊列處理間隔 (毫秒)
    pub process_interval_ms: u64,
    /// 過期清理間隔 (秒)
    pub cleanup_interval_secs: u64,
    /// 是否啟用並行執行
    pub parallel_execution: bool,
    /// 最大並行訂單數
    pub max_parallel_orders: usize,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            queue_size: 1000,
            risk_config: RiskConfig::default(),
            execution_config: ExecutionConfig::default(),
            process_interval_ms: 100,
            cleanup_interval_secs: 60,
            parallel_execution: false,
            max_parallel_orders: 5,
        }
    }
}

/// 平台統計
#[derive(Debug, Clone, Default)]
pub struct PlatformStats {
    /// 處理的訂單意圖數
    pub intents_processed: u64,
    /// 通過風控的訂單數
    pub risk_passed: u64,
    /// 被風控攔截的訂單數
    pub risk_blocked: u64,
    /// 被調整的訂單數
    pub risk_adjusted: u64,
    /// 執行成功數
    pub executions_success: u64,
    /// 執行失敗數
    pub executions_failed: u64,
    /// 事件處理數
    pub events_processed: u64,
}

/// 下單平台主結構
///
/// 統一管理所有策略 Agent 的訂單執行。
pub struct OrderPlatform {
    /// 事件路由器
    router: Arc<EventRouter>,
    /// 風控閘門
    risk_gate: Arc<RiskGate>,
    /// 訂單隊列
    queue: Arc<RwLock<OrderQueue>>,
    /// 倉位聚合器
    positions: Arc<PositionAggregator>,
    /// 訂單執行器
    executor: Arc<OrderExecutor>,
    /// 配置
    config: PlatformConfig,
    /// 統計
    stats: Arc<RwLock<PlatformStats>>,
    /// 是否運行中
    running: Arc<RwLock<bool>>,
}

impl OrderPlatform {
    /// 創建新的下單平台
    pub fn new(client: PolymarketClient, config: PlatformConfig) -> Self {
        let executor = Arc::new(OrderExecutor::new(client, config.execution_config.clone()));

        Self {
            router: Arc::new(EventRouter::new()),
            risk_gate: Arc::new(RiskGate::new(config.risk_config.clone())),
            queue: Arc::new(RwLock::new(OrderQueue::new(config.queue_size))),
            positions: Arc::new(PositionAggregator::new()),
            executor,
            config,
            stats: Arc::new(RwLock::new(PlatformStats::default())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// 使用自定義執行器創建
    pub fn with_executor(executor: Arc<OrderExecutor>, config: PlatformConfig) -> Self {
        Self {
            router: Arc::new(EventRouter::new()),
            risk_gate: Arc::new(RiskGate::new(config.risk_config.clone())),
            queue: Arc::new(RwLock::new(OrderQueue::new(config.queue_size))),
            positions: Arc::new(PositionAggregator::new()),
            executor,
            config,
            stats: Arc::new(RwLock::new(PlatformStats::default())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    // ==================== Agent 管理 ====================

    /// 註冊 Agent
    pub async fn register_agent(
        &self,
        agent: Box<dyn DomainAgent>,
        subscription: AgentSubscription,
    ) {
        let agent_id = agent.id().to_string();
        let risk_params = agent.risk_params().clone();

        // 註冊到路由器
        self.router.register_agent(agent, subscription).await;

        // 註冊風控參數
        self.risk_gate.register_agent(&agent_id, risk_params).await;

        info!("Platform registered agent: {}", agent_id);
    }

    /// 註冊 Agent (使用自定義風控參數)
    pub async fn register_agent_with_risk(
        &self,
        agent: Box<dyn DomainAgent>,
        subscription: AgentSubscription,
        risk_params: AgentRiskParams,
    ) {
        let agent_id = agent.id().to_string();

        self.router.register_agent(agent, subscription).await;
        self.risk_gate.register_agent(&agent_id, risk_params).await;

        info!("Platform registered agent with custom risk: {}", agent_id);
    }

    /// 取消註冊 Agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        self.router.unregister_agent(agent_id).await;
        self.risk_gate.unregister_agent(agent_id).await;
        self.positions.clear_agent(agent_id).await;

        info!("Platform unregistered agent: {}", agent_id);
    }

    // ==================== 事件處理 ====================

    /// 處理領域事件
    ///
    /// 1. 路由事件到相關 Agent
    /// 2. 收集訂單意圖
    /// 3. 加入優先隊列
    pub async fn process_event(&self, event: DomainEvent) -> Result<usize> {
        // 路由事件並收集意圖
        let intents = self.router.dispatch(event).await?;
        let intent_count = intents.len();

        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.events_processed += 1;
        }

        // 加入隊列
        let mut queue = self.queue.write().await;
        for intent in intents {
            if let Err(e) = queue.enqueue(intent.clone()) {
                warn!("Failed to enqueue intent {}: {}", intent.intent_id, e);
            }
        }

        Ok(intent_count)
    }

    /// 批量處理事件
    pub async fn process_events(&self, events: Vec<DomainEvent>) -> Result<usize> {
        let mut total_intents = 0;
        for event in events {
            total_intents += self.process_event(event).await?;
        }
        Ok(total_intents)
    }

    /// 直接入隊訂單意圖 (用於外部 Agent 提交)
    ///
    /// 允許外部 Agent (如 RLCryptoAgent) 直接提交訂單意圖到隊列
    pub async fn enqueue_intent(&self, intent: OrderIntent) -> Result<()> {
        let mut queue = self.queue.write().await;
        queue.enqueue(intent.clone()).map_err(|e| {
            PloyError::Internal(format!(
                "Failed to enqueue intent {}: {}",
                intent.intent_id, e
            ))
        })?;

        // 更新統計
        let mut stats = self.stats.write().await;
        stats.intents_processed += 1;

        Ok(())
    }

    /// 批量入隊訂單意圖
    pub async fn enqueue_intents(&self, intents: Vec<OrderIntent>) -> Result<usize> {
        let mut queued = 0;
        for intent in intents {
            if let Err(e) = self.enqueue_intent(intent).await {
                warn!("Failed to enqueue intent: {}", e);
            } else {
                queued += 1;
            }
        }
        Ok(queued)
    }

    // ==================== 訂單處理 ====================

    /// 處理隊列中的訂單
    ///
    /// 從優先隊列取出訂單，進行風控檢查，然後執行。
    pub async fn process_queue(&self) -> Result<usize> {
        let mut processed = 0;

        // 批量取出訂單
        let batch_size = if self.config.parallel_execution {
            self.config.max_parallel_orders
        } else {
            1
        };

        let intents = self.queue.write().await.dequeue_batch(batch_size);

        for intent in intents {
            if let Err(e) = self.process_intent(intent).await {
                warn!("Failed to process intent: {}", e);
            }
            processed += 1;
        }

        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.intents_processed += processed as u64;
        }

        Ok(processed)
    }

    /// 處理單個訂單意圖
    async fn process_intent(&self, intent: OrderIntent) -> Result<()> {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        debug!("Processing intent {} from agent {}", intent_id, agent_id);

        // 風控檢查
        let risk_result = self.risk_gate.check_order(&intent).await;

        match risk_result {
            RiskCheckResult::Passed => {
                // 更新統計
                self.stats.write().await.risk_passed += 1;

                // 執行訂單
                self.execute_intent(&intent).await?;
            }
            RiskCheckResult::Blocked(reason) => {
                // 更新統計
                self.stats.write().await.risk_blocked += 1;

                // 發送被攔截報告
                let report = ExecutionReport::risk_blocked(&intent, reason.to_string());
                self.send_execution_report(&report).await;

                warn!("Intent {} blocked: {}", intent_id, reason);
            }
            RiskCheckResult::Adjusted(suggestion) => {
                // 更新統計
                self.stats.write().await.risk_adjusted += 1;

                // 調整後執行
                let mut adjusted_intent = intent.clone();
                adjusted_intent.shares = suggestion.max_shares;

                info!(
                    "Intent {} adjusted: {} -> {} shares ({})",
                    intent_id, intent.shares, suggestion.max_shares, suggestion.reason
                );

                self.execute_intent(&adjusted_intent).await?;
            }
        }

        Ok(())
    }

    /// 執行訂單
    async fn execute_intent(&self, intent: &OrderIntent) -> Result<()> {
        let agent_id = &intent.agent_id;
        let intent_id = intent.intent_id;

        // 構建訂單請求
        let request = if intent.is_buy {
            OrderRequest::buy_limit(
                intent.token_id.clone(),
                intent.side,
                intent.shares,
                intent.limit_price,
            )
        } else {
            OrderRequest::sell_limit(
                intent.token_id.clone(),
                intent.side,
                intent.shares,
                intent.limit_price,
            )
        };

        // 執行訂單
        match self.executor.execute(&request).await {
            Ok(result) => {
                // 檢查是否成交
                let is_filled = matches!(
                    result.status,
                    OrderStatus::Filled | OrderStatus::PartiallyFilled
                );
                let has_fill = result.filled_shares > 0;

                // 創建執行報告
                let report = if is_filled || has_fill {
                    // 更新倉位
                    if intent.is_buy && has_fill {
                        self.positions
                            .open_position(
                                agent_id,
                                intent.domain,
                                &intent.market_slug,
                                &intent.token_id,
                                intent.side,
                                result.filled_shares,
                                result.avg_fill_price.unwrap_or(intent.limit_price),
                            )
                            .await;
                    }

                    // 更新風控統計
                    self.risk_gate
                        .record_success(agent_id, rust_decimal::Decimal::ZERO)
                        .await;

                    // 更新平台統計
                    self.stats.write().await.executions_success += 1;

                    ExecutionReport::success(
                        intent,
                        result.order_id,
                        result.filled_shares,
                        result.avg_fill_price.unwrap_or(intent.limit_price),
                    )
                } else {
                    // 執行失敗
                    let reason = format!("Order status: {:?}", result.status);
                    self.risk_gate.record_failure(agent_id, &reason).await;
                    self.stats.write().await.executions_failed += 1;

                    ExecutionReport::rejected(intent, reason)
                };

                // 發送報告給 Agent
                self.send_execution_report(&report).await;

                info!(
                    "Intent {} executed: {} shares filled",
                    intent_id, report.filled_shares
                );
            }
            Err(e) => {
                // 執行錯誤
                self.risk_gate
                    .record_failure(agent_id, &e.to_string())
                    .await;
                self.stats.write().await.executions_failed += 1;

                let report = ExecutionReport::rejected(intent, e.to_string());
                self.send_execution_report(&report).await;

                error!("Intent {} failed: {}", intent_id, e);
            }
        }

        Ok(())
    }

    /// 發送執行報告給 Agent
    async fn send_execution_report(&self, report: &ExecutionReport) {
        // 通過路由器發送給對應的 Agent
        if let Err(e) = self
            .router
            .dispatch_to_agent(
                &report.agent_id,
                DomainEvent::OrderUpdate(super::types::OrderUpdateEvent {
                    domain: super::types::Domain::Crypto, // TODO: 從 report 獲取
                    order_id: report.order_id.clone().unwrap_or_default(),
                    client_order_id: report.intent_id.to_string(),
                    status: format!("{:?}", report.status),
                    filled_shares: report.filled_shares,
                    avg_price: report.avg_fill_price,
                    timestamp: report.executed_at,
                }),
            )
            .await
        {
            warn!(
                "Failed to send execution report to agent {}: {}",
                report.agent_id, e
            );
        }
    }

    // ==================== 運行控制 ====================

    /// 啟動平台
    pub async fn start(&self) -> Result<()> {
        *self.running.write().await = true;

        // 啟動所有 Agent
        self.router.start_all_agents().await?;

        info!("Order platform started");
        Ok(())
    }

    /// 停止平台
    pub async fn stop(&self) -> Result<()> {
        *self.running.write().await = false;

        // 停止所有 Agent
        self.router.stop_all_agents().await?;

        info!("Order platform stopped");
        Ok(())
    }

    /// 運行主循環
    ///
    /// 定期處理隊列和清理過期訂單。
    pub async fn run_loop(&self) {
        let mut process_interval = interval(Duration::from_millis(self.config.process_interval_ms));
        let mut cleanup_interval = interval(Duration::from_secs(self.config.cleanup_interval_secs));

        loop {
            tokio::select! {
                _ = process_interval.tick() => {
                    if !*self.running.read().await {
                        break;
                    }

                    if let Err(e) = self.process_queue().await {
                        error!("Queue processing error: {}", e);
                    }
                }
                _ = cleanup_interval.tick() => {
                    self.cleanup().await;
                }
            }
        }

        info!("Platform run loop exited");
    }

    /// 清理過期訂單和倉位
    async fn cleanup(&self) {
        let expired_orders = self.queue.write().await.cleanup_expired();
        let expired_positions = self.positions.cleanup_expired().await;

        if expired_orders > 0 || expired_positions > 0 {
            debug!(
                "Cleanup: {} expired orders, {} expired positions",
                expired_orders, expired_positions
            );
        }
    }

    // ==================== 查詢方法 ====================

    /// 獲取隊列長度
    pub async fn queue_len(&self) -> usize {
        self.queue.read().await.len()
    }

    /// 獲取隊列統計
    pub async fn queue_stats(&self) -> super::queue::QueueStats {
        self.queue.read().await.stats()
    }

    /// 獲取風控狀態
    pub async fn risk_state(&self) -> super::risk::PlatformRiskState {
        self.risk_gate.state().await
    }

    /// 獲取平台統計
    pub async fn stats(&self) -> PlatformStats {
        self.stats.read().await.clone()
    }

    /// 獲取聚合倉位
    pub async fn aggregated_positions(&self) -> super::position::AggregatedPosition {
        self.positions.aggregate().await
    }

    /// 獲取 Agent 倉位
    pub async fn agent_positions(&self, agent_id: &str) -> Vec<super::position::Position> {
        self.positions.get_agent_positions(agent_id).await
    }

    /// 獲取路由統計
    pub async fn router_stats(&self) -> super::router::RouterStats {
        self.router.stats().await
    }

    /// 是否運行中
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// 是否可以交易
    pub async fn can_trade(&self) -> bool {
        self.risk_gate.can_trade().await
    }

    /// 重置熔斷
    pub async fn reset_circuit_breaker(&self) {
        self.risk_gate.reset_circuit_breaker().await;
    }

    // ==================== 組件訪問 ====================

    /// 獲取路由器引用
    pub fn router(&self) -> &Arc<EventRouter> {
        &self.router
    }

    /// 獲取風控閘門引用
    pub fn risk_gate(&self) -> &Arc<RiskGate> {
        &self.risk_gate
    }

    /// 獲取倉位聚合器引用
    pub fn positions(&self) -> &Arc<PositionAggregator> {
        &self.positions
    }

    /// 獲取執行器引用
    pub fn executor(&self) -> &Arc<OrderExecutor> {
        &self.executor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 需要 mock PolymarketClient 進行測試
    // 這裡只測試基本的結構和配置

    #[test]
    fn test_platform_config_default() {
        let config = PlatformConfig::default();
        assert_eq!(config.queue_size, 1000);
        assert_eq!(config.process_interval_ms, 100);
        assert!(!config.parallel_execution);
    }

    #[test]
    fn test_platform_stats_default() {
        let stats = PlatformStats::default();
        assert_eq!(stats.intents_processed, 0);
        assert_eq!(stats.executions_success, 0);
    }
}
