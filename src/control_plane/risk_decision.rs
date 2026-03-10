use serde::{Deserialize, Serialize};

/// Risk gate outcome for a `TradeIntent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskDecision {
    pub status: RiskDecisionStatus,
    pub reason_code: Option<String>,
    pub message: Option<String>,
    pub suggested_max_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskDecisionStatus {
    Allow,
    Deny,
    Throttle,
}
