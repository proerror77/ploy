use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use tokio::join;
use tracing::{debug, warn};

use super::{
    AdjustmentSuggestion, BlockReason, OrderIntent, OrderPriority, RiskCheckResult, RiskGate,
    RiskOrderSnapshot,
};

impl RiskGate {
    /// 檢查訂單是否可以執行
    ///
    /// 這是主要的風控入口點，會依序執行多層風控檢查。
    pub async fn check_order(&self, intent: &OrderIntent) -> RiskCheckResult {
        self.try_auto_recover_circuit_breaker().await;

        if intent.is_expired() {
            return RiskCheckResult::Blocked(BlockReason::OrderExpired);
        }

        // Binary-options semantics (Polymarket): SELL intents are reduce-only exits.
        // They must stay allowed during circuit-breaker, daily-loss, and exposure limits.
        if !intent.is_buy {
            return RiskCheckResult::Passed;
        }

        let snapshot = match self.load_order_snapshot(intent).await {
            Ok(snapshot) => snapshot,
            Err(result) => return result,
        };

        if !snapshot.platform_state.can_trade() {
            return RiskCheckResult::Blocked(BlockReason::CircuitBreakerTripped {
                reason: "Platform trading halted".to_string(),
            });
        }

        if intent.priority == OrderPriority::Critical && self.config.critical_bypass_exposure {
            warn!(
                "critical_bypass_exposure is enabled for intent {} but is ignored by policy",
                intent.intent_id
            );
        }

        if !snapshot.params.is_market_allowed(&intent.market_slug) {
            return RiskCheckResult::Blocked(BlockReason::MarketNotAllowed {
                market: intent.market_slug.clone(),
                agent: intent.agent_id.clone(),
            });
        }

        let order_value = intent.notional_value();

        if let Some(result) = self.check_single_order_limit(intent, order_value, &snapshot.params) {
            return result;
        }

        if snapshot.current_agent_exposure + order_value > snapshot.params.max_total_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: snapshot.params.max_total_exposure,
                current: snapshot.current_agent_exposure,
                requested: order_value,
            });
        }

        if let Some(result) = self.check_domain_exposure_limit(intent, order_value, &snapshot) {
            return result;
        }

        if snapshot.current_platform_exposure + order_value > self.config.max_platform_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: self.config.max_platform_exposure,
                current: snapshot.current_platform_exposure,
                requested: order_value,
            });
        }

        if !snapshot.platform_state.can_open_new() {
            debug!(
                "Elevated state: allowing buy order {} with extra scrutiny",
                intent.intent_id
            );
        }

        if let Some(result) = self.check_daily_loss_limits(intent, &snapshot) {
            return result;
        }

        if let Some(limit) = self.config.max_drawdown_limit {
            if limit > Decimal::ZERO && snapshot.current_drawdown >= limit {
                return RiskCheckResult::Blocked(BlockReason::DrawdownExceeded {
                    limit,
                    current: snapshot.current_drawdown,
                });
            }
        }

        RiskCheckResult::Passed
    }

    async fn load_order_snapshot(
        &self,
        intent: &OrderIntent,
    ) -> Result<RiskOrderSnapshot, RiskCheckResult> {
        let (
            platform_state,
            params_map,
            stats_map,
            domain_exposure_map,
            platform_exposure,
            daily_stats,
            drawdown_stats,
        ) = join!(
            self.state.read(),
            self.agent_params.read(),
            self.agent_stats.read(),
            self.domain_exposure.read(),
            self.total_exposure.read(),
            self.daily_stats.read(),
            self.drawdown_stats.read(),
        );

        let Some(params) = params_map.get(&intent.agent_id).cloned() else {
            warn!(
                "No risk params for agent {}, blocking order",
                intent.agent_id
            );
            return Err(RiskCheckResult::Blocked(BlockReason::UnregisteredAgent {
                agent: intent.agent_id.clone(),
            }));
        };

        Ok(RiskOrderSnapshot {
            platform_state: *platform_state,
            params,
            current_agent_exposure: stats_map
                .get(&intent.agent_id)
                .map(|stats| stats.exposure)
                .unwrap_or(Decimal::ZERO),
            current_domain_exposure: domain_exposure_map
                .get(&intent.domain)
                .copied()
                .unwrap_or(Decimal::ZERO),
            current_platform_exposure: *platform_exposure,
            daily_total_pnl: daily_stats.total_pnl,
            domain_pnl: daily_stats
                .domain_pnl
                .get(&intent.domain)
                .copied()
                .unwrap_or(Decimal::ZERO),
            current_drawdown: drawdown_stats.current_drawdown,
        })
    }

    fn check_single_order_limit(
        &self,
        intent: &OrderIntent,
        order_value: Decimal,
        params: &crate::agent_runtime::AgentRiskParams,
    ) -> Option<RiskCheckResult> {
        if order_value <= params.max_order_value {
            return None;
        }

        let max_shares = (params.max_order_value / intent.limit_price)
            .to_u64()
            .unwrap_or(0);

        Some(if max_shares > 0 {
            RiskCheckResult::Adjusted(AdjustmentSuggestion {
                max_shares,
                reason: format!(
                    "Order value ${} exceeds agent limit ${}",
                    order_value, params.max_order_value
                ),
            })
        } else {
            RiskCheckResult::Blocked(BlockReason::ExceedsSingleLimit {
                limit: params.max_order_value,
                requested: order_value,
            })
        })
    }

    fn check_domain_exposure_limit(
        &self,
        intent: &OrderIntent,
        order_value: Decimal,
        snapshot: &RiskOrderSnapshot,
    ) -> Option<RiskCheckResult> {
        let domain_limit = self.config.domain_exposure_limit(intent.domain)?;

        if snapshot.current_domain_exposure + order_value > domain_limit {
            return Some(RiskCheckResult::Blocked(
                BlockReason::DomainExposureExceeded {
                    domain: intent.domain,
                    limit: domain_limit,
                    current: snapshot.current_domain_exposure,
                    requested: order_value,
                },
            ));
        }

        None
    }

    fn check_daily_loss_limits(
        &self,
        intent: &OrderIntent,
        snapshot: &RiskOrderSnapshot,
    ) -> Option<RiskCheckResult> {
        if snapshot.daily_total_pnl < Decimal::ZERO
            && snapshot.daily_total_pnl.abs() >= self.config.daily_loss_limit
        {
            return Some(RiskCheckResult::Blocked(BlockReason::DailyLossExceeded {
                limit: self.config.daily_loss_limit,
                current: snapshot.daily_total_pnl.abs(),
            }));
        }

        let domain_loss_limit = self.config.domain_daily_loss_limit(intent.domain)?;
        if snapshot.domain_pnl < Decimal::ZERO && snapshot.domain_pnl.abs() >= domain_loss_limit {
            return Some(RiskCheckResult::Blocked(
                BlockReason::DomainDailyLossExceeded {
                    domain: intent.domain,
                    limit: domain_loss_limit,
                    current: snapshot.domain_pnl.abs(),
                },
            ));
        }

        None
    }
}
