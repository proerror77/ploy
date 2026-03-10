use rust_decimal::Decimal;

use crate::coordinator::OrderIntent;
use crate::domain::OrderStatus;

use super::*;

impl Coordinator {
    /// Risk-check an incoming order intent and enqueue if passed.
    pub(super) async fn handle_order_intent(&self, intent: OrderIntent) {
        let mut intent = intent;
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;
        let strategy_max_shares = intent.shares;

        if let Some((reason, log_message)) = self.validate_runtime_order_intent(&intent).await {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                log_message,
            )
            .await;
            return;
        }

        if intent.is_buy && self.governance.is_agent_paused(&intent.agent_id).await {
            let reason = format!("Agent {} is paused; blocking BUY intent", intent.agent_id);
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by per-agent pause",
            )
            .await;
            return;
        }

        if let Some(reason) = self.check_governance_policy(&intent).await {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by global governance policy",
            )
            .await;
            return;
        }

        if let Err(reason) = self
            .admission
            .enforce_live_buy_deployment_gate(
                self.account_id.as_str(),
                self.executor.is_dry_run(),
                &self.allowed_domains,
                &mut intent,
            )
            .await
        {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by deployment gate",
            )
            .await;
            return;
        }

        self.journal.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.journal.persist_exit_reason_intent(&intent).await;
        }

        if let Some(reason) = self.admission.check_duplicate_intent(&intent).await {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by duplicate-intent guard",
            )
            .await;
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_kelly_sizing(&self.capital_policy, &mut intent)
            .await
        {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by kelly sizing policy",
            )
            .await;
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_min_order_constraints(&mut intent, strategy_max_shares)
        {
            self.reject_order_intent(
                &intent,
                OrderStatus::Rejected,
                "BLOCKED",
                reason,
                None,
                "order blocked by venue minimum constraints",
            )
            .await;
            return;
        }

        let mut adjusted: Option<(u64, String)> = None;
        let mut evaluated = intent;
        for attempt in 0..3 {
            match self.risk_gate.check_order(&evaluated).await {
                RiskCheckResult::Passed => {
                    if let Some(reason) = self.reserve_domain_capital(&evaluated).await {
                        self.reject_order_intent(
                            &evaluated,
                            OrderStatus::Rejected,
                            "BLOCKED",
                            reason,
                            adjusted.clone(),
                            "order blocked by domain allocator",
                        )
                        .await;
                        return;
                    }

                    self.journal
                        .persist_risk_decision(&evaluated, "PASSED", None, adjusted.clone())
                        .await;
                    let enqueue_result = {
                        let mut queue = self.order_queue.write().await;
                        queue.enqueue(evaluated.clone())
                    };
                    match enqueue_result {
                        Ok(()) => {
                            self.emit_pending_intent_update(&evaluated).await;
                            debug!(%agent_id, %intent_id, "order enqueued");
                        }
                        Err(error) => {
                            self.release_domain_reservation(intent_id).await;
                            self.emit_rejected_intent_update(
                                &evaluated,
                                OrderStatus::Failed,
                                format!("queue full, order dropped: {}", error),
                            )
                            .await;
                            warn!(%agent_id, %intent_id, error = %error, "queue full, order dropped");
                        }
                    }
                    return;
                }
                RiskCheckResult::Blocked(reason) => {
                    self.reject_order_intent(
                        &evaluated,
                        OrderStatus::Rejected,
                        "BLOCKED",
                        reason.to_string(),
                        adjusted.clone(),
                        "order blocked by risk gate",
                    )
                    .await;
                    return;
                }
                RiskCheckResult::Adjusted(suggestion) => {
                    if suggestion.max_shares == 0 {
                        let reason =
                            format!("risk-gate suggested max_shares=0: {}", suggestion.reason);
                        self.reject_order_intent(
                            &evaluated,
                            OrderStatus::Rejected,
                            "BLOCKED",
                            reason,
                            adjusted.clone(),
                            "order blocked after risk adjustment",
                        )
                        .await;
                        return;
                    }

                    adjusted = Some((suggestion.max_shares, suggestion.reason.clone()));
                    evaluated.shares = suggestion.max_shares;
                    info!(
                        %agent_id, %intent_id,
                        attempt,
                        max_shares = suggestion.max_shares,
                        reason = %suggestion.reason,
                        "order adjusted by risk gate; re-evaluating"
                    );
                }
            }
        }

        let reason = "risk-gate adjustment loop exceeded max attempts".to_string();
        self.reject_order_intent(
            &evaluated,
            OrderStatus::Rejected,
            "BLOCKED",
            reason,
            adjusted,
            "order blocked",
        )
        .await;
    }

    pub(super) async fn check_governance_policy(&self, intent: &OrderIntent) -> Option<String> {
        let policy = self.governance.current_policy().await;
        let current_notional = self.current_account_notional().await;
        governance_block_reason(&policy, intent, current_notional)
    }

    pub(super) async fn current_account_notional(&self) -> Decimal {
        let platform_exposure = self.risk_gate.total_exposure().await;
        let (allocator_open, allocator_pending) = self.capital_policy.allocator_totals().await;
        let other_pending_buy_notional = self
            .order_queue
            .read()
            .await
            .pending_buy_notional_excluding_domains(&[
                Domain::Crypto,
                Domain::Sports,
                Domain::Politics,
                Domain::Economics,
            ]);

        let open_notional = platform_exposure.max(allocator_open);
        open_notional + allocator_pending + other_pending_buy_notional
    }

    pub(super) async fn reserve_domain_capital(&self, intent: &OrderIntent) -> Option<String> {
        self.capital_policy.reserve_buy(intent).await
    }

    pub(super) async fn release_domain_reservation(&self, intent_id: Uuid) {
        self.capital_policy.release_buy_reservation(intent_id).await;
    }
}
