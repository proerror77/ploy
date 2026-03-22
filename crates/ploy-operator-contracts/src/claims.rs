use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimLoopState {
    Running,
    Degraded,
    Recovering,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPositionState {
    Detected,
    Claiming,
    Claimed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimExecutionOutcome {
    Submitted,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountClaimActionState {
    Accepted,
    Rejected,
    NotSupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountClaimStatus {
    pub account_id: String,
    pub enabled: bool,
    pub runtime_mode: String,
    pub loop_state: ClaimLoopState,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub last_claim_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub pending_redeemable_count: usize,
    pub pending_redeemable_notional: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemablePositionSnapshot {
    pub account_id: String,
    pub condition_id: String,
    pub market_id: Option<String>,
    #[serde(default)]
    pub token_ids: Vec<String>,
    #[serde(default)]
    pub outcome_labels: Vec<String>,
    pub redeemable_size: Decimal,
    pub estimated_payout: Decimal,
    pub detected_at: DateTime<Utc>,
    pub claim_state: ClaimPositionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimExecutionRecord {
    pub claim_id: String,
    pub account_id: String,
    pub condition_id: String,
    pub submitted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub tx_hash: Option<String>,
    pub amount_claimed: Decimal,
    pub outcome: ClaimExecutionOutcome,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountClaimActionResponse {
    pub account_id: String,
    pub state: AccountClaimActionState,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountClaimDetailResponse {
    pub status: AccountClaimStatus,
    #[serde(default)]
    pub redeemable_positions: Vec<RedeemablePositionSnapshot>,
    #[serde(default)]
    pub claim_history: Vec<ClaimExecutionRecord>,
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use serde_json::json;

    use super::{
        AccountClaimActionResponse, AccountClaimActionState, AccountClaimStatus,
        ClaimExecutionOutcome, ClaimExecutionRecord, ClaimLoopState, ClaimPositionState,
        RedeemablePositionSnapshot,
    };

    #[test]
    fn account_claim_status_uses_stable_wire_keys() {
        let value = serde_json::to_value(AccountClaimStatus {
            account_id: "acct-live".to_string(),
            enabled: true,
            runtime_mode: "live".to_string(),
            loop_state: ClaimLoopState::Running,
            last_scan_at: None,
            last_claim_at: None,
            last_error: None,
            consecutive_failures: 0,
            next_retry_at: None,
            pending_redeemable_count: 2,
            pending_redeemable_notional: Decimal::new(1250, 2),
        })
        .expect("serialize");

        assert_eq!(
            value,
            json!({
                "account_id": "acct-live",
                "enabled": true,
                "runtime_mode": "live",
                "loop_state": "running",
                "last_scan_at": null,
                "last_claim_at": null,
                "last_error": null,
                "consecutive_failures": 0,
                "next_retry_at": null,
                "pending_redeemable_count": 2,
                "pending_redeemable_notional": "12.50",
            })
        );
    }

    #[test]
    fn redeemable_position_snapshot_uses_stable_wire_keys() {
        let value = serde_json::to_value(RedeemablePositionSnapshot {
            account_id: "acct-live".to_string(),
            condition_id: "0xcondition".to_string(),
            market_id: Some("market-1".to_string()),
            token_ids: vec!["1".to_string(), "2".to_string()],
            outcome_labels: vec!["YES".to_string(), "NO".to_string()],
            redeemable_size: Decimal::new(300, 2),
            estimated_payout: Decimal::new(300, 2),
            detected_at: "2026-03-22T00:00:00Z".parse().expect("time"),
            claim_state: ClaimPositionState::Detected,
        })
        .expect("serialize");

        assert_eq!(
            value,
            json!({
                "account_id": "acct-live",
                "condition_id": "0xcondition",
                "market_id": "market-1",
                "token_ids": ["1", "2"],
                "outcome_labels": ["YES", "NO"],
                "redeemable_size": "3.00",
                "estimated_payout": "3.00",
                "detected_at": "2026-03-22T00:00:00Z",
                "claim_state": "detected",
            })
        );
    }

    #[test]
    fn claim_execution_record_uses_stable_wire_keys() {
        let value = serde_json::to_value(ClaimExecutionRecord {
            claim_id: "claim-1".to_string(),
            account_id: "acct-live".to_string(),
            condition_id: "0xcondition".to_string(),
            submitted_at: "2026-03-22T00:00:00Z".parse().expect("time"),
            completed_at: None,
            tx_hash: Some("0xtx".to_string()),
            amount_claimed: Decimal::new(300, 2),
            outcome: ClaimExecutionOutcome::Submitted,
            error_message: None,
        })
        .expect("serialize");

        assert_eq!(
            value,
            json!({
                "claim_id": "claim-1",
                "account_id": "acct-live",
                "condition_id": "0xcondition",
                "submitted_at": "2026-03-22T00:00:00Z",
                "completed_at": null,
                "tx_hash": "0xtx",
                "amount_claimed": "3.00",
                "outcome": "submitted",
                "error_message": null,
            })
        );
    }

    #[test]
    fn account_claim_action_response_uses_stable_wire_keys() {
        let value = serde_json::to_value(AccountClaimActionResponse {
            account_id: "acct-live".to_string(),
            state: AccountClaimActionState::Accepted,
            message: "claim rescan accepted".to_string(),
        })
        .expect("serialize");

        assert_eq!(
            value,
            json!({
                "account_id": "acct-live",
                "state": "accepted",
                "message": "claim rescan accepted",
            })
        );
    }
}
