//! Unified TOML configuration for strategy runtime.
//!
//! A single config file drives backtest, dry-run, and live modes.
//! The `[runtime].mode` field selects which Feed and Executor are wired.

use chrono::{DateTime, Utc};
use ploy_market_contracts::{InstrumentKind, PredictionFamily, VenueKind};
use rust_decimal::prelude::FromPrimitive;
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
    pub reference_data: ReferenceDataSection,
    #[serde(default)]
    pub backtest_data: BacktestDataSection,
    #[serde(default)]
    pub execution: SimExecutionSection,
    #[serde(default)]
    pub live_execution: LiveExecutionSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSection {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub throttle_hz: Option<u32>,
    pub max_updates: Option<u64>,
    /// Strategy variant.
    ///
    /// Strategy variant. Canonical variants and roadmap aliases are owned by
    /// `strategies::registry`; keep this config field as the TOML surface only.
    #[serde(default = "default_strategy_variant")]
    pub strategy_variant: String,
    /// Prediction family for the configured runtime.
    #[serde(default)]
    pub prediction_family: PredictionFamily,
    /// Instrument kind for the configured runtime.
    #[serde(default)]
    pub instrument_kind: InstrumentKind,
    /// Venue kind for the configured runtime.
    #[serde(default)]
    pub venue: VenueKind,
    /// Backtest start time (ISO 8601 format, e.g., "2026-04-01T00:00:00Z")
    pub from: Option<String>,
    /// Backtest end time (ISO 8601 format, e.g., "2026-04-01T23:59:59Z")
    pub to: Option<String>,
    /// Optional NDJSON log path for canonical `MarketUpdate` recording.
    pub record_market_updates_to: Option<PathBuf>,
    /// Required when `mode = "replay"`; points at a previously recorded NDJSON log.
    pub replay_market_updates_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReferenceDataSection {
    /// Optional Pyth/RTDS symbols to capture alongside the existing crypto runtime.
    ///
    /// Symbols are case-insensitive in config. RTDS payloads are normalized to lowercase.
    #[serde(default)]
    pub pyth_symbols: Vec<String>,
    /// Enable Polymarket sports live-state capture for record/replay and persistence.
    #[serde(default)]
    pub capture_sports_state: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BacktestDataSection {
    /// Load additive reference-price ticks into historical backtests.
    #[serde(default)]
    pub include_reference_prices: bool,
    /// Load additive sports-state updates into historical backtests.
    #[serde(default)]
    pub include_sports_state: bool,
    /// When true, historical backtests skip markets without official settlement rows.
    #[serde(default)]
    pub require_official_settlement: bool,
    /// Optional symbol override for reference-price backtests.
    ///
    /// When empty, the historical loader falls back to `reference_data.pyth_symbols`.
    #[serde(default)]
    pub reference_symbols: Vec<String>,
}

impl BacktestDataSection {
    #[must_use]
    pub fn reference_symbols(&self, reference_data: &ReferenceDataSection) -> Vec<String> {
        let raw_symbols = if self.reference_symbols.is_empty() {
            &reference_data.pyth_symbols
        } else {
            &self.reference_symbols
        };

        raw_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}

fn default_mode() -> String {
    "dryrun".into()
}

fn default_strategy_variant() -> String {
    "directional".into()
}

impl RuntimeSection {
    #[must_use]
    pub fn canonical_strategy_variant(&self) -> String {
        crate::strategies::registry::canonical_strategy_variant(&self.strategy_variant)
    }
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

#[derive(Debug, Clone, Deserialize)]
pub struct LiveExecutionSection {
    /// Maximum permitted price movement from the strategy limit for live FAK/FOK
    /// execution. Buy orders cap at `limit * (1 + bps/10000)`; sell orders floor
    /// at `limit * (1 - bps/10000)`.
    #[serde(default = "default_live_max_slippage_bps")]
    pub max_slippage_bps: u32,
    /// Maximum total submission attempts for an acknowledged-but-unfilled FAK
    /// order before the runtime marks the intent terminal and lets the strategy
    /// cool down.
    #[serde(default = "default_live_max_attempts")]
    pub max_attempts: u8,
    /// Number of reconciliation passes to wait before treating an acknowledged
    /// FAK order with no new fill as terminal/unfilled for that attempt.
    #[serde(default = "default_live_reconcile_cycles_before_retry")]
    pub reconcile_cycles_before_retry: u8,
}

fn default_live_max_slippage_bps() -> u32 {
    150
}

fn default_live_max_attempts() -> u8 {
    2
}

fn default_live_reconcile_cycles_before_retry() -> u8 {
    2
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

impl Default for LiveExecutionSection {
    fn default() -> Self {
        Self {
            max_slippage_bps: default_live_max_slippage_bps(),
            max_attempts: default_live_max_attempts(),
            reconcile_cycles_before_retry: default_live_reconcile_cycles_before_retry(),
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
            skip_settlement_exits: mode == RuntimeMode::Live,
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

    pub fn live_execution_policy(&self) -> crate::traits::ExecutionPolicy {
        crate::traits::ExecutionPolicy {
            max_slippage_bps: Decimal::from_u32(self.live_execution.max_slippage_bps)
                .unwrap_or_default(),
            max_attempts: self.live_execution.max_attempts.max(1),
            reconcile_cycles_before_retry: self.live_execution.reconcile_cycles_before_retry.max(1),
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

[reference_data]
pyth_symbols = ["AAPL", "XAUUSD"]
capture_sports_state = true

[backtest_data]
include_reference_prices = true
include_sports_state = true

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
        assert_eq!(config.reference_data.pyth_symbols, vec!["AAPL", "XAUUSD"]);
        assert!(config.reference_data.capture_sports_state);
        assert!(config.backtest_data.include_reference_prices);
        assert!(config.backtest_data.include_sports_state);
        assert_eq!(config.live_execution.max_slippage_bps, 150);
        assert!(!config.backtest_data.require_official_settlement);
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
        assert!(config.reference_data.pyth_symbols.is_empty());
        assert!(!config.reference_data.capture_sports_state);
        assert!(!config.backtest_data.include_reference_prices);
        assert!(!config.backtest_data.include_sports_state);
        assert!(!config.backtest_data.require_official_settlement);
        assert_eq!(config.live_execution.max_attempts, 2);
        assert_eq!(config.live_execution.reconcile_cycles_before_retry, 2);
        assert!((config.strategy.min_edge - 0.02).abs() < 1e-10);
        assert!(!config.execution.use_spread);
    }

    #[test]
    fn parses_live_execution_policy() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "live"

[strategy]

[live_execution]
max_slippage_bps = 75
max_attempts = 3
reconcile_cycles_before_retry = 2
"#,
        )
        .unwrap();

        let policy = config.live_execution_policy();
        assert_eq!(policy.max_slippage_bps, Decimal::new(75, 0));
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.reconcile_cycles_before_retry, 2);
    }

    #[test]
    fn backtest_data_defaults_reference_symbols_to_reference_capture_list() {
        let config = FullConfig::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(
            config
                .backtest_data
                .reference_symbols(&config.reference_data),
            vec!["aapl".to_string(), "xauusd".to_string()]
        );
    }

    #[test]
    fn backtest_data_prefers_explicit_reference_symbols() {
        let toml = r#"
[runtime]
mode = "backtest"

[strategy]

[reference_data]
pyth_symbols = ["AAPL", "XAUUSD"]

[backtest_data]
include_reference_prices = true
reference_symbols = ["GLD", "SPY"]
"#;

        let config = FullConfig::from_toml(toml).unwrap();
        assert_eq!(
            config
                .backtest_data
                .reference_symbols(&config.reference_data),
            vec!["gld".to_string(), "spy".to_string()]
        );
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
            let config = FullConfig::from_toml(&format!(
                r#"
[runtime]
mode = "dryrun"
strategy_variant = "{raw}"

[strategy]
"#
            ))
            .unwrap();
            assert_eq!(config.runtime.canonical_strategy_variant(), expected);
        }
    }

    #[test]
    fn runtime_contract_metadata_defaults_to_crypto_polymarket() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "dryrun"

[strategy]
"#,
        )
        .unwrap();

        assert_eq!(
            config.runtime.prediction_family,
            PredictionFamily::CryptoExpiry
        );
        assert_eq!(config.runtime.instrument_kind, InstrumentKind::UpDown);
        assert_eq!(config.runtime.venue, VenueKind::Polymarket);
    }

    #[test]
    fn runtime_contract_metadata_parses_snake_case() {
        let config = FullConfig::from_toml(
            r#"
[runtime]
mode = "dryrun"
prediction_family = "sports_pregame"
instrument_kind = "moneyline"
venue = "sportsbook"

[strategy]
"#,
        )
        .unwrap();

        assert_eq!(
            config.runtime.prediction_family,
            PredictionFamily::SportsPregame
        );
        assert_eq!(config.runtime.instrument_kind, InstrumentKind::Moneyline);
        assert_eq!(config.runtime.venue, VenueKind::Sportsbook);
    }

    #[test]
    fn roadmap_config_family_parses() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let config_dir = manifest_dir.join("../../config/strategies");

        for file in [
            "02-pm5d.v1-dryrun.toml",
            "02-pm5d.v1-live.toml",
            "02-pm5d.v2-dryrun.toml",
            "02-pm5d.v2-live.toml",
            "02-pm5d.v3-dryrun.toml",
            "02-pm5d.v3-live.toml",
            "02-pm5d.v4-dryrun.toml",
            "02-pm5d.v4-live.toml",
            "05-reversal.dryrun.toml",
            "05-reversal.backtest.toml",
        ] {
            let path = config_dir.join(file);
            let config = FullConfig::from_file(path.to_str().unwrap()).unwrap();
            assert!(
                !config.strategy.symbols.is_empty(),
                "{file} should define at least one symbol"
            );
            assert!(
                !config.runtime.canonical_strategy_variant().is_empty(),
                "{file} should resolve to a runtime variant"
            );
        }
    }
}
