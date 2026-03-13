#[cfg(test)]
use rust_decimal_macros::dec;

mod analysis;
mod monitor;

pub use analysis::{
    ExpectedValue, MarketMakingAction, MarketMakingConfig, MarketMakingOpportunity,
    NearSettlementAnalysis, POLYMARKET_FEE_RATE, RiskLevel, SplitMergeOpportunity,
    SplitMergeType, analyze_market_making_opportunity, analyze_near_settlement,
    detect_split_merge_opportunity, generate_ev_table,
};
pub use monitor::{
    ArbitrageType, MultiOutcomeArbitrage, MultiOutcomeMonitor, Outcome, OutcomeDirection,
    OutcomeSummary, fetch_multi_outcome_event,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price_level() {
        assert_eq!(Outcome::parse_price_level("↑ 94,000"), Some(dec!(94000)));
        assert_eq!(Outcome::parse_price_level("↓ 86,000"), Some(dec!(86000)));
        assert_eq!(Outcome::parse_price_level("↑ 104,000"), Some(dec!(104000)));
    }

    #[test]
    fn test_direction_parsing() {
        assert_eq!(
            OutcomeDirection::from_symbol("↑ 94,000"),
            Some(OutcomeDirection::Up)
        );
        assert_eq!(
            OutcomeDirection::from_symbol("↓ 86,000"),
            Some(OutcomeDirection::Down)
        );
    }

    #[test]
    fn test_monotonicity_detection() {
        let mut monitor = MultiOutcomeMonitor::new("test", "BTC Price Test");

        monitor.add_outcome("token1".to_string(), "↓ 86,000".to_string());
        monitor.add_outcome("token2".to_string(), "↓ 84,000".to_string());
        monitor.add_outcome("token3".to_string(), "↓ 82,000".to_string());
        monitor.add_outcome("token4".to_string(), "↓ 80,000".to_string());

        monitor.update_quote("token1", Some(dec!(0.24)), None, None, None);
        monitor.update_quote("token2", Some(dec!(0.049)), None, None, None);
        monitor.update_quote("token3", Some(dec!(0.012)), None, None, None);
        monitor.update_quote("token4", Some(dec!(0.013)), None, None, None);

        let violations = monitor.find_monotonicity_violations();
        assert!(
            !violations.is_empty(),
            "Should detect monotonicity violation"
        );
    }
}
