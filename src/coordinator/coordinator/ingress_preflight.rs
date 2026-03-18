use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::coordinator::governance::{GovernanceController, IngressMode};
use crate::coordinator::position::PositionAggregator;
use crate::coordinator::queue::OrderQueue;
use crate::coordinator::OrderIntent;
use crate::domain::Domain;
use crate::error::{PloyError, Result};

use super::super::admission::{
    buy_intent_missing_deployment_reason, sell_reduce_only_violation_reason,
};
use super::{Coordinator, CoordinatorHandle};

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

async fn handle_buy_ingress_block_reason(
    governance: &Arc<GovernanceController>,
    intent: &OrderIntent,
) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    let (global_mode, domain_mode) = governance.ingress_modes(intent.domain).await;

    if global_mode != IngressMode::Running {
        return Some(format!(
            "coordinator global ingress is {:?}; new intents are blocked",
            global_mode
        ));
    }

    if domain_mode != IngressMode::Running {
        return Some(format!(
            "coordinator {:?} ingress is {:?}; new intents are blocked",
            intent.domain, domain_mode
        ));
    }

    None
}

async fn runtime_buy_ingress_block_reason(
    governance: &Arc<GovernanceController>,
    intent: &OrderIntent,
) -> Option<(String, &'static str)> {
    if !intent.is_buy {
        return None;
    }

    let (ingress_mode, domain_mode) = governance.ingress_modes(intent.domain).await;
    if ingress_mode != IngressMode::Running {
        return Some((
            format!(
                "Coordinator ingress is {:?}; blocking BUY intent while paused/halted",
                ingress_mode
            ),
            "order blocked by coordinator ingress state",
        ));
    }

    if domain_mode != IngressMode::Running {
        return Some((
            format!(
                "Domain {:?} ingress is {:?}; blocking BUY intent while paused/halted",
                intent.domain, domain_mode
            ),
            "order blocked by coordinator domain ingress state",
        ));
    }

    None
}

impl CoordinatorHandle {
    pub(super) async fn validate_submit_order_intent(&self, intent: &OrderIntent) -> Result<()> {
        if let Some(reason) = runtime_domain_allowlist_reason(&self.allowed_domains, intent) {
            return Err(PloyError::Validation(reason));
        }

        if let Some(reason) = buy_intent_missing_deployment_reason(intent) {
            return Err(PloyError::Validation(reason));
        }

        if let Some(reason) =
            reduce_only_violation(&self.positions, &self.order_queue, intent).await
        {
            return Err(PloyError::Validation(reason));
        }

        if let Some(reason) = handle_buy_ingress_block_reason(&self.governance, intent).await {
            return Err(PloyError::Validation(reason));
        }

        Ok(())
    }
}

impl Coordinator {
    pub(super) async fn validate_runtime_order_intent(
        &self,
        intent: &OrderIntent,
    ) -> Option<(String, &'static str)> {
        if let Some(reason) = runtime_domain_allowlist_reason(&self.allowed_domains, intent) {
            return Some((reason, "order blocked by runtime domain allowlist"));
        }

        if let Some(reason) = buy_intent_missing_deployment_reason(intent) {
            return Some((reason, "order blocked due to missing deployment identity"));
        }

        if let Some(reason) =
            reduce_only_violation(&self.positions, &self.order_queue, intent).await
        {
            return Some((reason, "order blocked by reduce-only sell guard"));
        }

        runtime_buy_ingress_block_reason(&self.governance, intent).await
    }
}
