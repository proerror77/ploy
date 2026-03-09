use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, warn};

use super::{
    AdjustmentSuggestion, BlockReason, OrderIntent, OrderPriority, RiskCheckResult, RiskGate,
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

        let platform_state = *self.state.read().await;
        if !platform_state.can_trade() {
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

        let params = match self.load_agent_params(intent).await {
            Ok(params) => params,
            Err(result) => return result,
        };

        if !params.is_market_allowed(&intent.market_slug) {
            return RiskCheckResult::Blocked(BlockReason::MarketNotAllowed {
                market: intent.market_slug.clone(),
                agent: intent.agent_id.clone(),
            });
        }

        let order_value = intent.notional_value();

        if let Some(result) = self.check_single_order_limit(intent, order_value, &params) {
            return result;
        }

        let current_agent_exposure = self.current_agent_exposure(&intent.agent_id).await;
        if current_agent_exposure + order_value > params.max_total_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: params.max_total_exposure,
                current: current_agent_exposure,
                requested: order_value,
            });
        }

        if let Some(result) = self.check_domain_exposure_limit(intent, order_value).await {
            return result;
        }

        let current_platform_exposure = *self.total_exposure.read().await;
        if current_platform_exposure + order_value > self.config.max_platform_exposure {
            return RiskCheckResult::Blocked(BlockReason::ExceedsTotalExposure {
                limit: self.config.max_platform_exposure,
                current: current_platform_exposure,
                requested: order_value,
            });
        }

        if !platform_state.can_open_new() {
            debug!(
                "Elevated state: allowing buy order {} with extra scrutiny",
                intent.intent_id
            );
        }

        if let Some(result) = self.check_daily_loss_limits(intent).await {
            return result;
        }

        if let Some(limit) = self.config.max_drawdown_limit {
            let current_drawdown = self.drawdown_stats.read().await.current_drawdown;
            if limit > Decimal::ZERO && current_drawdown >= limit {
                return RiskCheckResult::Blocked(BlockReason::DrawdownExceeded {
                    limit,
                    current: current_drawdown,
                });
            }
        }

        RiskCheckResult::Passed
    }

    async fn load_agent_params(
        &self,
        intent: &OrderIntent,
    ) -> Result<crate::agent_runtime::AgentRiskParams, RiskCheckResult> {
        let params_map = self.agent_params.read().await;
        match params_map.get(&intent.agent_id) {
            Some(params) => Ok(params.clone()),
            None => {
                warn!(
                    "No risk params for agent {}, blocking order",
                    intent.agent_id
                );
                Err(RiskCheckResult::Blocked(BlockReason::UnregisteredAgent {
                    agent: intent.agent_id.clone(),
                }))
            }
        }
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

    async fn current_agent_exposure(&self, agent_id: &str) -> Decimal {
        let stats_map = self.agent_stats.read().await;
        stats_map
            .get(agent_id)
            .map(|stats| stats.exposure)
            .unwrap_or(Decimal::ZERO)
    }

    async fn check_domain_exposure_limit(
        &self,
        intent: &OrderIntent,
        order_value: Decimal,
    ) -> Option<RiskCheckResult> {
        let domain_limit = self.config.domain_exposure_limit(intent.domain)?;
        let current_domain_exposure = self
            .domain_exposure
            .read()
            .await
            .get(&intent.domain)
            .copied()
            .unwrap_or(Decimal::ZERO);

        if current_domain_exposure + order_value > domain_limit {
            return Some(RiskCheckResult::Blocked(
                BlockReason::DomainExposureExceeded {
                    domain: intent.domain,
                    limit: domain_limit,
                    current: current_domain_exposure,
                    requested: order_value,
                },
            ));
        }

        None
    }

    async fn check_daily_loss_limits(&self, intent: &OrderIntent) -> Option<RiskCheckResult> {
        let daily = self.daily_stats.read().await;
        if daily.total_pnl < Decimal::ZERO && daily.total_pnl.abs() >= self.config.daily_loss_limit
        {
            return Some(RiskCheckResult::Blocked(BlockReason::DailyLossExceeded {
                limit: self.config.daily_loss_limit,
                current: daily.total_pnl.abs(),
            }));
        }

        let domain_loss_limit = self.config.domain_daily_loss_limit(intent.domain)?;
        let domain_pnl = daily
            .domain_pnl
            .get(&intent.domain)
            .copied()
            .unwrap_or(Decimal::ZERO);
        if domain_pnl < Decimal::ZERO && domain_pnl.abs() >= domain_loss_limit {
            return Some(RiskCheckResult::Blocked(
                BlockReason::DomainDailyLossExceeded {
                    domain: intent.domain,
                    limit: domain_loss_limit,
                    current: domain_pnl.abs(),
                },
            ));
        }

        None
    }
}
