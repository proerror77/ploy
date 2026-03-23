//! Strategy factory and metadata for canonical strategy-manager entrypoints.

use super::{Result, Strategy};
use anyhow::anyhow;
use chrono::Utc;

/// Factory for creating strategy instances from configuration.
pub struct StrategyFactory;

impl StrategyFactory {
    /// Create a strategy from a TOML configuration string.
    pub fn from_toml(config_content: &str, dry_run: bool) -> Result<Box<dyn Strategy>> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_content).map_err(|e| anyhow!("Invalid TOML: {}", e))?;

        let strategy_section = config
            .get("strategy")
            .ok_or_else(|| anyhow!("Missing [strategy] section"))?;

        let strategy_name = strategy_section
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing strategy.name"))?;

        let strategy_id = format!("{}_{}", strategy_name, Utc::now().timestamp());

        match strategy_name {
            "momentum" => {
                let adapter = super::super::adapters::MomentumStrategyAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "split_arb" => {
                let adapter = super::super::adapters::SplitArbStrategyAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "pattern_memory" => {
                let strat = super::super::pattern_memory::PatternMemoryStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strat))
            }
            "staggered_arb" | "gamma_scalping" => {
                let adapter = super::super::staggered_arb_live::StaggeredArbAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "event_edge" => {
                let strategy = super::super::event_edge::strategy::EventEdgeStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strategy))
            }
            "crypto_lob_ml" => {
                let strategy =
                    super::super::crypto_lob_ml::strategy::CryptoLobMlStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            "crypto_rl_policy" => {
                let strategy =
                    super::super::crypto_rl_policy::strategy::CryptoRlPolicyStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            "nba_comeback" => {
                let strategy =
                    super::super::nba_comeback::strategy::NbaComebackStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            "weather_market" => {
                let strategy =
                    super::super::weather_market::strategy::WeatherMarketStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            "pm_5m_directional" => {
                let strategy =
                    super::super::pm_5m_directional::Pm5mDirectionalStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            other => Err(anyhow!("Unknown strategy type: {}", other).into()),
        }
    }

    /// Get list of available strategy types.
    pub fn available_strategies() -> Vec<StrategyInfo> {
        vec![
            StrategyInfo {
                name: "momentum".to_string(),
                description: "Trade crypto UP/DOWN based on CEX price momentum".to_string(),
                config_template: "momentum_default.toml".to_string(),
            },
            StrategyInfo {
                name: "split_arb".to_string(),
                description: "Split arbitrage when YES+NO prices < $1".to_string(),
                config_template: "split_arb_default.toml".to_string(),
            },
            StrategyInfo {
                name: "pattern_memory".to_string(),
                description: "Associative memory on Binance klines for 5m UP/DOWN".to_string(),
                config_template: "pattern_memory_default.toml".to_string(),
            },
            StrategyInfo {
                name: "staggered_arb".to_string(),
                description: "Time-staggered two-leg arb on crypto UP/DOWN binary options; aliases: gamma_scalping, staggered-arb"
                    .to_string(),
                config_template: "staggered_arb.toml".to_string(),
            },
            StrategyInfo {
                name: "event_edge".to_string(),
                description: "Politics/event-driven edge strategy on Polymarket events"
                    .to_string(),
                config_template: "event_edge.toml".to_string(),
            },
            StrategyInfo {
                name: "crypto_lob_ml".to_string(),
                description:
                    "Canonical observe-only crypto LOB ML wrapper for runtime migration"
                        .to_string(),
                config_template: "crypto_lob_ml.toml".to_string(),
            },
            StrategyInfo {
                name: "crypto_rl_policy".to_string(),
                description:
                    "Canonical observe-only crypto RL policy wrapper for runtime migration"
                        .to_string(),
                config_template: "crypto_rl_policy.toml".to_string(),
            },
            StrategyInfo {
                name: "nba_comeback".to_string(),
                description: "NBA comeback strategy using ESPN/game-state inputs".to_string(),
                config_template: "nba_comeback.toml".to_string(),
            },
            StrategyInfo {
                name: "weather_market".to_string(),
                description:
                    "Observe-only weather contract strategy using public station and forecast data"
                        .to_string(),
                config_template: "weather_market_default.toml".to_string(),
            },
            StrategyInfo {
                name: "pm_5m_directional".to_string(),
                description:
                    "Polymarket 5m directional engine using Binance direction and Polymarket cost gates"
                        .to_string(),
                config_template: "pm_5m_directional_default.toml".to_string(),
            },
        ]
    }
}

/// Information about an available strategy type.
#[derive(Debug, Clone)]
pub struct StrategyInfo {
    /// Strategy name/type.
    pub name: String,
    /// Description.
    pub description: String,
    /// Default config template filename.
    pub config_template: String,
}

#[cfg(test)]
mod tests {
    use super::StrategyFactory;

    #[test]
    fn test_available_strategies() {
        let strategies = StrategyFactory::available_strategies();
        assert!(!strategies.is_empty());
        assert!(strategies.iter().any(|s| s.name == "momentum"));
        assert!(strategies.iter().any(|s| s.name == "event_edge"));
        assert!(strategies.iter().any(|s| s.name == "nba_comeback"));
        assert!(strategies.iter().any(|s| s.name == "weather_market"));
    }
}
