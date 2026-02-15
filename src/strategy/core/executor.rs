//! Order execution for all strategies
//!
//! Centralized order execution with retry logic, timeout handling,
//! and execution metrics tracking.

use crate::adapters::PolymarketClient;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::{OrderError, Result};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, error, info, warn};

/// Execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Order timeout in milliseconds
    pub order_timeout_ms: u64,
    /// Polling interval for order status
    pub poll_interval_ms: u64,
    /// Maximum retry attempts
    pub max_retries: u32,
    /// Default slippage tolerance (percentage)
    pub default_slippage: Decimal,
    /// Enable dry run mode
    pub dry_run: bool,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            order_timeout_ms: 5000,
            poll_interval_ms: 200,
            max_retries: 3,
            default_slippage: Decimal::new(2, 2), // 0.02 = 2%
            dry_run: true,
        }
    }
}

/// Execution result with fill details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Order ID from exchange
    pub order_id: String,
    /// Client order ID (strategy-assigned)
    pub client_order_id: Option<String>,
    /// Final order status
    pub status: OrderStatus,
    /// Number of shares filled
    pub filled_shares: u64,
    /// Average fill price
    pub avg_fill_price: Option<Decimal>,
    /// Execution time in milliseconds
    pub elapsed_ms: u64,
    /// Number of retry attempts
    pub attempts: u32,
    /// Timestamp of completion
    pub completed_at: DateTime<Utc>,
    /// Error message if failed
    pub error: Option<String>,
}

impl ExecutionResult {
    /// Check if order was fully filled
    pub fn is_filled(&self) -> bool {
        self.status == OrderStatus::Filled
    }

    /// Check if order was at least partially filled
    pub fn has_fill(&self) -> bool {
        self.filled_shares > 0
    }

    /// Get fill value (shares * price)
    pub fn fill_value(&self) -> Decimal {
        self.avg_fill_price
            .map(|p| p * Decimal::from(self.filled_shares))
            .unwrap_or(Decimal::ZERO)
    }
}

/// Execution metrics for monitoring
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetrics {
    /// Total orders submitted
    pub total_orders: u64,
    /// Successfully filled orders
    pub filled_orders: u64,
    /// Partially filled orders
    pub partial_fills: u64,
    /// Failed orders
    pub failed_orders: u64,
    /// Total retry attempts
    pub total_retries: u64,
    /// Average execution time (ms)
    pub avg_execution_ms: f64,
    /// Total shares traded
    pub total_shares: u64,
    /// Total value traded
    pub total_value: Decimal,
}

/// Order executor for managing order lifecycle
pub struct OrderExecutor {
    client: Arc<PolymarketClient>,
    config: ExecutionConfig,
    /// Execution metrics
    metrics: Arc<RwLock<ExecutionMetrics>>,
    /// Pending orders by client_order_id
    pending: Arc<RwLock<HashMap<String, OrderTracker>>>,
}

/// Tracks a pending order
#[derive(Debug, Clone)]
struct OrderTracker {
    order_id: String,
    client_order_id: String,
    request: OrderRequest,
    submitted_at: DateTime<Utc>,
    strategy_id: String,
}

