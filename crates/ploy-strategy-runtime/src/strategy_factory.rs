use ploy_strategy_bundles::{
    BayesianDirectionalStrategy, FullConfig, MeanReversionStrategy, ReversalStrategy, StrategyLogic,
};
use tracing::info;

pub(crate) fn build_strategy(config: &FullConfig) -> Box<dyn StrategyLogic> {
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

            Box::new(ploy_strategy_bundles::DirectionalStrategy::new(
                config.strategy.clone(),
            ))
        }
        "directional_bayes" => {
            info!(
                configured_variant = configured_variant,
                canonical_variant = canonical_variant.as_str(),
                "Using Bayesian directional strategy variant",
            );
            let json = serde_json::to_value(&config.strategy).expect("serialize DirectionalConfig");
            let bayes_config: ploy_strategy_bundles::strategies::directional_bayes::BayesianDirectionalConfig =
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
            Box::new(ploy_strategy_bundles::ThreeLayerStrategy::new(
                ploy_strategy_bundles::strategies::three_layer::ThreeLayerConfig::from(
                    config.strategy.clone(),
                ),
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
    use super::build_strategy;
    use ploy_strategy_bundles::FullConfig;

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
