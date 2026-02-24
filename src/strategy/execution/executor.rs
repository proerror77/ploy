use crate::adapters::{FeishuNotifier, PolymarketClient};
use crate::config::ExecutionConfig;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::{OrderError, Result};
use crate::exchange::ExchangeClient;
use super::idempotency::{IdempotencyManager, IdempotencyRecord, IdempotencyResult};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, error, info, warn};

/// Order executor for managing order lifecycle
pub struct OrderExecutor {
    client: Arc<dyn ExchangeClient>,
    config: ExecutionConfig,
    feishu: Option<Arc<FeishuNotifier>>,
    idempotency: Option<Arc<IdempotencyManager>>,
}

/// Execution result with fill details
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionResult {
    pub order_id: String,
    pub status: OrderStatus,
    pub filled_shares: u64,
    pub avg_fill_price: Option<Decimal>,
    pub elapsed_ms: u64,
}

impl OrderExecutor {
    /// Create a new order executor
    pub fn new(client: PolymarketClient, config: ExecutionConfig) -> Self {
        Self::new_with_exchange(Arc::new(client), config)
    }

    /// Create a new order executor from any exchange implementation.
    pub fn new_with_exchange(client: Arc<dyn ExchangeClient>, config: ExecutionConfig) -> Self {
        Self {
            client,
            config,
            feishu: FeishuNotifier::from_env(),
            idempotency: None,
        }
    }

    /// Set the Feishu notifier
    pub fn with_feishu(mut self, feishu: Option<Arc<FeishuNotifier>>) -> Self {
        self.feishu = feishu;
        self
    }

    /// Set the idempotency manager
    pub fn with_idempotency(mut self, idempotency: Arc<IdempotencyManager>) -> Self {
        self.idempotency = Some(idempotency);
        self
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.client.is_dry_run()
    }