impl OrderExecutor {
    /// Create a new order executor
    pub fn new(client: Arc<PolymarketClient>, config: ExecutionConfig) -> Self {
        Self {
            client,
            config,
            metrics: Arc::new(RwLock::new(ExecutionMetrics::default())),
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.config.dry_run || self.client.is_dry_run()
    }

    /// Execute an order with retry logic
    pub async fn execute(
        &self,
        request: &OrderRequest,
        client_order_id: Option<String>,
        strategy_id: &str,
    ) -> Result<ExecutionResult> {
        let start = Instant::now();
        let mut attempts = 0;

        loop {
            attempts += 1;

            match self.try_execute(request, client_order_id.clone(), strategy_id).await {
                Ok(mut result) => {
                    result.attempts = attempts;
                    result.elapsed_ms = start.elapsed().as_millis() as u64;

                    // Update metrics
                    self.update_metrics(&result).await;

                    info!(
                        "Order {} executed: {} shares @ {:?} ({}ms, {} attempts)",
                        result.order_id,
                        result.filled_shares,
                        result.avg_fill_price,
                        result.elapsed_ms,
                        attempts
                    );
                    return Ok(result);
                }
                Err(e) => {
                    if attempts >= self.config.max_retries {
                        error!(
                            "Order execution failed after {} attempts: {}",
                            attempts, e
                        );

                        // Update failure metrics
                        let mut metrics = self.metrics.write().await;
                        metrics.total_orders += 1;
                        metrics.failed_orders += 1;
                        metrics.total_retries += (attempts - 1) as u64;

                        return Err(OrderError::MaxRetriesExceeded { attempts }.into());
                    }

                    warn!(
                        "Order attempt {} failed: {}. Retrying...",
                        attempts, e
                    );

                    // Exponential backoff
                    let delay = Duration::from_millis(100 * (1 << attempts));
                    sleep(delay).await;
                }
            }
        }
    }

    /// Single execution attempt
    async fn try_execute(
        &self,
        request: &OrderRequest,
        client_order_id: Option<String>,
        strategy_id: &str,
    ) -> Result<ExecutionResult> {
        let start = Instant::now();

        // If dry run, simulate immediate fill
        if self.is_dry_run() {
            let order_id = format!("dry-{}", uuid::Uuid::new_v4());
            return Ok(ExecutionResult {
                order_id,
                client_order_id,
                status: OrderStatus::Filled,
                filled_shares: request.shares,
                avg_fill_price: Some(request.limit_price),
                elapsed_ms: start.elapsed().as_millis() as u64,
                attempts: 1,
                completed_at: Utc::now(),
                error: None,
            });
        }

        // Submit order
        let order_resp = self.client.submit_order(request).await?;
        let order_id = order_resp.id.clone();

        debug!("Order submitted: {}", order_id);

        // Track pending order
        if let Some(ref cid) = client_order_id {
            let tracker = OrderTracker {
                order_id: order_id.clone(),
                client_order_id: cid.clone(),
                request: request.clone(),
                submitted_at: Utc::now(),
                strategy_id: strategy_id.to_string(),
            };
            self.pending.write().await.insert(cid.clone(), tracker);
        }

        // Wait for fill with timeout
        let timeout_duration = Duration::from_millis(self.config.order_timeout_ms);
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);

        let result = match timeout(timeout_duration, self.wait_for_fill(&order_id, poll_interval)).await {
            Ok(result) => {
                let mut r = result?;
                r.client_order_id = client_order_id.clone();
                r.elapsed_ms = start.elapsed().as_millis() as u64;
                r
            }
            Err(_) => {
                // Timeout - try to cancel and return partial fill
                warn!("Order {} timed out, attempting cancel", order_id);
                let _ = self.client.cancel_order(&order_id).await;

                // Get final state
                let final_order = self.client.get_order(&order_id).await?;
                let (filled, price) = PolymarketClient::calculate_fill(&final_order);

                ExecutionResult {
                    order_id: order_id.clone(),
                    client_order_id,
                    status: if filled > 0 {
                        OrderStatus::PartiallyFilled
                    } else {
                        OrderStatus::Cancelled
                    },
                    filled_shares: filled,
                    avg_fill_price: price,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    attempts: 1,
                    completed_at: Utc::now(),
                    error: Some("Order timed out".to_string()),
                }
            }
        };

        // Remove from pending
        if let Some(ref cid) = result.client_order_id {
            self.pending.write().await.remove(cid);
        }

        Ok(result)
    }

