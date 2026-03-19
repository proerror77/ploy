use crate::coordinator::OrderIntent;
use crate::domain::OrderStatus;

use super::Coordinator;

impl Coordinator {
    pub(super) async fn block_order_intent(
        &self,
        intent: &OrderIntent,
        reason: impl Into<String>,
        adjusted: Option<(u64, String)>,
        log_message: &'static str,
    ) {
        self.reject_order_intent(
            intent,
            OrderStatus::Rejected,
            "BLOCKED",
            reason.into(),
            adjusted,
            log_message,
        )
        .await;
    }

    pub(super) async fn reject_order_intent(
        &self,
        intent: &OrderIntent,
        status: OrderStatus,
        decision: &'static str,
        reason: String,
        adjusted: Option<(u64, String)>,
        log_message: &'static str,
    ) {
        self.journal
            .persist_risk_decision(intent, decision, Some(reason.clone()), adjusted)
            .await;
        self.emit_rejected_intent_update(intent, status, reason.clone())
            .await;
        tracing::warn!(
            agent_id = %intent.agent_id,
            intent_id = %intent.intent_id,
            reason = %reason,
            "{log_message}"
        );
    }

    pub(super) async fn fail_order_intent(
        &self,
        intent: &OrderIntent,
        reason: impl Into<String>,
        log_message: &'static str,
    ) {
        let reason = reason.into();
        self.emit_rejected_intent_update(intent, OrderStatus::Failed, reason.clone())
            .await;
        tracing::warn!(
            agent_id = %intent.agent_id,
            intent_id = %intent.intent_id,
            reason = %reason,
            "{log_message}"
        );
    }
}
