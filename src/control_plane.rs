mod deployments;
mod evaluation;
mod risk_decision;
mod trade_intent;

pub use deployments::{
    DeploymentExecutionMode, MarketSelector, StrategyDeployment, StrategyLifecycleStage,
    StrategyProductType, Timeframe,
};
pub use evaluation::{
    StrategyEvaluationEvidence, StrategyEvaluationMetrics, StrategyEvaluationStage,
};
pub use risk_decision::{RiskDecision, RiskDecisionStatus};
pub use trade_intent::TradeIntent;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::OrderPriority;
    use crate::domain::Side;
    use crate::platform::Domain;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn trade_intent_into_order_intent_maps_priority_and_metadata() {
        let intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: Some(dec!(0.73)),
            edge: Some(dec!(0.05)),
            event_time: None,
            reason: Some("signal_edge".to_string()),
            priority: Some("high".to_string()),
            metadata: HashMap::new(),
        };

        let mapped = intent.into_order_intent();
        assert_eq!(mapped.priority, OrderPriority::High);
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
        assert_eq!(
            mapped.metadata.get("intent_reason").map(String::as_str),
            Some("signal_edge")
        );
    }

    #[test]
    fn deployment_runtime_scope_matching() {
        let mut deployment = StrategyDeployment {
            id: "dep.crypto.5m".to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v1".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Dynamic {
                domain: Domain::Crypto,
                query: Some("BTC 5m".to_string()),
                min_liquidity_usd: None,
                max_spread_bps: None,
                min_time_remaining_secs: None,
                max_time_remaining_secs: None,
            },
            timeframe: Timeframe::M5,
            enabled: true,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 10,
            cooldown_secs: 30,
            account_ids: vec![" acct-a ".to_string(), "ACCT-A".to_string()],
            execution_mode: DeploymentExecutionMode::LiveOnly,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        };

        deployment.normalize_account_ids_in_place();
        assert_eq!(deployment.account_ids.len(), 1);
        assert!(deployment.matches_account("acct-a"));
        assert!(!deployment.matches_account("acct-b"));
        assert!(deployment.matches_execution_mode(false));
        assert!(!deployment.matches_execution_mode(true));
        assert!(deployment.is_enabled_for_runtime("acct-a", false));
        assert!(!deployment.is_enabled_for_runtime("acct-a", true));
    }

    #[test]
    fn trade_intent_into_order_intent_normalizes_blank_deployment_metadata() {
        let mut intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: None,
            edge: None,
            event_time: None,
            reason: None,
            priority: None,
            metadata: HashMap::new(),
        };
        intent
            .metadata
            .insert("deployment_id".to_string(), "   ".to_string());

        let mapped = intent.into_order_intent();
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
    }
}