    /// Poll for order fill
    async fn wait_for_fill(
        &self,
        order_id: &str,
        poll_interval: Duration,
    ) -> Result<ExecutionResult> {
        loop {
            let order = self.client.get_order(order_id).await?;
            let status = PolymarketClient::infer_order_status(&order);
            let (filled, price) = PolymarketClient::calculate_fill(&order);

            match status {
                OrderStatus::Filled => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        client_order_id: None,
                        status,
                        filled_shares: filled,
                        avg_fill_price: price,
                        elapsed_ms: 0,
                        attempts: 1,
                        completed_at: Utc::now(),
                        error: None,
                    });
                }
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        client_order_id: None,
                        status,
                        filled_shares: filled,
                        avg_fill_price: price,
                        elapsed_ms: 0,
                        attempts: 1,
                        completed_at: Utc::now(),
                        error: Some(format!("Order {}", status)),
                    });
                }
                _ => {
                    // Still pending, continue polling
                    sleep(poll_interval).await;
                }
            }
        }
    }

    /// Create and execute a buy order
    pub async fn buy(
        &self,
        token_id: &str,
        market_side: Side,
        shares: u64,
        price: Decimal,
        strategy_id: &str,
    ) -> Result<ExecutionResult> {
        let request = OrderRequest::buy_limit(token_id.to_string(), market_side, shares, price);
        let client_id = format!("{}-buy-{}", strategy_id, Utc::now().timestamp_millis());
        self.execute(&request, Some(client_id), strategy_id).await
    }

    /// Create and execute a sell order
    pub async fn sell(
        &self,
        token_id: &str,
        market_side: Side,
        shares: u64,
        price: Decimal,
        strategy_id: &str,
    ) -> Result<ExecutionResult> {
        let request = OrderRequest::sell_limit(token_id.to_string(), market_side, shares, price);
        let client_id = format!("{}-sell-{}", strategy_id, Utc::now().timestamp_millis());
        self.execute(&request, Some(client_id), strategy_id).await
    }

    /// Cancel an order by exchange order ID
    pub async fn cancel(&self, order_id: &str) -> Result<bool> {
        self.client.cancel_order(order_id).await
    }

    /// Cancel an order by client order ID
    pub async fn cancel_by_client_id(&self, client_order_id: &str) -> Result<bool> {
        let pending = self.pending.read().await;
        if let Some(tracker) = pending.get(client_order_id) {
            self.client.cancel_order(&tracker.order_id).await
        } else {
            Ok(false)
        }
    }

    /// Get all pending orders for a strategy
    pub async fn get_pending_for_strategy(&self, strategy_id: &str) -> Vec<String> {
        self.pending
            .read()
            .await
            .values()
            .filter(|t| t.strategy_id == strategy_id)
            .map(|t| t.client_order_id.clone())
            .collect()
    }

    /// Cancel all pending orders for a strategy
    pub async fn cancel_all_for_strategy(&self, strategy_id: &str) -> Vec<String> {
        let pending: Vec<OrderTracker> = self
            .pending
            .read()
            .await
            .values()
            .filter(|t| t.strategy_id == strategy_id)
            .cloned()
            .collect();

        let mut cancelled = Vec::new();
        for tracker in pending {
            if let Ok(true) = self.client.cancel_order(&tracker.order_id).await {
                cancelled.push(tracker.client_order_id);
            }
        }

        // Remove from pending
        let mut pending_map = self.pending.write().await;
        for cid in &cancelled {
            pending_map.remove(cid);
        }

        cancelled
    }

    /// Get current best prices for a token
    pub async fn get_prices(&self, token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
        self.client.get_best_prices(token_id).await
    }

    /// Update execution metrics
    async fn update_metrics(&self, result: &ExecutionResult) {
        let mut metrics = self.metrics.write().await;
        metrics.total_orders += 1;
        metrics.total_retries += (result.attempts - 1) as u64;

        match result.status {
            OrderStatus::Filled => {
                metrics.filled_orders += 1;
                metrics.total_shares += result.filled_shares;
                metrics.total_value += result.fill_value();
            }
            OrderStatus::PartiallyFilled => {
                metrics.partial_fills += 1;
                metrics.total_shares += result.filled_shares;
                metrics.total_value += result.fill_value();
            }
            _ => {
                metrics.failed_orders += 1;
            }
        }

        // Update average execution time
        let total_time = metrics.avg_execution_ms * (metrics.total_orders - 1) as f64
            + result.elapsed_ms as f64;
        metrics.avg_execution_ms = total_time / metrics.total_orders as f64;
    }

    /// Get current metrics
    pub async fn metrics(&self) -> ExecutionMetrics {
        self.metrics.read().await.clone()
    }

    /// Reset metrics
    pub async fn reset_metrics(&self) {
        *self.metrics.write().await = ExecutionMetrics::default();
    }
}

/// Helper for building execution parameters
#[derive(Debug, Clone)]
pub struct ExecutionParams {
    pub shares: u64,
    pub max_price: Decimal,
    pub slippage_tolerance: Decimal,
    pub priority: u8,
}

impl ExecutionParams {
    pub fn new(shares: u64, max_price: Decimal) -> Self {
        Self {
            shares,
            max_price,
            slippage_tolerance: Decimal::ZERO,
            priority: 0,
        }
    }

    pub fn with_slippage(mut self, tolerance: Decimal) -> Self {
        self.slippage_tolerance = tolerance;
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Calculate effective max price including slippage
    pub fn effective_max_price(&self) -> Decimal {
        self.max_price * (Decimal::ONE + self.slippage_tolerance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_execution_params() {
        let params = ExecutionParams::new(100, dec!(0.50))
            .with_slippage(dec!(0.02))
            .with_priority(5);

        // 0.50 * 1.02 = 0.51
        assert_eq!(params.effective_max_price(), dec!(0.51));
        assert_eq!(params.priority, 5);
    }

    #[test]
    fn test_execution_result() {
        let result = ExecutionResult {
            order_id: "test".to_string(),
            client_order_id: Some("client-1".to_string()),
            status: OrderStatus::Filled,
            filled_shares: 100,
            avg_fill_price: Some(dec!(0.50)),
            elapsed_ms: 500,
            attempts: 1,
            completed_at: Utc::now(),
            error: None,
        };

        assert!(result.is_filled());
        assert!(result.has_fill());
        assert_eq!(result.fill_value(), dec!(50)); // 100 * 0.50
    }
}
