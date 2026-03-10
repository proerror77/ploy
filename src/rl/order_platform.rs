//! RL compatibility execution runtime.
//!
//! This preserves the old queue/risk/execution loop only for RL CLI flows. Canonical
//! live trading continues to run exclusively through the coordinator.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::config::ExecutionConfig;
use crate::coordinator::{AggregatedPosition, OrderQueue, Position, PositionAggregator, QueueStats};
use crate::coordinator::{PlatformRiskState, RiskCheckResult, RiskConfig, RiskGate};
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::{PloyError, Result};
use crate::exchange::ExchangeClient;
use crate::coordinator::OrderIntent;
use crate::rl::ExecutionReport;
use crate::strategy::executor::OrderExecutor;

/// Legacy RL CLI order-runtime config.
#[derive(Debug, Clone)]
pub struct RlOrderRuntimeConfig {
    pub queue_size: usize,
    pub risk_config: RiskConfig,
    pub execution_config: ExecutionConfig,
    pub process_interval_ms: u64,
    pub cleanup_interval_secs: u64,
    pub parallel_execution: bool,
    pub max_parallel_orders: usize,
}

impl Default for RlOrderRuntimeConfig {
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

#[derive(Debug, Clone, Default)]
pub struct RlRuntimeStats {
    pub intents_processed: u64,
    pub risk_passed: u64,
    pub risk_blocked: u64,
    pub risk_adjusted: u64,
    pub executions_success: u64,
    pub executions_failed: u64,
    pub events_processed: u64,
}

/// Legacy queue-driven execution surface retained only for the RL CLI path.
pub struct RlOrderRuntime {
    risk_gate: Arc<RiskGate>,
    queue: Arc<RwLock<OrderQueue>>,
    positions: Arc<PositionAggregator>,
    executor: Arc<OrderExecutor>,
    config: RlOrderRuntimeConfig,
    stats: Arc<RwLock<RlRuntimeStats>>,
    running: Arc<RwLock<bool>>,
}

impl RlOrderRuntime {
    fn enforce_coordinator_only_live(&self) -> Result<()> {
        if self.executor.is_dry_run() {
            return Ok(());
        }
        Err(PloyError::Validation(
            "legacy RL order runtime live execution is disabled; use coordinator runtime (`ploy platform start`)".to_string(),
        ))
    }

    pub fn new(client: PolymarketClient, config: RlOrderRuntimeConfig) -> Self {
        Self::new_with_exchange(Arc::new(client), config)
    }

