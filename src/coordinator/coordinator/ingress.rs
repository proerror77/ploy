use super::*;
use crate::domain::OrderStatus;

impl Coordinator {
    /// Risk-check an incoming order intent and enqueue if passed
    pub(super) async fn handle_order_intent(&self, intent: OrderIntent) {
        let mut intent = intent;
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;
        let strategy_max_shares = intent.shares;

        if !self.is_domain_allowed(intent.domain) {
            let reason = format!("domain {} is not enabled for this runtime", intent.domain);
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by runtime domain allowlist"
            );
            return;
        }

        if let Some(reason) = buy_intent_missing_deployment_reason(&intent) {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked due to missing deployment identity"
            );
            return;
        }

        if !intent.is_buy {
            let tracked_open_shares = self
                .positions
                .agent_open_shares_for_token_side(
                    &intent.agent_id,
                    intent.domain,
                    &intent.token_id,
                    intent.side,
                )
                .await;
            let pending_sell_shares = self.order_queue.read().await.pending_sell_shares_for(
                &intent.agent_id,
                intent.domain,
                &intent.token_id,
                intent.side,
            );

            if let Some(reason) =
                sell_reduce_only_violation_reason(&intent, tracked_open_shares, pending_sell_shares)
            {
                self.journal
                    .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                    .await;
                self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                    .await;
                warn!(
                    %agent_id, %intent_id, reason = %reason,
                    "order blocked by reduce-only sell guard"
                );
                return;
            }
        }

        let (ingress_mode, domain_mode) = self.governance.ingress_modes(intent.domain).await;
        if intent.is_buy && ingress_mode != IngressMode::Running {
            let reason = format!(
                "Coordinator ingress is {:?}; blocking BUY intent while paused/halted",
                ingress_mode
            );
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator ingress state"
            );
            return;
        }

        if intent.is_buy && domain_mode != IngressMode::Running {
            let reason = format!(
                "Domain {:?} ingress is {:?}; blocking BUY intent while paused/halted",
                intent.domain, domain_mode
            );
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by coordinator domain ingress state"
            );
            return;
        }

        if intent.is_buy && self.governance.is_agent_paused(&intent.agent_id).await {
            let reason = format!("Agent {} is paused; blocking BUY intent", intent.agent_id);
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by per-agent pause"
            );
            return;
        }

        if let Some(reason) = self.check_governance_policy(&intent).await {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by global governance policy"
            );
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
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by deployment gate"
            );
            return;
        }

        self.journal.persist_signal_from_intent(&intent).await;
        if !intent.is_buy {
            self.journal.persist_exit_reason_intent(&intent).await;
        }

        if let Some(reason) = self.admission.check_duplicate_intent(&intent).await {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by duplicate-intent guard"
            );
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_kelly_sizing(&self.capital_policy, &mut intent)
            .await
        {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by kelly sizing policy"
            );
            return;
        }

        if let Some(reason) = self
            .admission
            .apply_min_order_constraints(&mut intent, strategy_max_shares)
        {
            self.journal
                .persist_risk_decision(&intent, "BLOCKED", Some(reason.clone()), None)
                .await;
            self.emit_rejected_intent_update(&intent, OrderStatus::Rejected, reason.clone())
                .await;
            warn!(
                %agent_id, %intent_id, reason = %reason,
                "order blocked by venue minimum constraints"
            );
            return;
        }

        let mut adjusted: Option<(u64, String)> = None;
        let mut evaluated = intent;
        for attempt in 0..3 {
            match self.risk_gate.check_order(&evaluated).await {
                RiskCheckResult::Passed => {
                    if let Some(reason) = self.reserve_domain_capital(&evaluated).await {
                        self.journal
                            .persist_risk_decision(
                                &evaluated,
                                "BLOCKED",
                                Some(reason.clone()),
                                adjusted.clone(),
                            )
                            .await;
                        self.emit_rejected_intent_update(
                            &evaluated,
                            OrderStatus::Rejected,
                            reason.clone(),
                        )
                        .await;
                        warn!(
                            %agent_id, %intent_id, reason = %reason,
                            "order blocked by domain allocator"
                        );
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
                        Err(e) => {
                            self.release_domain_reservation(intent_id).await;
                            self.emit_rejected_intent_update(
                                &evaluated,
                                OrderStatus::Failed,
                                format!("queue full, order dropped: {}", e),
                            )
                            .await;
                            warn!(%agent_id, %intent_id, error = %e, "queue full, order dropped");
                        }
                    }
                    return;
                }
                RiskCheckResult::Blocked(reason) => {
                    self.journal
                        .persist_risk_decision(
                            &evaluated,
                            "BLOCKED",
                            Some(reason.to_string()),
                            adjusted.clone(),
                        )
                        .await;
                    self.emit_rejected_intent_update(
                        &evaluated,
                        OrderStatus::Rejected,
                        reason.to_string(),
                    )
                    .await;
                    warn!(
                        %agent_id, %intent_id,
                        reason = ?reason,
                        "order blocked by risk gate"
                    );
                    return;
                }
                RiskCheckResult::Adjusted(suggestion) => {
                    if suggestion.max_shares == 0 {
                        let reason =
                            format!("risk-gate suggested max_shares=0: {}", suggestion.reason);
                        self.journal
                            .persist_risk_decision(
                                &evaluated,
                                "BLOCKED",
                                Some(reason.clone()),
                                adjusted.clone(),
                            )
                            .await;
                        self.emit_rejected_intent_update(
                            &evaluated,
                            OrderStatus::Rejected,
                            reason.clone(),
                        )
                        .await;
                        warn!(
                            %agent_id,
                            %intent_id,
                            reason = %reason,
                            "order blocked after risk adjustment"
                        );
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
        self.journal
            .persist_risk_decision(&evaluated, "BLOCKED", Some(reason.clone()), adjusted)
            .await;
        self.emit_rejected_intent_update(&evaluated, OrderStatus::Rejected, reason.clone())
            .await;
        warn!(%agent_id, %intent_id, reason = %reason, "order blocked");
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