    /// Execute an order with retry logic and idempotency protection
    pub async fn execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        // Check for duplicate order if idempotency is enabled
        if let Some(ref idempotency) = self.idempotency {
            let idem_key = IdempotencyManager::generate_key(request);

            match idempotency.check_or_create(&idem_key, request).await? {
                IdempotencyResult::Duplicate {
                    order_id,
                    status,
                    response_data,
                    error_message,
                } => {
                    warn!(
                        "Duplicate order detected (key: {}), status: {}",
                        idem_key, status
                    );

                    let mut record = IdempotencyRecord {
                        order_id,
                        status,
                        response_data,
                        error_message,
                    };

                    match record.status.to_lowercase().as_str() {
                        "completed" => {
                            return Self::cached_result(record, request);
                        }
                        "failed" => {
                            let msg = record
                                .error_message
                                .unwrap_or_else(|| "Previous attempt failed".to_string());
                            return Err(crate::error::PloyError::Internal(format!(
                                "Order submission failed: {}",
                                msg
                            )));
                        }
                        _ => {
                            warn!(
                                "Previous order attempt still pending, polling idempotency status..."
                            );

                            let poll_interval =
                                Duration::from_millis(self.config.poll_interval_ms.max(100));
                            let timeout_ms = self
                                .config
                                .confirm_fill_timeout_ms
                                .max(poll_interval.as_millis() as u64);
                            let start = Instant::now();

                            loop {
                                if start.elapsed() >= Duration::from_millis(timeout_ms) {
                                    return Err(crate::error::PloyError::OrderSubmission(
                                        "Order already pending; retry later".to_string(),
                                    ));
                                }

                                sleep(poll_interval).await;
                                record = idempotency.fetch_record(&idem_key).await?;

                                match record.status.to_lowercase().as_str() {
                                    "completed" => {
                                        return Self::cached_result(record, request);
                                    }
                                    "failed" => {
                                        let msg = record.error_message.unwrap_or_else(|| {
                                            "Previous attempt failed".to_string()
                                        });
                                        return Err(crate::error::PloyError::Internal(format!(
                                            "Order submission failed: {}",
                                            msg
                                        )));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                IdempotencyResult::New => {
                    // Continue with new order execution
                    debug!("New order request (key: {})", idem_key);
                }
            }

            // Execute the order
            let result = self.execute_with_retry(request).await;

            // Mark idempotency status
            match &result {
                Ok(exec_result) => {
                    if let Err(e) = idempotency
                        .mark_completed(&idem_key, &exec_result.order_id, exec_result)
                        .await
                    {
                        warn!("Failed to mark idempotency as completed: {}", e);
                    }
                }
                Err(e) => {
                    if let Err(err) = idempotency.mark_failed(&idem_key, &e.to_string()).await {
                        warn!("Failed to mark idempotency as failed: {}", err);
                    }
                }
            }

            result
        } else {
            // No idempotency protection, execute directly
            self.execute_with_retry(request).await
        }
    }

    fn cached_result(record: IdempotencyRecord, request: &OrderRequest) -> Result<ExecutionResult> {
        if let Some(data) = record.response_data {
            if let Ok(result) = serde_json::from_value::<ExecutionResult>(data) {
                info!("Returning cached order result: {}", result.order_id);
                return Ok(result);
            }
        }

        if let Some(order_id) = record.order_id {
            return Ok(ExecutionResult {
                order_id,
                status: OrderStatus::Submitted,
                filled_shares: 0,
                avg_fill_price: Some(request.limit_price),
                elapsed_ms: 0,
            });
        }

        Err(crate::error::PloyError::Internal(
            "Idempotency record completed without order_id".to_string(),
        ))
    }

    /// Execute order with retry logic (internal method)
    async fn execute_with_retry(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        let mut attempts = 0;

        loop {
            attempts += 1;

            match self.try_execute(request).await {
                Ok(result) => {
                    info!(
                        "Order {} executed: {} shares @ {:?} ({}ms)",
                        result.order_id,
                        result.filled_shares,
                        result.avg_fill_price,
                        result.elapsed_ms
                    );

                    // Send Feishu notification
                    if let Some(ref feishu) = self.feishu {
                        let action = match request.order_side {
                            crate::domain::OrderSide::Buy => "BUY",
                            crate::domain::OrderSide::Sell => "SELL",
                        };
                        let side = match request.market_side {
                            Side::Up => "UP",
                            Side::Down => "DOWN",
                        };
                        let price = result
                            .avg_fill_price
                            .map(|p| p.to_f64().unwrap_or(0.0))
                            .unwrap_or(request.limit_price.to_f64().unwrap_or(0.0));

                        // Use request.shares since filled_shares may be 0 for submitted orders
                        let shares = if result.filled_shares > 0 {
                            result.filled_shares
                        } else {
                            request.shares
                        };
                        feishu
                            .notify_trade(
                                action,
                                &request.token_id[..16.min(request.token_id.len())],
                                side,
                                price,
                                shares as f64,
                                Some(&result.order_id),
                            )
                            .await;
                    }

                    return Ok(result);
                }
                Err(e) => {
                    if attempts >= self.config.max_retries {
                        error!("Order execution failed after {} attempts: {}", attempts, e);
                        return Err(OrderError::MaxRetriesExceeded { attempts }.into());
                    }

                    warn!("Order attempt {} failed: {}. Retrying...", attempts, e);

                    // Exponential backoff
                    let delay = Duration::from_millis(100 * (1 << attempts));
                    sleep(delay).await;
                }
            }
        }
    }

    /// Single execution attempt
    async fn try_execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        let start = Instant::now();

        // Submit order
        let order_resp = self.client.submit_order_gateway(request).await?;
        let order_id = order_resp.id.clone();

        debug!("Order submitted: {}", order_id);

        // If dry run, simulate immediate fill
        if self.client.is_dry_run() {
            return Ok(ExecutionResult {
                order_id,
                status: OrderStatus::Filled,
                filled_shares: request.shares,
                avg_fill_price: Some(request.limit_price),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Optional best-effort confirmation: never fail the execution after a successful submit,
        // otherwise retry logic would resubmit and potentially create duplicates.
        if self.config.confirm_fills {
            let poll_interval = Duration::from_millis(self.config.poll_interval_ms.max(100));
            let confirm_timeout = Duration::from_millis(self.config.confirm_fill_timeout_ms);

            match timeout(
                confirm_timeout,
                self.wait_for_fill(&order_id, poll_interval),
            )
            .await
            {
                Ok(Ok(mut result)) => {
                    result.elapsed_ms = start.elapsed().as_millis() as u64;
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    warn!(
                        order_id,
                        error = %e,
                        "Order submitted but confirmation polling failed; returning Submitted"
                    );
                }
                Err(_) => {
                    debug!(
                        order_id,
                        timeout_ms = self.config.confirm_fill_timeout_ms,
                        "Order confirmation timed out; returning Submitted"
                    );
                }
            }

            // For non-resting orders, make a best-effort attempt to cancel and fetch final fill
            // so callers don't treat a partially-filled order as 0-fill.
            match request.time_in_force {
                crate::domain::TimeInForce::IOC | crate::domain::TimeInForce::FOK => {
                    let _ = self.client.cancel_order(&order_id).await;
                    if let Ok(order) = self.client.get_order(&order_id).await {
                        let status = self.client.infer_order_status(&order);
                        let (filled_u64, price) = self.client.calculate_fill(&order);

                        return Ok(ExecutionResult {
                            order_id,
                            status,
                            filled_shares: filled_u64,
                            avg_fill_price: price,
                            elapsed_ms: start.elapsed().as_millis() as u64,
                        });
                    }
                }
                crate::domain::TimeInForce::GTC => {}
            }
        }

        // Default: return immediately after submission (order is live on the book).
        info!(
            "Order {} submitted to market, status: {}",
            order_id, order_resp.status
        );

        Ok(ExecutionResult {
            order_id,
            status: OrderStatus::Submitted, // Order is live on the book
            filled_shares: 0,               // Will be determined at market resolution
            avg_fill_price: Some(request.limit_price),
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Poll for order fill
    async fn wait_for_fill(
        &self,
        order_id: &str,
        poll_interval: Duration,
    ) -> Result<ExecutionResult> {
        loop {
            let order = self.client.get_order(order_id).await?;
            let status = self.client.infer_order_status(&order);
            let (filled_u64, price) = self.client.calculate_fill(&order);

            match status {
                OrderStatus::Filled => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        status,
                        filled_shares: filled_u64,
                        avg_fill_price: price,
                        elapsed_ms: 0, // Will be updated by caller
                    });
                }
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        status,
                        filled_shares: filled_u64,
                        avg_fill_price: price,
                        elapsed_ms: 0,
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
    ) -> Result<ExecutionResult> {
        let request = OrderRequest::buy_limit(token_id.to_string(), market_side, shares, price);
        self.execute(&request).await
    }

    /// Create and execute a sell order
    pub async fn sell(
        &self,
        token_id: &str,
        market_side: Side,
        shares: u64,
        price: Decimal,
    ) -> Result<ExecutionResult> {
        let request = OrderRequest::sell_limit(token_id.to_string(), market_side, shares, price);
        self.execute(&request).await
    }

    /// Cancel an order
    pub async fn cancel(&self, order_id: &str) -> Result<bool> {
        self.client.cancel_order(order_id).await
    }

    /// Get current best prices for a token
    pub async fn get_prices(&self, token_id: &str) -> Result<(Option<Decimal>, Option<Decimal>)> {
        self.client.get_best_prices(token_id).await
    }

    /// Execute multiple orders in batch with concurrent submission
    ///
    /// This method submits multiple orders concurrently, providing significant
    /// performance improvements over sequential submission:
    /// - 10-100x faster for large batches
    /// - Reduced latency variance
    /// - Better resource utilization
    ///
    /// # Arguments
    /// * `requests` - Vector of order requests to execute
    ///
    /// # Returns
    /// Vector of results, one for each request. Failed orders return errors
    /// but don't prevent other orders from executing.
    pub async fn execute_batch(&self, requests: Vec<OrderRequest>) -> Vec<Result<ExecutionResult>> {
        use futures_util::future::join_all;

        // Submit all orders concurrently - clone requests to avoid lifetime issues
        let futures: Vec<_> = requests
            .iter()
            .cloned()
            .map(|request| async move { self.execute(&request).await })
            .collect();

        // Wait for all to complete
        join_all(futures).await
    }

    /// Execute multiple orders in batch with rate limiting
    ///
    /// Similar to execute_batch but with controlled concurrency to avoid
    /// overwhelming the exchange API or hitting rate limits.
    ///
    /// # Arguments
    /// * `requests` - Vector of order requests to execute
    /// * `max_concurrent` - Maximum number of concurrent requests (default: 10)
    ///
    /// # Returns
    /// Vector of results, one for each request
    pub async fn execute_batch_with_limit(
        &self,
        requests: Vec<OrderRequest>,
        max_concurrent: usize,
    ) -> Vec<Result<ExecutionResult>> {
        use futures_util::stream::{self, StreamExt};

        // Process requests with concurrency limit - clone to avoid lifetime issues
        stream::iter(requests.iter().cloned())
            .map(|request| async move { self.execute(&request).await })
            .buffer_unordered(max_concurrent)
            .collect::<Vec<_>>()
            .await
    }
}

/// Helper for building execution parameters
pub struct ExecutionParams {
    pub shares: u64,
    pub max_price: Decimal,
    pub slippage_tolerance: Decimal,
}

impl ExecutionParams {
    pub fn new(shares: u64, max_price: Decimal) -> Self {
        Self {
            shares,
            max_price,
            slippage_tolerance: Decimal::ZERO,
        }
    }

    pub fn with_slippage(mut self, tolerance: Decimal) -> Self {
        self.slippage_tolerance = tolerance;
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
        let params = ExecutionParams::new(100, dec!(0.50)).with_slippage(dec!(0.02));

        // 0.50 * 1.02 = 0.51
        assert_eq!(params.effective_max_price(), dec!(0.51));
    }
}