    pub fn new_with_exchange(
        client: Arc<dyn ExchangeClient>,
        config: RlOrderRuntimeConfig,
    ) -> Self {
        let executor = Arc::new(OrderExecutor::new_with_exchange(
            client,
            config.execution_config.clone(),
        ));

        Self {
            risk_gate: Arc::new(RiskGate::new(config.risk_config.clone())),
            queue: Arc::new(RwLock::new(OrderQueue::new(config.queue_size))),
            positions: Arc::new(PositionAggregator::new()),
            executor,
            config,
            stats: Arc::new(RwLock::new(RlRuntimeStats::default())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    pub fn with_executor(executor: Arc<OrderExecutor>, config: RlOrderRuntimeConfig) -> Self {
        Self {
            risk_gate: Arc::new(RiskGate::new(config.risk_config.clone())),
            queue: Arc::new(RwLock::new(OrderQueue::new(config.queue_size))),
            positions: Arc::new(PositionAggregator::new()),
            executor,
            config,
            stats: Arc::new(RwLock::new(RlRuntimeStats::default())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn enqueue_intent(&self, intent: OrderIntent) -> Result<()> {
        let mut queue = self.queue.write().await;
        queue.enqueue(intent.clone()).map_err(|e| {
            PloyError::Internal(format!(
                "Failed to enqueue intent {}: {}",
                intent.intent_id, e
            ))
        })?;

        let mut stats = self.stats.write().await;
        stats.intents_processed += 1;

        Ok(())
    }

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

    pub async fn process_queue(&self) -> Result<Vec<ExecutionReport>> {
        self.enforce_coordinator_only_live()?;

        let batch_size = if self.config.parallel_execution {
            self.config.max_parallel_orders
        } else {
            1
        };

        let intents = self.queue.write().await.dequeue_batch(batch_size);
        let mut reports = Vec::with_capacity(intents.len());

        for intent in intents {
            match self.process_intent(intent).await {
                Ok(report) => reports.push(report),
                Err(e) => warn!("Failed to process intent: {}", e),
            }
        }

        {
            let mut stats = self.stats.write().await;
            stats.intents_processed += reports.len() as u64;
        }

        Ok(reports)
    }

    async fn process_intent(&self, intent: OrderIntent) -> Result<ExecutionReport> {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        debug!("Processing intent {} from agent {}", intent_id, agent_id);

        let risk_result = self.risk_gate.check_order(&intent).await;

        match risk_result {
            RiskCheckResult::Passed => {
                self.stats.write().await.risk_passed += 1;
                self.execute_intent(&intent).await
            }
            RiskCheckResult::Blocked(reason) => {
                self.stats.write().await.risk_blocked += 1;
                let report = ExecutionReport::risk_blocked(&intent, reason.to_string());
                warn!("Intent {} blocked: {}", intent_id, reason);
                Ok(report)
            }
            RiskCheckResult::Adjusted(suggestion) => {
                self.stats.write().await.risk_adjusted += 1;

                let mut adjusted_intent = intent.clone();
                adjusted_intent.shares = suggestion.max_shares;

                info!(
                    "Intent {} adjusted: {} -> {} shares ({})",
                    intent_id, intent.shares, suggestion.max_shares, suggestion.reason
                );

                self.execute_intent(&adjusted_intent).await
            }
        }
    }

    async fn execute_intent(&self, intent: &OrderIntent) -> Result<ExecutionReport> {
        let agent_id = &intent.agent_id;
        let intent_id = intent.intent_id;

        let mut request = if intent.is_buy {
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
        request.client_order_id = format!("intent:{}", intent.intent_id);
        request.idempotency_key = Some(format!("intent:{}", intent.intent_id));

        match self.executor.execute(&request).await {
            Ok(result) => {
                let is_filled = matches!(
                    result.status,
                    OrderStatus::Filled | OrderStatus::PartiallyFilled
                );
                let has_fill = result.filled_shares > 0;

                let report = if is_filled || has_fill {
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

                    self.risk_gate
                        .record_success(agent_id, rust_decimal::Decimal::ZERO)
                        .await;

                    self.stats.write().await.executions_success += 1;

                    ExecutionReport::success(
                        intent,
                        result.order_id,
                        result.filled_shares,
                        result.avg_fill_price.unwrap_or(intent.limit_price),
                    )
                } else {
                    let reason = format!("Order status: {:?}", result.status);
                    self.risk_gate.record_failure(agent_id, &reason).await;
                    self.stats.write().await.executions_failed += 1;
                    ExecutionReport::rejected(intent, reason)
                };

                info!(
                    "Intent {} executed: {} shares filled",
                    intent_id, report.filled_shares
                );
                Ok(report)
            }
            Err(e) => {
                self.risk_gate
                    .record_failure(agent_id, &e.to_string())
                    .await;
                self.stats.write().await.executions_failed += 1;

                error!("Intent {} failed: {}", intent_id, e);
                Ok(ExecutionReport::rejected(intent, e.to_string()))
            }
        }
    }

    pub async fn start(&self) -> Result<()> {
        self.enforce_coordinator_only_live()?;
        *self.running.write().await = true;
        info!("RL order runtime started");
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        *self.running.write().await = false;
        info!("RL order runtime stopped");
        Ok(())
    }

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

        info!("RL order runtime loop exited");
    }

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

    pub async fn queue_len(&self) -> usize {
        self.queue.read().await.len()
    }

    pub async fn queue_stats(&self) -> QueueStats {
        self.queue.read().await.stats()
    }

    pub async fn risk_state(&self) -> PlatformRiskState {
        self.risk_gate.state().await
    }

    pub async fn stats(&self) -> RlRuntimeStats {
        self.stats.read().await.clone()
    }

    pub async fn aggregated_positions(&self) -> AggregatedPosition {
        self.positions.aggregate().await
    }

    pub async fn agent_positions(&self, agent_id: &str) -> Vec<Position> {
        self.positions.get_agent_positions(agent_id).await
    }

    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    pub async fn can_trade(&self) -> bool {
        self.risk_gate.can_trade().await
    }

    pub async fn reset_circuit_breaker(&self) {
        self.risk_gate.reset_circuit_breaker().await;
    }

    pub fn risk_gate(&self) -> &Arc<RiskGate> {
        &self.risk_gate
    }

    pub fn positions(&self) -> &Arc<PositionAggregator> {
        &self.positions
    }

    pub fn executor(&self) -> &Arc<OrderExecutor> {
        &self.executor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::PolymarketClient;

    #[test]
    fn test_rl_order_runtime_config_default() {
        let config = RlOrderRuntimeConfig::default();
        assert_eq!(config.queue_size, 1000);
        assert_eq!(config.process_interval_ms, 100);
        assert!(!config.parallel_execution);
    }

    #[test]
    fn test_rl_runtime_stats_default() {
        let stats = RlRuntimeStats::default();
        assert_eq!(stats.intents_processed, 0);
        assert_eq!(stats.executions_success, 0);
    }

    fn build_runtime(dry_run: bool) -> RlOrderRuntime {
        let client = PolymarketClient::new("https://clob.polymarket.com", dry_run)
            .expect("build polymarket client");
        RlOrderRuntime::new(client, RlOrderRuntimeConfig::default())
    }

    #[tokio::test]
    async fn test_rl_order_runtime_start_allows_dry_run() {
        let runtime = build_runtime(true);
        assert!(runtime.start().await.is_ok());
        assert!(runtime.is_running().await);
        assert!(runtime.stop().await.is_ok());
    }

    #[tokio::test]
    async fn test_rl_order_runtime_start_blocks_live_runtime() {
        let runtime = build_runtime(false);
        let err = runtime.start().await.expect_err("live start must fail");
        assert!(err
            .to_string()
            .contains("legacy RL order runtime live execution is disabled"));
        assert!(!runtime.is_running().await);
    }
}
