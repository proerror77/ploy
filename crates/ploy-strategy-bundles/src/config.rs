//! Unified TOML configuration for strategy runtime.
//!
//! A single config file drives backtest, dry-run, and live modes.
//! The `[runtime].mode` field selects which Feed and Executor are wired.

use rust_decimal::Decimal;
use serde::Deserialize;

use crate::engine::{RuntimeConfig, RuntimeMode};
use crate::executor::SimulatedExecutorConfig;
use crate::strategies::directional::DirectionalConfig;

/// Top-level config deserialized from a TOML file.
///
/// ```toml
/// [runtime]
/// mode = "dryrun"
/// throttle_hz = 1
///
/// [strategy]
/// symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
/// min_edge = 0.05
/// # ... (all DirectionalConfig fields)
///
/// [execution]
/// spread_pct = 0.02
/// enable_partial_fills = true
/// # ... (all SimulatedExecutorConfig fields)
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct FullConfig {
    pub runtime: RuntimeSection,
    pub strategy: DirectionalConfig,
    #[serde(default)]
    pub execution: SimExecutionSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSection {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub throttle_hz: Option<u32>,
    pub max_updates: Option<u64>,
}

fn default_mode() -> String {
    "dryrun".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimExecutionSection {
    #[serde(default = "default_true")]
    pub use_spread: bool,
    #[serde(default = "default_spread")]
    pub spread_pct: f64,
    #[serde(default = "default_true")]
    pub enable_partial_fills: bool,
    #[serde(default = "default_depth_multiple")]
    pub depth_multiple: f64,
    #[serde(default = "default_min_fill")]
    pub min_fill_pct: f64,
    #[serde(default = "default_true")]
    pub enable_market_impact: bool,
    #[serde(default = "default_impact")]
    pub impact_coefficient: f64,
    #[serde(default = "default_depth_shares")]
    pub default_depth_shares: u64,
}

fn default_true() -> bool { true }
fn default_spread() -> f64 { 0.02 }
fn default_depth_multiple() -> f64 { 5.0 }
fn default_min_fill() -> f64 { 0.5 }
fn default_impact() -> f64 { 0.1 }
fn default_depth_shares() -> u64 { 500 }

impl Default for SimExecutionSection {
    fn default() -> Self {
        Self {
            use_spread: true,
            spread_pct: 0.02,
            enable_partial_fills: true,
            depth_multiple: 5.0,
            min_fill_pct: 0.5,
            enable_market_impact: true,
            impact_coefficient: 0.1,
            default_depth_shares: 500,
        }
    }
}

impl FullConfig {
    /// Parse from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// Parse from a TOML file path.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::from_toml(&content)?)
    }

    /// Build RuntimeConfig from the parsed config.
    pub fn runtime_config(&self) -> RuntimeConfig {
        let mode = match self.runtime.mode.as_str() {
            "backtest" => RuntimeMode::Backtest,
            "live" => RuntimeMode::Live,
            _ => RuntimeMode::DryRun,
        };
        RuntimeConfig {
            mode,
            throttle_hz: self.runtime.throttle_hz,
            max_updates: self.runtime.max_updates,
        }
    }

    /// Build SimulatedExecutorConfig from the parsed config.
    pub fn sim_executor_config(&self) -> SimulatedExecutorConfig {
        let e = &self.execution;
        SimulatedExecutorConfig {
            use_spread: e.use_spread,
            spread_pct: Decimal::try_from(e.spread_pct).unwrap_or_default(),
            enable_partial_fills: e.enable_partial_fills,
            depth_multiple: Decimal::try_from(e.depth_multiple).unwrap_or_default(),
            min_fill_pct: Decimal::try_from(e.min_fill_pct).unwrap_or_default(),
            enable_market_impact: e.enable_market_impact,
            impact_coefficient: Decimal::try_from(e.impact_coefficient).unwrap_or_default(),
            default_depth_shares: e.default_depth_shares,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
[runtime]
mode = "backtest"
throttle_hz = 1
max_updates = 10000

[strategy]
symbols = ["BTCUSDT", "ETHUSDT"]
vol_floor = 0.001
min_probability = 0.62
min_z_score = 0.35
min_entry_price = 0.15
max_entry_price = 0.85
no_trade_zone_min = 0.45
no_trade_zone_max = 0.55
min_edge = 0.05
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 60
quantity = 25.0
max_positions = 3
max_daily_trades = 1000

[execution]
spread_pct = 0.02
enable_partial_fills = true
enable_market_impact = true
"#;

    #[test]
    fn parses_full_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(config.runtime.mode, "backtest");
        assert_eq!(config.runtime.throttle_hz, Some(1));
        assert_eq!(config.strategy.symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert!((config.strategy.min_edge - 0.05).abs() < 1e-10);
        assert_eq!(config.strategy.max_positions, 3);
    }

    #[test]
    fn builds_runtime_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        let rc = config.runtime_config();
        assert_eq!(rc.mode, RuntimeMode::Backtest);
        assert_eq!(rc.throttle_hz, Some(1));
        assert_eq!(rc.max_updates, Some(10000));
    }

    #[test]
    fn builds_sim_executor_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        let sec = config.sim_executor_config();
        assert!(sec.use_spread);
        assert!(sec.enable_market_impact);
    }

    #[test]
    fn defaults_work_with_minimal_config() {
        let minimal = r#"
[runtime]
mode = "dryrun"

[strategy]
"#;
        let config = FullConfig::from_toml(minimal).unwrap();
        assert_eq!(config.runtime.mode, "dryrun");
        assert_eq!(config.strategy.symbols, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
        assert!((config.strategy.min_edge - 0.05).abs() < 1e-10);
        assert!(config.execution.use_spread);
    }
}
