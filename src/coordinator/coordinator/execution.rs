use chrono::Utc;
use tracing::debug;

use crate::domain::OrderStatus;

use super::Coordinator;

impl Coordinator {
    /// Drain the order queue and execute via OrderExecutor.
    pub(super) async fn drain_and_execute(&self) {
        let (expired, batch) = {
            let mut queue = self.order_queue.write().await;
            let expired = queue.cleanup_expired_intents();
            let batch = queue.dequeue_batch(self.config.batch_size);
            (expired, batch)
        };

        for intent in expired {
            self.emit_rejected_intent_update(
                &intent,
                OrderStatus::Expired,
                "intent expired in coordinator queue".to_string(),
            )
            .await;
            self.settle_domain_failure(&intent).await;
        }

        if batch.is_empty() {
            return;
        }

        debug!(count = batch.len(), "draining order queue");

        for intent in batch {
            let execute_started_at = Utc::now();
            let queue_delay_ms = execute_started_at
                .signed_duration_since(intent.created_at)
                .num_milliseconds()
                .max(0);

            let request = self
                .admission
                .build_order_request(self.account_id.as_str(), &intent);

            match self.executor.execute(&request).await {
                Ok(result) => {
                    self.handle_execution_success(&intent, &request, &result, queue_delay_ms)
                        .await;
                }
                Err(error) => {
                    self.handle_execution_failure(
                        &intent,
                        &request,
                        error.to_string(),
                        queue_delay_ms,
                    )
                    .await;
                }
            }
        }
    }
}
