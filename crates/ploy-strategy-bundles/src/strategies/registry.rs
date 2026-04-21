use crate::config::FullConfig;
use crate::traits::StrategyLogic;
use crate::{
    BayesianDirectionalStrategy, DirectionalStrategy, MeanReversionStrategy, ReversalStrategy,
    ThreeLayerStrategy,
};
use tracing::info;

#[must_use]
pub fn canonical_strategy_variant(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "directional" | "v1" | "v2" | "v3" | "pm5d_v1" | "pm5d_v2" | "pm5d_v3" => {
            "directional".to_string()
        }
        "directional_bayes" | "directional-bayes" | "pm5d_bayes" => "directional_bayes".to_string(),
        "mean_reversion" | "mean-reversion" | "pm5d_v4" | "v4" => "mean_reversion".to_string(),
        "reversal" | "pm5d_reversal" | "pm-5m-reversal" => "reversal".to_string(),
        "three_layer" | "three-layer" | "threelayer" => "three_layer".to_string(),
        "diff_enhanced" | "diff-enhanced" | "s1" | "s1_enhanced" => "diff_enhanced".to_string(),
        "diff_regular" | "diff-regular" | "s2" | "s2_regular" => "diff_regular".to_string(),
        "sweep" | "s3" | "s3_sweep" => "sweep".to_string(),
        "prob_reversal" | "prob-reversal" | "s4" | "s4_reversal" => "prob_reversal".to_string(),
        "prob_chase" | "prob-chase" | "s5" | "s5_prob_chase" => "prob_chase".to_string(),
        other => other.to_string(),
    }
}

pub fn build_strategy(config: &FullConfig) -> Box<dyn StrategyLogic> {
    let configured_variant = config.runtime.strategy_variant.trim();
    let canonical_variant = config.runtime.canonical_strategy_variant();

    match canonical_variant.as_str() {
        "directional" => {
            if configured_variant != "directional" {
                info!(
                    configured_variant = configured_variant,
                    canonical_variant = canonical_variant.as_str(),
                    "Using roadmap alias for directional strategy variant",
                );
            }

            Box::new(DirectionalStrategy::new(config.strategy.clone()))
        }
        "directional_bayes" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using Bayesian directional strategy variant",
            );
            let json = serde_json::to_value(&config.strategy).expect("serialize DirectionalConfig");
            let bayes_config: crate::strategies::directional_bayes::BayesianDirectionalConfig =
                serde_json::from_value(json).expect("deserialize BayesianDirectionalConfig");
            Box::new(BayesianDirectionalStrategy::new(bayes_config))
        }
        "mean_reversion" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using mean-reversion strategy variant",
            );
            Box::new(MeanReversionStrategy::new(config.strategy.clone()))
        }
        "reversal" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using reversal strategy variant",
            );
            Box::new(ReversalStrategy::new(config.strategy.clone().into()))
        }
        "three_layer" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using three-layer directional strategy variant",
            );
            Box::new(ThreeLayerStrategy::new(
                crate::strategies::three_layer::ThreeLayerConfig::from(config.strategy.clone()),
            ))
        }
        _ => {
            eprintln!(
                "Unsupported strategy_variant `{configured_variant}` in config. \
                 Supported runtime variants: directional, directional_bayes, mean_reversion, reversal, three_layer, v1, v2, v3, v4."
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_strategy, canonical_strategy_variant};
    use crate::FullConfig;

    #[test]
    fn canonical_strategy_variant_normalizes_roadmap_aliases() {
        for (raw, expected) in [
            ("directional", "directional"),
            ("v1", "directional"),
            ("v2", "directional"),
            ("v3", "directional"),
            ("directional_bayes", "directional_bayes"),
            ("v4", "mean_reversion"),
            ("pm5d_v4", "mean_reversion"),
            ("reversal", "reversal"),
            ("pm5d_reversal", "reversal"),
        ] {
            assert_eq!(canonical_strategy_variant(raw), expected);
        }
    }

    #[test]
    fn roadmap_aliases_build_expected_strategy_variants() {
        for (variant, expected_name) in [
            ("v1", "pm_5m_directional"),
            ("v2", "pm_5m_directional"),
            ("v3", "pm_5m_directional"),
            ("v4", "pm_5m_mean_reversion"),
            ("reversal", "pm_5m_reversal"),
            ("three_layer", "three_layer"),
            ("three-layer", "three_layer"),
        ] {
            let config = FullConfig::from_toml(&format!(
                r#"
[runtime]
mode = "dryrun"
strategy_variant = "{variant}"

[strategy]
"#
            ))
            .unwrap();

            let strategy = build_strategy(&config);
            assert_eq!(strategy.name(), expected_name);
        }
    }
}
