use crate::adapters::{FeishuNotifier, PolymarketClient};
use crate::config::ExecutionConfig;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::{OrderError, Result};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, error, info, warn};

/// Order executor for managing order lifecycle
pub struct OrderExecutor {
    client: PolymarketClient,
    config: ExecutionConfig,
    feishu: Option<Arc<FeishuNotifier>>,
}

/// Execution result with fill details
#[derive(Debug, Clone)]
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
        Self {
            client,
            config,
            feishu: FeishuNotifier::from_env(),
        }
    }

    /// Set the Feishu notifier
    pub fn with_feishu(mut self, feishu: Option<Arc<FeishuNotifier>>) -> Self {
        self.feishu = feishu;
        self
    }

    /// Check if in dry run mode
    pub fn is_dry_run(&self) -> bool {
        self.client.is_dry_run()
    }

    /// Execute an order with retry logic
    pub async fn execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        let start = Instant::now();
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
                        let price = result.avg_fill_price
                            .map(|p| p.to_f64().unwrap_or(0.0))
                            .unwrap_or(request.limit_price.to_f64().unwrap_or(0.0));

                        // Use request.shares since filled_shares may be 0 for submitted orders
                        let shares = if result.filled_shares > 0 {
                            result.filled_shares
                        } else {
                            request.shares
                        };
                        feishu.notify_trade(
                            action,
                            &request.token_id[..16.min(request.token_id.len())],
                            side,
                            price,
                            shares as f64,
                            Some(&result.order_id),
                        ).await;
                    }

                    return Ok(result);
                }
                Err(e) => {
                    if attempts >= self.config.max_retries {
                        error!(
                            "Order execution failed after {} attempts: {}",
                            attempts, e
                        );
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
    async fn try_execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        let start = Instant::now();

        // Submit order
        let order_resp = self.client.submit_order(request).await?;
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

        // For 15-minute prediction markets, return immediately after submission
        // to avoid auth errors during polling that cause duplicate order submissions.
        // The order is already on the book - market resolution will handle the rest.
        info!("Order {} submitted to market, status: {}", order_id, order_resp.status);

        Ok(ExecutionResult {
            order_id,
            status: OrderStatus::Submitted, // Order is live on the book
            filled_shares: 0, // Will be determined at market resolution
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
            let status = PolymarketClient::parse_order_status(&order.status);
            let (filled, price) = PolymarketClient::calculate_fill(&order);
            let filled_u64 = filled.to_u64().unwrap_or(0);

            match status {
                OrderStatus::Filled => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        status,
                        filled_shares: filled_u64,
                        avg_fill_price: Some(price),
                        elapsed_ms: 0, // Will be updated by caller
                    });
                }
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    return Ok(ExecutionResult {
                        order_id: order_id.to_string(),
                        status,
                        filled_shares: filled_u64,
                        avg_fill_price: Some(price),
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
        let params = ExecutionParams::new(100, dec!(0.50))
            .with_slippage(dec!(0.02));

        // 0.50 * 1.02 = 0.51
        assert_eq!(params.effective_max_price(), dec!(0.51));
    }
}
