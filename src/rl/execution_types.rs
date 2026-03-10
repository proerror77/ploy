use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::coordinator::OrderIntent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    Pending,
    Submitted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Expired,
    RiskBlocked,
}

#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub intent_id: Uuid,
    pub agent_id: String,
    pub order_id: Option<String>,
    pub status: ExecutionStatus,
    pub filled_shares: u64,
    pub avg_fill_price: Option<Decimal>,
    pub fees: Decimal,
    pub error_message: Option<String>,
    pub executed_at: DateTime<Utc>,
    pub latency_ms: u64,
}

impl ExecutionReport {
    pub fn success(
        intent: &OrderIntent,
        order_id: String,
        filled: u64,
        avg_price: Decimal,
    ) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: Some(order_id),
            status: if filled == intent.shares {
                ExecutionStatus::Filled
            } else {
                ExecutionStatus::PartiallyFilled
            },
            filled_shares: filled,
            avg_fill_price: Some(avg_price),
            fees: Decimal::ZERO,
            error_message: None,
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn rejected(intent: &OrderIntent, reason: impl Into<String>) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: None,
            status: ExecutionStatus::Rejected,
            filled_shares: 0,
            avg_fill_price: None,
            fees: Decimal::ZERO,
            error_message: Some(reason.into()),
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn risk_blocked(intent: &OrderIntent, reason: impl Into<String>) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: None,
            status: ExecutionStatus::RiskBlocked,
            filled_shares: 0,
            avg_fill_price: None,
            fees: Decimal::ZERO,
            error_message: Some(reason.into()),
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Filled | ExecutionStatus::PartiallyFilled
        )
    }
}
