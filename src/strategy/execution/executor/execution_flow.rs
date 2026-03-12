use super::*;

impl OrderExecutor {
    /// Execute an order with retry logic and idempotency protection
    pub async fn execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
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
                    debug!("New order request (key: {})", idem_key);
                }
            }

            let result = self.execute_with_retry(request).await;

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
            self.execute_with_retry(request).await
        }
    }

    pub(super) fn cached_result(
        record: IdempotencyRecord,
        request: &OrderRequest,
    ) -> Result<ExecutionResult> {
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

    pub(super) fn retryable_order_submission_message(message: &str) -> bool {
        let normalized = message.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return true;
        }

        let definitely_non_retryable = [
            "invalid token_id",
            "invalid token id",
            "invalid price",
            "invalid size",
            "failed to build order",
            "failed to sign order",
            "gateway-only mode: idempotency_key is required",
            "gateway-only mode: client_order_id must start with 'intent:'",
            "not authenticated",
            "authentication error",
            "signature error",
            "insufficient liquidity",
        ];
        if definitely_non_retryable
            .iter()
            .any(|needle| normalized.contains(needle))
        {
            return false;
        }

        let definitely_retryable = [
            "rate limit",
            "timeout",
            "timed out",
            "temporar",
            "connection reset",
            "connection refused",
            "service unavailable",
            "bad gateway",
            "gateway timeout",
            "502",
            "503",
            "504",
            "too many requests",
            "network",
        ];
        if definitely_retryable
            .iter()
            .any(|needle| normalized.contains(needle))
        {
            return true;
        }

        true
    }

    pub(super) fn error_is_retryable(error: &crate::error::PloyError) -> bool {
        match error {
            crate::error::PloyError::Validation(_)
            | crate::error::PloyError::Auth(_)
            | crate::error::PloyError::AddressParsing(_)
            | crate::error::PloyError::Wallet(_)
            | crate::error::PloyError::Signature(_)
            | crate::error::PloyError::OrderRejected(_)
            | crate::error::PloyError::InsufficientLiquidity(_) => false,
            crate::error::PloyError::OrderSubmission(message) => {
                Self::retryable_order_submission_message(message)
            }
            crate::error::PloyError::Cancelled => false,
            crate::error::PloyError::RateLimited(_)
            | crate::error::PloyError::Http(_)
            | crate::error::PloyError::WebSocket(_)
            | crate::error::PloyError::OrderTimeout(_)
            | crate::error::PloyError::MarketDataUnavailable(_)
            | crate::error::PloyError::StaleData(_) => true,
            _ => true,
        }
    }

    /// Execute order with retry logic (internal method)
    pub(super) async fn execute_with_retry(
        &self,
        request: &OrderRequest,
    ) -> Result<ExecutionResult> {
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
                    let retryable = Self::error_is_retryable(&e);
                    if !retryable {
                        error!(
                            attempts,
                            error = %e,
                            "Order execution failed with non-retryable error"
                        );
                        return Err(e);
                    }

                    if attempts >= self.config.max_retries {
                        error!("Order execution failed after {} attempts: {}", attempts, e);
                        return Err(crate::error::PloyError::OrderSubmission(format!(
                            "Max retries exceeded after {} attempts; last error: {}",
                            attempts, e
                        )));
                    }

                    warn!("Order attempt {} failed: {}. Retrying...", attempts, e);
                    let delay = Duration::from_millis(100 * (1 << attempts));
                    sleep(delay).await;
                }
            }
        }
    }

    /// Single execution attempt
    pub(super) async fn try_execute(&self, request: &OrderRequest) -> Result<ExecutionResult> {
        let start = Instant::now();
        let order_resp = self.client.submit_order_gateway(request).await?;
        let order_id = order_resp.id.clone();

        debug!("Order submitted: {}", order_id);

        if self.client.is_dry_run() {
            return Ok(ExecutionResult {
                order_id,
                status: OrderStatus::Filled,
                filled_shares: request.shares,
                avg_fill_price: Some(request.limit_price),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }

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

        let mut immediate_status = self.client.infer_order_status(&order_resp);
        let (immediate_filled, immediate_price) = self.client.calculate_fill(&order_resp);
        if immediate_status == OrderStatus::Submitted && immediate_filled > 0 {
            immediate_status = OrderStatus::PartiallyFilled;
        }
        if immediate_status != OrderStatus::Submitted {
            if should_reconcile_immediate_fill(&order_resp, immediate_status, immediate_filled) {
                if let Ok(reconciled) = self.reconcile_immediate_fill(&order_id).await {
                    if reconciled.status != OrderStatus::Submitted
                        && reconciled.filled_shares >= immediate_filled
                    {
                        return Ok(ExecutionResult {
                            order_id,
                            status: reconciled.status,
                            filled_shares: reconciled.filled_shares,
                            avg_fill_price: reconciled
                                .avg_fill_price
                                .or(immediate_price)
                                .or(Some(request.limit_price)),
                            elapsed_ms: start.elapsed().as_millis() as u64,
                        });
                    }
                }
            }

            return Ok(ExecutionResult {
                order_id,
                status: immediate_status,
                filled_shares: immediate_filled,
                avg_fill_price: immediate_price.or(Some(request.limit_price)),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }

        info!(
            "Order {} submitted to market, status: {}",
            order_id, order_resp.status
        );

        Ok(ExecutionResult {
            order_id,
            status: OrderStatus::Submitted,
            filled_shares: 0,
            avg_fill_price: Some(request.limit_price),
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Poll for order fill
    pub(super) async fn wait_for_fill(
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
                        elapsed_ms: 0,
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
                    sleep(poll_interval).await;
                }
            }
        }
    }

    pub(super) async fn reconcile_immediate_fill(&self, order_id: &str) -> Result<ExecutionResult> {
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
}

fn should_reconcile_immediate_fill(
    order_resp: &crate::adapters::OrderResponse,
    status: OrderStatus,
    filled_shares: u64,
) -> bool {
    filled_shares > 0
        && matches!(
            status,
            OrderStatus::Filled
                | OrderStatus::PartiallyFilled
                | OrderStatus::Cancelled
                | OrderStatus::Expired
        )
        && order_resp.associate_trades.is_none()
}
