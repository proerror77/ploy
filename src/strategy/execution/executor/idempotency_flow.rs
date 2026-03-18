use super::{ExecutionResult, OrderExecutor};
use crate::domain::{OrderRequest, OrderStatus};
use crate::error::{PloyError, Result};
use crate::strategy::execution::idempotency::{
    IdempotencyManager, IdempotencyRecord, IdempotencyResult,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{debug, info, warn};

pub(super) struct IdempotencyExecution {
    manager: Arc<IdempotencyManager>,
    key: String,
}

pub(super) enum IdempotencyAction {
    Skip,
    Track(IdempotencyExecution),
    Return(ExecutionResult),
}

impl OrderExecutor {
    pub(super) async fn begin_idempotent_execution(
        &self,
        request: &OrderRequest,
    ) -> Result<IdempotencyAction> {
        let Some(idempotency) = self.idempotency.clone() else {
            return Ok(IdempotencyAction::Skip);
        };

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

                let record = IdempotencyRecord {
                    order_id,
                    status,
                    response_data,
                    error_message,
                };

                self.resolve_duplicate_record(&idempotency, &idem_key, request, record)
                    .await
            }
            IdempotencyResult::New => {
                debug!("New order request (key: {})", idem_key);
                Ok(IdempotencyAction::Track(IdempotencyExecution {
                    manager: idempotency,
                    key: idem_key,
                }))
            }
        }
    }

    pub(super) async fn finish_idempotent_execution(
        &self,
        tracking: Option<&IdempotencyExecution>,
        result: &Result<ExecutionResult>,
    ) {
        let Some(tracking) = tracking else {
            return;
        };

        match result {
            Ok(exec_result) => {
                if let Err(e) = tracking
                    .manager
                    .mark_completed(&tracking.key, &exec_result.order_id, exec_result)
                    .await
                {
                    warn!("Failed to mark idempotency as completed: {}", e);
                }
            }
            Err(error) => {
                if let Err(mark_err) = tracking
                    .manager
                    .mark_failed(&tracking.key, &error.to_string())
                    .await
                {
                    warn!("Failed to mark idempotency as failed: {}", mark_err);
                }
            }
        }
    }

    async fn resolve_duplicate_record(
        &self,
        idempotency: &Arc<IdempotencyManager>,
        idem_key: &str,
        request: &OrderRequest,
        record: IdempotencyRecord,
    ) -> Result<IdempotencyAction> {
        match record.status.to_lowercase().as_str() {
            "completed" => Ok(IdempotencyAction::Return(Self::cached_result(
                record, request,
            )?)),
            "failed" => Err(Self::failed_duplicate_error(record.error_message)),
            _ => {
                self.poll_pending_duplicate(idempotency, idem_key, request)
                    .await
            }
        }
    }

    async fn poll_pending_duplicate(
        &self,
        idempotency: &Arc<IdempotencyManager>,
        idem_key: &str,
        request: &OrderRequest,
    ) -> Result<IdempotencyAction> {
        warn!("Previous order attempt still pending, polling idempotency status...");

        let poll_interval = Duration::from_millis(self.config.poll_interval_ms.max(100));
        let timeout_ms = self
            .config
            .confirm_fill_timeout_ms
            .max(poll_interval.as_millis() as u64);
        let start = Instant::now();

        loop {
            if start.elapsed() >= Duration::from_millis(timeout_ms) {
                return Err(PloyError::OrderSubmission(
                    "Order already pending; retry later".to_string(),
                ));
            }

            sleep(poll_interval).await;
            let record = idempotency.fetch_record(idem_key).await?;

            match record.status.to_lowercase().as_str() {
                "completed" => {
                    return Ok(IdempotencyAction::Return(Self::cached_result(
                        record, request,
                    )?));
                }
                "failed" => {
                    return Err(Self::failed_duplicate_error(record.error_message));
                }
                _ => {}
            }
        }
    }

    fn failed_duplicate_error(message: Option<String>) -> PloyError {
        let msg = message.unwrap_or_else(|| "Previous attempt failed".to_string());
        PloyError::Internal(format!("Order submission failed: {}", msg))
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

        Err(PloyError::Internal(
            "Idempotency record completed without order_id".to_string(),
        ))
    }
}
