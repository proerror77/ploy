//! Unified TOML configuration for strategy runtime.
//!
//! A single config file drives backtest, dry-run, and live modes.
//! The `[runtime].mode` field selects which Feed and Executor are wired.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::Path;
use std::path::PathBuf;

use crate::engine::{RuntimeConfig, RuntimeMode};
use crate::executor::SimulatedExecutorConfig;
use crate::strategies::directional::DirectionalConfig;

/// Top-level config deserialized from a TOML file.
///
/// ```toml
/// [runtime]
/// mode = "dryrun"
/// throttle_hz = 1
/// record_market_updates_to = "tmp/dryrun.ndjson"
///
/// [strategy]
/// symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
/// min_edge = 0.02
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
    /// Backtest start time (ISO 8601 format, e.g., "2026-04-01T00:00:00Z")
    pub from: Option<String>,
    /// Backtest end time (ISO 8601 format, e.g., "2026-04-01T23:59:59Z")
    pub to: Option<String>,
    /// Optional NDJSON log path for canonical `MarketUpdate` recording.
    pub record_market_updates_to: Option<PathBuf>,
    /// Required when `mode = "replay"`; points at a previously recorded NDJSON log.
    pub replay_market_updates_from: Option<PathBuf>,
}

fn default_mode() -> String {
    "dryrun".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimExecutionSection {
    #[serde(default)]
    pub use_spread: bool,
    #[serde(default = "default_spread")]
    pub spread_pct: f64,
    #[serde(default)]
    pub enable_partial_fills: bool,
    #[serde(default = "default_depth_multiple")]
    pub depth_multiple: f64,
    #[serde(default = "default_min_fill")]
    pub min_fill_pct: f64,
    #[serde(default)]
    pub enable_market_impact: bool,
    #[serde(default = "default_impact")]
    pub impact_coefficient: f64,
    #[serde(default = "default_depth_shares")]
    pub default_depth_shares: u64,
}

fn default_spread() -> f64 {
    0.02
}
fn default_depth_multiple() -> f64 {
    5.0
}
fn default_min_fill() -> f64 {
    0.5
}
fn default_impact() -> f64 {
    0.1
}
fn default_depth_shares() -> u64 {
    500
}

impl Default for SimExecutionSection {
    fn default() -> Self {
        Self {
            use_spread: false,
            spread_pct: 0.02,
            enable_partial_fills: false,
            depth_multiple: 5.0,
            min_fill_pct: 0.5,
            enable_market_impact: false,
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
    ///
    /// Rejects legacy-format configs (those with `[timing]` or `[entry]`
    /// sections) to prevent silent value misinterpretation.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Cannot read config {path}: {e}"))?;

        // Guard: reject legacy format that would silently misparse
        if content.contains("[timing]") || content.contains("[entry]") || content.contains("[risk]")
        {
            return Err(format!(
                "Config {path} uses legacy format ([timing]/[entry]/[risk] sections). \
                 Use 02-pm5d.unified.toml format instead: [runtime]/[strategy]/[execution]."
            )
            .into());
        }

        Self::from_toml(&content).map_err(|e| format!("Invalid TOML in {path}: {e}").into())
    }

    /// Build RuntimeConfig from the parsed config.
    pub fn runtime_config(&self) -> RuntimeConfig {
        let mode = match self.runtime.mode.as_str() {
            "backtest" => RuntimeMode::Backtest,
            "replay" => RuntimeMode::Replay,
            "live" => RuntimeMode::Live,
            _ => RuntimeMode::DryRun,
        };
        RuntimeConfig {
            mode,
            throttle_hz: self.runtime.throttle_hz,
            max_updates: self.runtime.max_updates,
        }
    }

    /// Parse backtest time range from config.
    /// Returns (from, to) if both are specified, otherwise None.
    pub fn backtest_time_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let from_str = self.runtime.from.as_ref()?;
        let to_str = self.runtime.to.as_ref()?;

        let from = DateTime::parse_from_rfc3339(from_str)
            .ok()?
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339(to_str)
            .ok()?
            .with_timezone(&Utc);

        Some((from, to))
    }

    pub fn record_market_updates_path(&self) -> Option<&Path> {
        self.runtime.record_market_updates_to.as_deref()
    }

    pub fn replay_market_updates_path(&self) -> Option<&Path> {
        self.runtime.replay_market_updates_from.as_deref()
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
record_market_updates_to = "tmp/sample.ndjson"

[strategy]
symbols = ["BTCUSDT", "ETHUSDT"]
vol_floor = 0.001
min_probability = 0.55
min_z_score = 0.35
min_entry_price = 0.15
max_entry_price = 0.85
no_trade_zone_min = 0.45
no_trade_zone_max = 0.55
min_edge = 0.02
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 0
stake_usd = 25.0
max_positions = 1000
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
        assert_eq!(
            config.runtime.record_market_updates_to.as_deref(),
            Some(Path::new("tmp/sample.ndjson"))
        );
        assert_eq!(config.strategy.symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert!((config.strategy.min_edge - 0.02).abs() < 1e-10);
        assert_eq!(config.strategy.stake_usd, Decimal::new(25, 0));
        assert_eq!(config.strategy.max_positions, 1000);
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
    fn parses_replay_runtime_paths() {
        let replay_toml = r#"
[runtime]
mode = "replay"
replay_market_updates_from = "captures/dryrun.ndjson"

[strategy]
"#;

        let config = FullConfig::from_toml(replay_toml).unwrap();
        let runtime = config.runtime_config();

        assert_eq!(runtime.mode, RuntimeMode::Replay);
        assert_eq!(
            config.replay_market_updates_path(),
            Some(Path::new("captures/dryrun.ndjson"))
        );
    }

    #[test]
    fn builds_sim_executor_config() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        let sec = config.sim_executor_config();
        assert!(!sec.use_spread);
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
        assert_eq!(
            config.strategy.symbols,
            vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        );
        assert!((config.strategy.min_edge - 0.02).abs() < 1e-10);
        assert!(!config.execution.use_spread);
    }

    #[test]
    fn legacy_quantity_alias_still_maps_to_stake_usd() {
        let legacy = r#"
[runtime]
mode = "backtest"

[strategy]
quantity = 25.0
"#;
        let config = FullConfig::from_toml(legacy).unwrap();
        assert_eq!(config.strategy.stake_usd, Decimal::new(25, 0));
    }
}
