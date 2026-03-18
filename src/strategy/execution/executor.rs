use super::idempotency::IdempotencyManager;
use crate::adapters::{FeishuNotifier, PolymarketClient};
use crate::config::ExecutionConfig;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::Result;
use crate::exchange::ExchangeClient;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, error, info, warn};

mod execution_flow;
mod idempotency_flow;

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

    /// Query latest order status from exchange without submitting/canceling.
    pub async fn query_order_status(&self, order_id: &str) -> Result<ExecutionResult> {
        let order = self.client.get_order(order_id).await?;
        let status = self.client.infer_order_status(&order);
        let (filled_u64, avg_fill_price) = self.client.calculate_fill(&order);
        Ok(ExecutionResult {
            order_id: order_id.to_string(),
            status,
            filled_shares: filled_u64,
            avg_fill_price,
            elapsed_ms: 0,
        })
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
    use crate::adapters::OrderResponse;
    use crate::config::ExecutionConfig;
    use crate::exchange::{ExchangeClient, ExchangeKind};
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_execution_params() {
        let params = ExecutionParams::new(100, dec!(0.50)).with_slippage(dec!(0.02));

        // 0.50 * 1.02 = 0.51
        assert_eq!(params.effective_max_price(), dec!(0.51));
    }

    #[derive(Default)]
    struct MockExchangeClient {
        submit_results: Mutex<VecDeque<Result<OrderResponse>>>,
        get_order_responses: Mutex<VecDeque<OrderResponse>>,
        get_order_calls: Mutex<Vec<String>>,
        submit_calls: Mutex<u32>,
    }

    impl MockExchangeClient {
        fn with_submit_response(self, response: OrderResponse) -> Self {
            self.submit_results
                .lock()
                .expect("submit_results lock")
                .push_back(Ok(response));
            self
        }

        fn with_submit_error(self, error: crate::error::PloyError) -> Self {
            self.submit_results
                .lock()
                .expect("submit_results lock")
                .push_back(Err(error));
            self
        }

        fn with_get_order_response(self, response: OrderResponse) -> Self {
            self.get_order_responses
                .lock()
                .expect("get_order_responses lock")
                .push_back(response);
            self
        }
    }

    #[async_trait]
    impl ExchangeClient for MockExchangeClient {
        fn kind(&self) -> ExchangeKind {
            ExchangeKind::Polymarket
        }

        fn is_dry_run(&self) -> bool {
            false
        }

        async fn submit_order_gateway(&self, _request: &OrderRequest) -> Result<OrderResponse> {
            *self.submit_calls.lock().expect("submit_calls lock") += 1;
            self.submit_results
                .lock()
                .expect("submit_results lock")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(crate::error::PloyError::Internal(
                        "missing submit response".to_string(),
                    ))
                })
        }

        async fn get_order(&self, order_id: &str) -> Result<OrderResponse> {
            self.get_order_calls
                .lock()
                .expect("get_order_calls lock")
                .push(order_id.to_string());
            self.get_order_responses
                .lock()
                .expect("get_order_responses lock")
                .pop_front()
                .ok_or_else(|| {
                    crate::error::PloyError::Internal("missing get_order response".to_string())
                })
        }

        async fn cancel_order(&self, _order_id: &str) -> Result<bool> {
            Ok(true)
        }

        async fn get_best_prices(
            &self,
            _token_id: &str,
        ) -> Result<(Option<Decimal>, Option<Decimal>)> {
            Ok((None, None))
        }

        fn infer_order_status(&self, order: &OrderResponse) -> OrderStatus {
            crate::adapters::PolymarketClient::infer_order_status(order)
        }

        fn calculate_fill(&self, order: &OrderResponse) -> (u64, Option<Decimal>) {
            let (filled, price) = crate::adapters::PolymarketClient::calculate_fill(order);
            (filled.to_u64().unwrap_or(0), Some(price))
        }
    }

    fn make_order_response(
        id: &str,
        status: &str,
        matched: &str,
        original: &str,
        price: &str,
    ) -> OrderResponse {
        OrderResponse {
            id: id.to_string(),
            status: status.to_string(),
            owner: None,
            market: None,
            asset_id: None,
            side: None,
            original_size: Some(original.to_string()),
            size_matched: Some(matched.to_string()),
            price: Some(price.to_string()),
            associate_trades: None,
            created_at: None,
            expiration: None,
            order_type: None,
        }
    }

    #[tokio::test]
    async fn execute_reconciles_immediate_fill_with_order_query() {
        let client = MockExchangeClient::default()
            .with_submit_response(make_order_response(
                "exchange-1",
                "FILLED",
                "20",
                "20",
                "0.40",
            ))
            .with_get_order_response(make_order_response(
                "exchange-1",
                "FILLED",
                "20",
                "20",
                "0.34",
            ));
        let executor =
            OrderExecutor::new_with_exchange(Arc::new(client), ExecutionConfig::default());
        let request = OrderRequest::buy_limit("token-1".to_string(), Side::Up, 20, dec!(0.40));

        let result = executor
            .execute(&request)
            .await
            .expect("execution should succeed");

        assert_eq!(result.status, OrderStatus::Filled);
        assert_eq!(result.filled_shares, 20);
        assert_eq!(result.avg_fill_price, Some(dec!(0.34)));
    }

    #[tokio::test]
    async fn execute_stops_retrying_non_retryable_validation_errors() {
        let client = Arc::new(MockExchangeClient::default().with_submit_error(
            crate::error::PloyError::Validation("bad request".to_string()),
        ));
        let mut config = ExecutionConfig::default();
        config.max_retries = 3;
        let executor = OrderExecutor::new_with_exchange(client.clone(), config);
        let request = OrderRequest::buy_limit("token-1".to_string(), Side::Up, 20, dec!(0.40));

        let err = executor
            .execute(&request)
            .await
            .expect_err("validation error should not be retried");

        assert!(matches!(err, crate::error::PloyError::Validation(_)));
        assert_eq!(*client.submit_calls.lock().expect("submit_calls lock"), 1);
    }

    #[tokio::test]
    async fn execute_reports_last_retryable_error_when_retries_exhausted() {
        let client = Arc::new(
            MockExchangeClient::default()
                .with_submit_error(crate::error::PloyError::OrderSubmission(
                    "temporary backend 503".to_string(),
                ))
                .with_submit_error(crate::error::PloyError::OrderSubmission(
                    "temporary backend 503".to_string(),
                )),
        );
        let mut config = ExecutionConfig::default();
        config.max_retries = 2;
        let executor = OrderExecutor::new_with_exchange(client.clone(), config);
        let request = OrderRequest::buy_limit("token-1".to_string(), Side::Up, 20, dec!(0.40));

        let err = executor
            .execute(&request)
            .await
            .expect_err("retry exhaustion should surface the last error");

        assert!(matches!(err, crate::error::PloyError::OrderSubmission(_)));
        let message = err.to_string();
        assert!(message.contains("Max retries exceeded after 2 attempts"));
        assert!(message.contains("temporary backend 503"));
        assert_eq!(*client.submit_calls.lock().expect("submit_calls lock"), 2);
    }
}
