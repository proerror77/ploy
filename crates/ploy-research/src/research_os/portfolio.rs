use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioInput {
    pub factors: Vec<PortfolioFactor>,
    #[serde(default)]
    pub pairwise_correlations: Vec<PairwiseCorrelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioFactor {
    pub factor_name: String,
    pub dsl_hash: String,
    pub reward: f64,
    pub ic: f64,
    pub icir: f64,
    pub test_pnl: Option<f64>,
    pub top_bucket_avg_label: Option<f64>,
    pub turnover_proxy: f64,
    pub top_bucket_full_depth_entry_fill_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseCorrelation {
    pub lhs: String,
    pub rhs: String,
    pub correlation: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortfolioOutput {
    pub selected_factors: Vec<SelectedFactor>,
    pub rejected_factors: Vec<RejectedFactor>,
    pub aggregate_expected_reward: f64,
    pub max_pairwise_correlation: f64,
    pub promotion_decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectedFactor {
    pub factor_name: String,
    pub dsl_hash: String,
    pub marginal_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectedFactor {
    pub factor_name: String,
    pub dsl_hash: String,
    pub reason: String,
}

pub fn build_factor_portfolio(input: &PortfolioInput) -> PortfolioOutput {
    let correlations = correlation_map(&input.pairwise_correlations);
    let mut candidates = input.factors.clone();
    candidates.sort_by(|lhs, rhs| rhs.reward.total_cmp(&lhs.reward));

    let mut selected: Vec<SelectedFactor> = Vec::new();
    let mut rejected = Vec::new();
    let mut aggregate_expected_reward = 0.0;
    let mut max_pairwise_correlation = 0.0_f64;

    for factor in candidates {
        if factor.top_bucket_full_depth_entry_fill_rate < 0.30 {
            rejected.push(rejection(&factor, "low_full_depth_entry_fill_rate"));
            continue;
        }

        let max_corr_existing = selected
            .iter()
            .map(|item| correlation_between(&correlations, &factor.dsl_hash, &item.dsl_hash).abs())
            .fold(0.0_f64, f64::max);
        if max_corr_existing >= 0.70 {
            rejected.push(rejection(&factor, "high_correlation_existing"));
            continue;
        }

        let marginal_score = factor.reward
            + factor.test_pnl.unwrap_or(0.0) * 0.10
            + factor.top_bucket_avg_label.unwrap_or(0.0) * 0.25
            + factor.icir.max(0.0) * 0.05
            - factor.turnover_proxy.max(0.0) * 0.10
            - (1.0 - factor.top_bucket_full_depth_entry_fill_rate).max(0.0) * 0.25;
        if marginal_score <= 0.0 {
            rejected.push(rejection(&factor, "non_positive_marginal_score"));
            continue;
        }

        max_pairwise_correlation = max_pairwise_correlation.max(max_corr_existing);
        aggregate_expected_reward += marginal_score;
        selected.push(SelectedFactor {
            factor_name: factor.factor_name,
            dsl_hash: factor.dsl_hash,
            marginal_score,
        });
    }

    let promotion_decision = if selected.len() >= 2 && aggregate_expected_reward > 0.0 {
        "portfolio_candidate"
    } else if selected.is_empty() {
        "revise"
    } else {
        "continue"
    };

    PortfolioOutput {
        selected_factors: selected,
        rejected_factors: rejected,
        aggregate_expected_reward,
        max_pairwise_correlation,
        promotion_decision: promotion_decision.to_string(),
    }
}

fn correlation_map(items: &[PairwiseCorrelation]) -> BTreeMap<(String, String), f64> {
    let mut out = BTreeMap::new();
    for item in items {
        out.insert(pair_key(&item.lhs, &item.rhs), item.correlation);
    }
    out
}

fn correlation_between(map: &BTreeMap<(String, String), f64>, lhs: &str, rhs: &str) -> f64 {
    map.get(&pair_key(lhs, rhs)).copied().unwrap_or(0.0)
}

fn pair_key(lhs: &str, rhs: &str) -> (String, String) {
    if lhs <= rhs {
        (lhs.to_string(), rhs.to_string())
    } else {
        (rhs.to_string(), lhs.to_string())
    }
}

fn rejection(factor: &PortfolioFactor, reason: &str) -> RejectedFactor {
    RejectedFactor {
        factor_name: factor.factor_name.clone(),
        dsl_hash: factor.dsl_hash.clone(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factor(name: &str, hash: &str, reward: f64) -> PortfolioFactor {
        PortfolioFactor {
            factor_name: name.to_string(),
            dsl_hash: hash.to_string(),
            reward,
            ic: 0.05,
            icir: 0.8,
            test_pnl: Some(1.0),
            top_bucket_avg_label: Some(0.5),
            turnover_proxy: 0.2,
            top_bucket_full_depth_entry_fill_rate: 0.8,
        }
    }

    #[test]
    fn portfolio_rejects_correlated_and_low_fill_candidates() {
        let output = build_factor_portfolio(&PortfolioInput {
            factors: vec![
                factor("a", "hash-a", 2.0),
                factor("b", "hash-b", 1.8),
                PortfolioFactor {
                    top_bucket_full_depth_entry_fill_rate: 0.1,
                    ..factor("c", "hash-c", 1.7)
                },
            ],
            pairwise_correlations: vec![PairwiseCorrelation {
                lhs: "hash-a".to_string(),
                rhs: "hash-b".to_string(),
                correlation: 0.8,
            }],
        });

        assert_eq!(output.selected_factors.len(), 1);
        assert_eq!(
            output.rejected_factors[0].reason,
            "high_correlation_existing"
        );
        assert_eq!(
            output.rejected_factors[1].reason,
            "low_full_depth_entry_fill_rate"
        );
        assert_eq!(output.promotion_decision, "continue");
    }

    #[test]
    fn portfolio_candidate_requires_multiple_selected_factors() {
        let output = build_factor_portfolio(&PortfolioInput {
            factors: vec![factor("a", "hash-a", 2.0), factor("b", "hash-b", 1.5)],
            pairwise_correlations: vec![PairwiseCorrelation {
                lhs: "hash-a".to_string(),
                rhs: "hash-b".to_string(),
                correlation: 0.2,
            }],
        });

        assert_eq!(output.selected_factors.len(), 2);
        assert_eq!(output.promotion_decision, "portfolio_candidate");
        assert_eq!(output.max_pairwise_correlation, 0.2);
    }
}
