use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::coordinator::governance::{GovernanceIntentSnapshot, IngressMode};
use crate::coordinator::position::PositionAggregator;
use crate::coordinator::queue::OrderQueue;
use crate::coordinator::OrderIntent;
use crate::domain::Domain;
use crate::error::{PloyError, Result};

use super::super::admission::{
    buy_intent_missing_deployment_reason, sell_reduce_only_violation_reason,
};
use super::{Coordinator, CoordinatorHandle};

struct IngressPreflightRejection {
    handle_reason: String,
    runtime_reason: String,
    runtime_log_message: &'static str,
}

fn runtime_domain_allowlist_reason(
    allowed_domains: &HashSet<Domain>,
    intent: &OrderIntent,
) -> Option<String> {
    (!allowed_domains.contains(&intent.domain))
        .then(|| format!("domain {} is not enabled for this runtime", intent.domain))
}

async fn reduce_only_violation(
    positions: &Arc<PositionAggregator>,
    order_queue: &Arc<RwLock<OrderQueue>>,
    intent: &OrderIntent,
) -> Option<String> {
    if intent.is_buy {
        return None;
    }

    let tracked_open_shares = positions
        .agent_open_shares_for_token_side(
            &intent.agent_id,
            intent.domain,
            &intent.token_id,
            intent.side,
        )
        .await;
    let pending_sell_shares = order_queue.read().await.pending_sell_shares_for(
        &intent.agent_id,
        intent.domain,
        &intent.token_id,
        intent.side,
    );

    sell_reduce_only_violation_reason(intent, tracked_open_shares, pending_sell_shares)
}

fn handle_buy_ingress_block_reason(
    snapshot: &GovernanceIntentSnapshot,
    intent: &OrderIntent,
) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    if snapshot.global_mode != IngressMode::Running {
        return Some(format!(
            "coordinator global ingress is {:?}; new intents are blocked",
            snapshot.global_mode
        ));
    }

    if snapshot.domain_mode != IngressMode::Running {
        return Some(format!(
            "coordinator {:?} ingress is {:?}; new intents are blocked",
            intent.domain, snapshot.domain_mode
        ));
    }

    None
}

fn runtime_buy_ingress_block_reason(
    snapshot: &GovernanceIntentSnapshot,
    intent: &OrderIntent,
) -> Option<(String, &'static str)> {
    if !intent.is_buy {
        return None;
    }

    if snapshot.global_mode != IngressMode::Running {
        return Some((
            format!(
                "Coordinator ingress is {:?}; blocking BUY intent while paused/halted",
                snapshot.global_mode
            ),
            "order blocked by coordinator ingress state",
        ));
    }

    if snapshot.domain_mode != IngressMode::Running {
        return Some((
            format!(
                "Domain {:?} ingress is {:?}; blocking BUY intent while paused/halted",
                intent.domain, snapshot.domain_mode
            ),
            "order blocked by coordinator domain ingress state",
        ));
    }

    None
}

async fn shared_ingress_preflight(
    allowed_domains: &HashSet<Domain>,
    positions: &Arc<PositionAggregator>,
    order_queue: &Arc<RwLock<OrderQueue>>,
    intent: &OrderIntent,
    governance_snapshot: Option<&GovernanceIntentSnapshot>,
) -> Option<IngressPreflightRejection> {
    if let Some(reason) = runtime_domain_allowlist_reason(allowed_domains, intent) {
        return Some(IngressPreflightRejection {
            handle_reason: reason.clone(),
            runtime_reason: reason,
            runtime_log_message: "order blocked by runtime domain allowlist",
        });
    }

    if let Some(reason) = buy_intent_missing_deployment_reason(intent) {
        return Some(IngressPreflightRejection {
            handle_reason: reason.clone(),
            runtime_reason: reason,
            runtime_log_message: "order blocked due to missing deployment identity",
        });
    }

    if let Some(reason) = reduce_only_violation(positions, order_queue, intent).await {
        return Some(IngressPreflightRejection {
            handle_reason: reason.clone(),
            runtime_reason: reason,
            runtime_log_message: "order blocked by reduce-only sell guard",
        });
    }

    if let Some(snapshot) = governance_snapshot {
        if let Some((runtime_reason, runtime_log_message)) =
            runtime_buy_ingress_block_reason(snapshot, intent)
        {
            let handle_reason = handle_buy_ingress_block_reason(snapshot, intent)
                .unwrap_or_else(|| runtime_reason.clone());
            return Some(IngressPreflightRejection {
                handle_reason,
                runtime_reason,
                runtime_log_message,
            });
        }
    }

    None
}

impl CoordinatorHandle {
    pub(super) async fn validate_submit_order_intent(&self, intent: &OrderIntent) -> Result<()> {
        let governance_snapshot = self.governance.intent_snapshot(intent).await;
        if let Some(rejection) = shared_ingress_preflight(
            &self.allowed_domains,
            &self.positions,
            &self.order_queue,
            intent,
            Some(&governance_snapshot),
        )
        .await
        {
            return Err(PloyError::Validation(rejection.handle_reason));
        }

        Ok(())
    }
}

impl Coordinator {
    pub(super) async fn validate_runtime_order_intent(
        &self,
        intent: &OrderIntent,
        governance_snapshot: Option<&GovernanceIntentSnapshot>,
    ) -> Option<(String, &'static str)> {
        shared_ingress_preflight(
            &self.allowed_domains,
            &self.positions,
            &self.order_queue,
            intent,
            governance_snapshot,
        )
        .await
        .map(|rejection| (rejection.runtime_reason, rejection.runtime_log_message))
    }
}
