use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use crate::coordinator::OrderIntent;
use crate::domain::{OrderRequest, OrderStatus};
use crate::strategy::executor::ExecutionResult;

use super::Coordinator;

impl Coordinator {
    pub(super) async fn settle_domain_success(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        fill_price: Decimal,
    ) {
        self.capital_policy
            .settle_success(intent, filled_shares, fill_price)
            .await;
    }

    pub(super) async fn settle_domain_failure(&self, intent: &OrderIntent) {
        self.capital_policy.settle_failure(intent).await;
    }

    pub(super) async fn refresh_risk_exposure_for_agent(&self, agent_id: &str) {
        let stats = self.positions.agent_stats(agent_id).await;
        self.risk_gate
            .update_agent_exposure(
                agent_id,
                stats.exposure,
                stats.unrealized_pnl,
                stats.position_count,
                stats.unhedged_count.min(u32::MAX as usize) as u32,
            )
            .await;
    }

    pub(super) async fn handle_execution_success(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: &ExecutionResult,
        queue_delay_ms: i64,
    ) {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        info!(
            %agent_id, %intent_id,
            order_id = %result.order_id,
            filled = result.filled_shares,
            "order executed successfully"
        );

        self.journal
            .persist_execution(
                self.executor.is_dry_run(),
                intent,
                request,
                Some(result),
                None,
                Some(queue_delay_ms),
            )
            .await;
        self.emit_execution_result_update(intent, result).await;

        let fill_price = result.avg_fill_price.unwrap_or(intent.limit_price);
        self.settle_domain_success(intent, result.filled_shares, fill_price)
            .await;

        let mut realized_pnl = Decimal::ZERO;
        if result.filled_shares > 0 {
            if intent.is_buy {
                let position_id = self
                    .positions
                    .open_position(
                        &agent_id,
                        intent.domain,
                        &intent.market_slug,
                        &intent.token_id,
                        intent.side,
                        result.filled_shares,
                        fill_price,
                    )
                    .await;
                debug!(
                    %agent_id,
                    %intent_id,
                    %position_id,
                    shares = result.filled_shares,
                    fill_price = %fill_price,
                    "tracked executed BUY position"
                );
            } else {
                realized_pnl = self
                    .apply_sell_fill_to_positions(intent, result.filled_shares, fill_price)
                    .await;
            }

            self.refresh_risk_exposure_for_agent(&agent_id).await;
        }

        if realized_pnl < Decimal::ZERO {
            self.risk_gate
                .record_success(&agent_id, Decimal::ZERO)
                .await;
            self.risk_gate
                .record_loss(&agent_id, realized_pnl.abs())
                .await;
        } else {
            self.risk_gate.record_success(&agent_id, realized_pnl).await;
        }
    }

    pub(super) async fn handle_execution_failure(
        &self,
        intent: &OrderIntent,
        request: &OrderRequest,
        error_message: String,
        queue_delay_ms: i64,
    ) {
        let agent_id = intent.agent_id.clone();
        let intent_id = intent.intent_id;

        error!(
            %agent_id, %intent_id,
            error = %error_message,
            "order execution failed"
        );

        self.journal
            .persist_execution(
                self.executor.is_dry_run(),
                intent,
                request,
                None,
                Some(error_message.clone()),
                Some(queue_delay_ms),
            )
            .await;
        self.emit_rejected_intent_update(intent, OrderStatus::Failed, error_message.clone())
            .await;

        self.risk_gate
            .record_failure(&agent_id, &error_message)
            .await;

        self.settle_domain_failure(intent).await;
    }

    pub(super) async fn apply_sell_fill_to_positions(
        &self,
        intent: &OrderIntent,
        filled_shares: u64,
        exit_price: Decimal,
    ) -> Decimal {
        if filled_shares == 0 {
            return Decimal::ZERO;
        }

        let mut remaining = filled_shares;
        let mut realized_pnl = Decimal::ZERO;
        let mut matching_positions = self
            .positions
            .get_agent_positions(&intent.agent_id)
            .await
            .into_iter()
            .filter(|pos| {
                pos.domain == intent.domain
                    && pos.market_slug == intent.market_slug
                    && pos.token_id == intent.token_id
                    && pos.side == intent.side
            })
            .collect::<Vec<_>>();

        matching_positions.sort_by_key(|p| p.entry_time);

        for pos in matching_positions {
            if remaining == 0 {
                break;
            }
            let reduce_by = remaining.min(pos.shares);
            if let Some(pnl) = self
                .positions
                .reduce_position(&pos.position_id, reduce_by, exit_price)
                .await
            {
                realized_pnl += pnl;
            }
            remaining -= reduce_by;
        }

        if remaining > 0 {
            warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                unmatched_shares = remaining,
                "sell fill exceeded tracked position shares; allocator adjusted, position book partially unmatched"
            );
        }

        realized_pnl
    }
}
