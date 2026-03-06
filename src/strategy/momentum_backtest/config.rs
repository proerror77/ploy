use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::strategy::momentum::MomentumConfig;

/// Configuration for a momentum backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumBacktestConfig {
    pub momentum_config: MomentumConfig,
    pub symbols: Vec<String>,
    pub initial_capital: Decimal,
    pub max_concurrent_positions: usize,
    pub cooldown_secs: u64,
}

impl MomentumBacktestConfig {
    pub fn default_with_symbols(symbols: Vec<String>, initial_capital: Decimal) -> Self {
        let mut momentum_config = MomentumConfig::default();
        momentum_config.symbols = symbols.clone();
        Self {
            momentum_config,
            symbols,
            initial_capital,
            max_concurrent_positions: 5,
            cooldown_secs: 30,
        }
    }

    /// SHA-256 hash of the serialized config for deduplication.
    pub fn config_hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let json = serde_json::to_string(self).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        json.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::MomentumBacktestConfig;

    #[test]
    fn default_with_symbols_keeps_top_level_and_nested_symbols_aligned() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let config = MomentumBacktestConfig::default_with_symbols(symbols.clone(), dec!(2500));

        assert_eq!(config.symbols, symbols);
        assert_eq!(config.momentum_config.symbols, symbols);
        assert_eq!(config.initial_capital, dec!(2500));
        assert_eq!(config.max_concurrent_positions, 5);
        assert_eq!(config.cooldown_secs, 30);
    }
}
