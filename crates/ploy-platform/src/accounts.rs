use chrono::{DateTime, Utc};
use ploy_operator_contracts::{
    AccountClaimDetailResponse, AccountClaimStatus, ClaimExecutionRecord, ClaimLoopState,
    RedeemablePositionSnapshot,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub account_id: String,
    pub runtime_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountClaimSnapshot {
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

impl AccountClaimSnapshot {
    pub fn for_runtime_mode(
        account_id: impl Into<String>,
        runtime_mode: impl Into<String>,
    ) -> Self {
        let runtime_mode = runtime_mode.into();
        let loop_state = if runtime_mode == "live" {
            ClaimLoopState::Running
        } else {
            ClaimLoopState::Paused
        };
        let enabled = runtime_mode == "live";
        Self {
            account_id: account_id.into(),
            enabled,
            runtime_mode,
            loop_state,
            last_scan_at: None,
            last_claim_at: None,
            last_error: None,
            consecutive_failures: 0,
            next_retry_at: None,
            pending_redeemable_count: 0,
            pending_redeemable_notional: Decimal::ZERO,
        }
    }

    pub fn status(&self) -> AccountClaimStatus {
        AccountClaimStatus {
            account_id: self.account_id.clone(),
            enabled: self.enabled,
            runtime_mode: self.runtime_mode.clone(),
            loop_state: self.loop_state,
            last_scan_at: self.last_scan_at,
            last_claim_at: self.last_claim_at,
            last_error: self.last_error.clone(),
            consecutive_failures: self.consecutive_failures,
            next_retry_at: self.next_retry_at,
            pending_redeemable_count: self.pending_redeemable_count,
            pending_redeemable_notional: self.pending_redeemable_notional,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountClaimDetail {
    pub status: AccountClaimSnapshot,
    #[serde(default)]
    pub redeemable_positions: Vec<RedeemablePositionSnapshot>,
    #[serde(default)]
    pub claim_history: Vec<ClaimExecutionRecord>,
}

impl AccountClaimDetail {
    fn new(status: AccountClaimSnapshot) -> Self {
        Self {
            status,
            redeemable_positions: Vec::new(),
            claim_history: Vec::new(),
        }
    }

    pub fn response(&self) -> AccountClaimDetailResponse {
        AccountClaimDetailResponse {
            status: self.status.status(),
            redeemable_positions: self.redeemable_positions.clone(),
            claim_history: self.claim_history.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct AccountClaimRegistry {
    accounts: BTreeMap<String, AccountClaimDetail>,
}

impl AccountClaimRegistry {
    pub fn upsert(&mut self, snapshot: AccountClaimSnapshot) -> &AccountClaimDetail {
        let account_id = snapshot.account_id.clone();
        self.accounts
            .entry(account_id.clone())
            .and_modify(|detail| detail.status = snapshot.clone())
            .or_insert_with(|| AccountClaimDetail::new(snapshot));
        self.accounts
            .get(&account_id)
            .expect("claim account inserted")
    }

    pub fn get(&self, account_id: &str) -> Option<&AccountClaimSnapshot> {
        self.accounts.get(account_id).map(|detail| &detail.status)
    }

    pub fn detail(&self, account_id: &str) -> Option<&AccountClaimDetail> {
        self.accounts.get(account_id)
    }

    pub fn statuses(&self) -> Vec<AccountClaimStatus> {
        self.accounts
            .values()
            .map(|detail| detail.status.status())
            .collect()
    }

    pub fn details(&self) -> Vec<AccountClaimDetailResponse> {
        self.accounts
            .values()
            .map(AccountClaimDetail::response)
            .collect()
    }

    pub fn records(&self) -> Vec<AccountClaimDetail> {
        self.accounts.values().cloned().collect()
    }

    pub fn restore(&mut self, records: Vec<AccountClaimDetail>) {
        self.accounts.clear();
        for record in records {
            self.accounts
                .insert(record.status.account_id.clone(), record);
        }
    }

    pub fn set_redeemable_positions(
        &mut self,
        account_id: &str,
        redeemable_positions: Vec<RedeemablePositionSnapshot>,
    ) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.pending_redeemable_count = redeemable_positions.len();
        detail.status.pending_redeemable_notional = redeemable_positions
            .iter()
            .map(|position| position.estimated_payout)
            .fold(Decimal::ZERO, |acc, value| acc + value);
        detail.redeemable_positions = redeemable_positions;
        Some(detail)
    }

    pub fn append_claim_record(
        &mut self,
        account_id: &str,
        record: ClaimExecutionRecord,
    ) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.last_claim_at = Some(record.submitted_at);
        detail.claim_history.push(record);
        Some(detail)
    }

    pub fn mark_scan_complete(
        &mut self,
        account_id: &str,
        scanned_at: DateTime<Utc>,
    ) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.last_scan_at = Some(scanned_at);
        Some(detail)
    }

    pub fn mark_running(&mut self, account_id: &str) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.loop_state = ClaimLoopState::Running;
        detail.status.consecutive_failures = 0;
        detail.status.next_retry_at = None;
        detail.status.last_error = None;
        Some(detail)
    }

    pub fn mark_recovering(
        &mut self,
        account_id: &str,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.loop_state = ClaimLoopState::Recovering;
        detail.status.next_retry_at = next_retry_at;
        Some(detail)
    }

    pub fn mark_degraded(
        &mut self,
        account_id: &str,
        error: String,
        next_retry_at: Option<DateTime<Utc>>,
    ) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.loop_state = ClaimLoopState::Degraded;
        detail.status.consecutive_failures = detail.status.consecutive_failures.saturating_add(1);
        detail.status.last_error = Some(error);
        detail.status.next_retry_at = next_retry_at;
        Some(detail)
    }

    pub fn set_enabled(&mut self, account_id: &str, enabled: bool) -> Option<&AccountClaimDetail> {
        let detail = self.accounts.get_mut(account_id)?;
        detail.status.enabled = enabled;
        detail.status.loop_state = if enabled {
            ClaimLoopState::Running
        } else {
            ClaimLoopState::Paused
        };
        Some(detail)
    }

    pub fn retain_accounts(&mut self, account_ids: &BTreeSet<String>) {
        self.accounts
            .retain(|account_id, _| account_ids.contains(account_id));
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use rust_decimal::Decimal;

    use super::{AccountClaimRegistry, AccountClaimSnapshot, ClaimLoopState};
    use ploy_operator_contracts::{
        ClaimExecutionOutcome, ClaimExecutionRecord, ClaimPositionState, RedeemablePositionSnapshot,
    };

    #[test]
    fn live_accounts_default_to_running_claim_loop() {
        let snapshot = AccountClaimSnapshot::for_runtime_mode("acct-live", "live");
        assert!(snapshot.enabled);
        assert_eq!(snapshot.loop_state, ClaimLoopState::Running);
        assert_eq!(snapshot.pending_redeemable_count, 0);
        assert_eq!(snapshot.pending_redeemable_notional, Decimal::ZERO);
    }

    #[test]
    fn registry_tracks_account_claim_snapshot_by_account_id() {
        let mut registry = AccountClaimRegistry::default();
        registry.upsert(AccountClaimSnapshot {
            account_id: "acct-live".to_string(),
            enabled: true,
            runtime_mode: "live".to_string(),
            loop_state: ClaimLoopState::Running,
            last_scan_at: Some(Utc.with_ymd_and_hms(2026, 3, 22, 0, 0, 0).unwrap()),
            last_claim_at: None,
            last_error: None,
            consecutive_failures: 0,
            next_retry_at: None,
            pending_redeemable_count: 2,
            pending_redeemable_notional: Decimal::new(250, 2),
        });

        let stored = registry.get("acct-live").expect("snapshot");
        assert_eq!(stored.account_id, "acct-live");
        assert_eq!(stored.pending_redeemable_count, 2);
        assert_eq!(stored.pending_redeemable_notional, Decimal::new(250, 2));
    }

    #[test]
    fn registry_tracks_positions_and_claim_history() {
        let mut registry = AccountClaimRegistry::default();
        registry.upsert(AccountClaimSnapshot::for_runtime_mode("acct-live", "live"));

        registry.set_redeemable_positions(
            "acct-live",
            vec![RedeemablePositionSnapshot {
                account_id: "acct-live".to_string(),
                condition_id: "condition-1".to_string(),
                market_id: Some("market-1".to_string()),
                token_ids: vec!["1".to_string()],
                outcome_labels: vec!["YES".to_string()],
                redeemable_size: Decimal::new(100, 2),
                estimated_payout: Decimal::new(125, 2),
                detected_at: Utc.with_ymd_and_hms(2026, 3, 22, 0, 0, 0).unwrap(),
                claim_state: ClaimPositionState::Detected,
            }],
        );
        registry.append_claim_record(
            "acct-live",
            ClaimExecutionRecord {
                claim_id: "claim-1".to_string(),
                account_id: "acct-live".to_string(),
                condition_id: "condition-1".to_string(),
                submitted_at: Utc.with_ymd_and_hms(2026, 3, 22, 0, 1, 0).unwrap(),
                completed_at: None,
                tx_hash: Some("0xtx".to_string()),
                amount_claimed: Decimal::new(125, 2),
                outcome: ClaimExecutionOutcome::Submitted,
                error_message: None,
            },
        );

        let detail = registry.detail("acct-live").expect("detail");
        assert_eq!(detail.status.pending_redeemable_count, 1);
        assert_eq!(
            detail.status.pending_redeemable_notional,
            Decimal::new(125, 2)
        );
        assert_eq!(detail.redeemable_positions.len(), 1);
        assert_eq!(detail.claim_history.len(), 1);
        assert_eq!(
            detail.status.last_claim_at,
            Some(Utc.with_ymd_and_hms(2026, 3, 22, 0, 1, 0).unwrap())
        );
    }

    #[test]
    fn registry_can_mark_degraded_and_running_again() {
        let mut registry = AccountClaimRegistry::default();
        registry.upsert(AccountClaimSnapshot::for_runtime_mode("acct-live", "live"));

        registry.mark_degraded("acct-live", "relay unavailable".to_string(), None);
        let degraded = registry.get("acct-live").expect("degraded");
        assert_eq!(degraded.loop_state, ClaimLoopState::Degraded);
        assert_eq!(degraded.consecutive_failures, 1);
        assert_eq!(degraded.last_error.as_deref(), Some("relay unavailable"));

        registry.mark_running("acct-live");
        let running = registry.get("acct-live").expect("running");
        assert_eq!(running.loop_state, ClaimLoopState::Running);
        assert_eq!(running.consecutive_failures, 0);
        assert!(running.last_error.is_none());
    }
}
