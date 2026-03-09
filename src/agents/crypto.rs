//! Legacy crypto momentum compatibility config.
//!
//! The canonical live momentum path now runs through the managed `Strategy`
//! runtime. This module intentionally keeps only the public config surface
//! consumed by bootstrap/config builders after the trading-agent runtime was
//! removed.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::platform::AgentRiskParams;

fn default_exit_edge_floor() -> Decimal {
    dec!(0.02)
}

fn default_exit_price_band() -> Decimal {
    dec!(0.05)
}

/// Entry mode for the bootstrap crypto momentum config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoEntryMode {
    /// Original arbitrage-only mode: require sum_of_asks < threshold.
    ArbOnly,
    /// Directional mode: trade based on momentum edge alone, no sum constraint.
    Directional,
    /// Volatility straddle: buy both UP and DOWN when sum < straddle_threshold.
    VolStraddle,
}

fn default_entry_mode() -> CryptoEntryMode {
    CryptoEntryMode::Directional
}

fn default_oracle_lag_buffer_secs() -> u64 {
    3
}

fn default_max_spread_pct() -> Decimal {
    dec!(0.10)
}

fn default_straddle_threshold() -> Decimal {
    dec!(0.99)
}

fn default_straddle_min_vol() -> Decimal {
    Decimal::ZERO
}

fn default_min_signal_score() -> Decimal {
    dec!(0.40)
}

/// Bootstrap/runtime-builder config for crypto momentum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub sum_threshold: Decimal,
    pub min_momentum_1s: f64,
    #[serde(default)]
    pub min_window_move_pct: Decimal,
    #[serde(default = "default_exit_edge_floor")]
    pub min_edge: Decimal,
    pub event_refresh_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub prefer_close_to_end: bool,
    #[serde(default)]
    pub entry_cooldown_secs: u64,
    #[serde(default)]
    pub require_mtf_agreement: bool,
    pub default_shares: u64,
    #[serde(default = "default_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    #[serde(default = "default_exit_price_band")]
    pub exit_price_band: Decimal,
    pub enable_price_exits: bool,
    pub min_hold_secs: u64,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_entry_mode")]
    pub entry_mode: CryptoEntryMode,
    #[serde(default = "default_oracle_lag_buffer_secs")]
    pub oracle_lag_buffer_secs: u64,
    #[serde(default = "default_max_spread_pct")]
    pub max_spread_pct: Decimal,
    #[serde(default = "default_straddle_threshold")]
    pub straddle_threshold: Decimal,
    #[serde(default = "default_straddle_min_vol")]
    pub straddle_min_vol: Decimal,
    #[serde(default = "default_min_signal_score")]
    pub min_signal_score: Decimal,
}

impl Default for CryptoTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto".to_string(),
            name: "Crypto Momentum".to_string(),
            coins: vec![
                "BTC".to_string(),
                "ETH".to_string(),
                "SOL".to_string(),
                "XRP".to_string(),
            ],
            sum_threshold: dec!(0.96),
            min_momentum_1s: 0.001,
            min_window_move_pct: dec!(0.0001),
            min_edge: dec!(0.02),
            event_refresh_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            prefer_close_to_end: true,
            entry_cooldown_secs: 0,
            require_mtf_agreement: true,
            default_shares: 100,
            exit_edge_floor: default_exit_edge_floor(),
            exit_price_band: default_exit_price_band(),
            enable_price_exits: false,
            min_hold_secs: 20,
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
            entry_mode: default_entry_mode(),
            oracle_lag_buffer_secs: default_oracle_lag_buffer_secs(),
            max_spread_pct: default_max_spread_pct(),
            straddle_threshold: default_straddle_threshold(),
            straddle_min_vol: default_straddle_min_vol(),
            min_signal_score: default_min_signal_score(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_managed_runtime_expectations() {
        let cfg = CryptoTradingConfig::default();
        assert_eq!(cfg.agent_id, "crypto");
        assert_eq!(cfg.name, "Crypto Momentum");
        assert_eq!(cfg.coins, vec!["BTC", "ETH", "SOL", "XRP"]);
        assert_eq!(cfg.entry_mode, CryptoEntryMode::Directional);
        assert_eq!(cfg.min_edge, dec!(0.02));
        assert!(!cfg.enable_price_exits);
    }
}
