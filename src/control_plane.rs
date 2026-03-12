pub(crate) mod deployment_files;
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
    use crate::domain::Domain;

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
}
