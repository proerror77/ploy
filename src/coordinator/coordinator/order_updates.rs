use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::warn;

use crate::domain::OrderStatus;
use crate::platform::OrderIntent;
use crate::strategy::{executor::ExecutionResult, OrderUpdate};

use super::Coordinator;

impl Coordinator {
    pub async fn register_order_updates(&mut self, agent_id: String) -> mpsc::Receiver<OrderUpdate> {
        let (tx, rx) = mpsc::channel(128);
        self.order_update_sinks.write().await.insert(agent_id, tx);
        rx
    }

    pub(super) async fn emit_pending_intent_update(&self, intent: &OrderIntent) {
        self.emit_intent_update(intent, OrderStatus::Pending, None, 0, None, None)
            .await;
    }

    pub(super) async fn emit_rejected_intent_update(
        &self,
        intent: &OrderIntent,
        status: OrderStatus,
        error: impl Into<String>,
    ) {
        self.emit_intent_update(intent, status, None, 0, None, Some(error.into()))
            .await;
    }

    pub(super) async fn emit_execution_result_update(
        &self,
        intent: &OrderIntent,
        result: &ExecutionResult,
    ) {
        self.emit_intent_update(
            intent,
            result.status,
            Some(result.order_id.as_str()),
            result.filled_shares,
            result.avg_fill_price,
            None,
        )
        .await;
    }

    async fn emit_intent_update(
        &self,
        intent: &OrderIntent,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
        filled_qty: u64,
        avg_fill_price: Option<Decimal>,
        error: Option<String>,
    ) {
        let client_order_id = intent.client_order_id.trim();
        if client_order_id.is_empty() {
            return;
        }
        self.emit_order_update(
            &intent.agent_id,
            OrderUpdate {
                order_id: exchange_order_id.unwrap_or(client_order_id).to_string(),
                client_order_id: Some(client_order_id.to_string()),
                status,
                filled_qty,
                avg_fill_price,
                timestamp: Utc::now(),
                error,
            },
        )
        .await;
    }

    async fn emit_order_update(&self, agent_id: &str, update: OrderUpdate) {
        let tx = {
            let sinks = self.order_update_sinks.read().await;
            match sinks.get(agent_id).cloned() {
                Some(tx) => tx,
                None => return,
            }
        };

        if tx.send(update).await.is_err() {
            warn!(agent_id, "strategy order update channel closed");
        }
    }
}
