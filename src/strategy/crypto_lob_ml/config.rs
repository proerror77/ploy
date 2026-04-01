use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::agent_runtime::AgentRiskParams;

fn default_exit_edge_floor() -> Decimal {
    dec!(0.15)
}

fn default_exit_price_band() -> Decimal {
    dec!(0.25)
}

fn default_trailing_pullback_pct() -> Decimal {
    dec!(0.15)
}

fn default_trailing_time_decay() -> Decimal {
    dec!(0.50)
}

fn default_max_time_remaining_secs_5m() -> u64 {
    120
}

fn default_max_time_remaining_secs_15m() -> u64 {
    240
}

fn default_oracle_lag_buffer_secs() -> u64 {
    3
}

fn default_max_spread_pct() -> Decimal {
    dec!(0.10)
}

fn default_force_settle_only_5m() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoLobMlExitMode {
    SettleOnly,
    EvExit,
    SignalFlip,
    TrailingExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoLobMlEntrySidePolicy {
    BestEv,
    LaggingOnly,
}

fn default_exit_mode() -> CryptoLobMlExitMode {
    CryptoLobMlExitMode::EvExit
}

fn default_entry_side_policy() -> CryptoLobMlEntrySidePolicy {
    CryptoLobMlEntrySidePolicy::LaggingOnly
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoLobMlConfig {
    pub agent_id: String,
    pub name: String,
    pub coins: Vec<String>,
    pub event_refresh_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    #[serde(default = "default_max_time_remaining_secs_5m")]
    pub max_time_remaining_secs_5m: u64,
    #[serde(default = "default_max_time_remaining_secs_15m")]
    pub max_time_remaining_secs_15m: u64,
    pub prefer_close_to_end: bool,
    pub default_shares: u64,
    #[serde(default = "default_exit_edge_floor")]
    pub exit_edge_floor: Decimal,
    #[serde(default = "default_exit_price_band")]
    pub exit_price_band: Decimal,
    #[serde(default = "default_trailing_pullback_pct")]
    pub trailing_pullback_pct: Decimal,
    #[serde(default = "default_trailing_time_decay")]
    pub trailing_time_decay: Decimal,
    #[serde(default = "default_exit_mode")]
    pub exit_mode: CryptoLobMlExitMode,
    pub min_hold_secs: u64,
    pub min_edge: Decimal,
    pub max_entry_price: Decimal,
    #[serde(default = "default_entry_side_policy")]
    pub entry_side_policy: CryptoLobMlEntrySidePolicy,
    #[serde(default = "default_entry_late_window_secs_5m")]
    pub entry_late_window_secs_5m: u64,
    #[serde(default = "default_entry_late_window_secs_15m")]
    pub entry_late_window_secs_15m: u64,
    #[serde(default = "default_taker_fee_rate")]
    pub taker_fee_rate: Decimal,
    #[serde(default = "default_entry_slippage_bps")]
    pub entry_slippage_bps: Decimal,
    #[serde(default = "default_use_price_to_beat")]
    pub use_price_to_beat: bool,
    #[serde(default = "default_require_price_to_beat")]
    pub require_price_to_beat: bool,
    #[serde(
        default = "default_model_blend_weight",
        alias = "threshold_prob_weight"
    )]
    pub model_blend_weight: Decimal,
    #[serde(default = "default_min_direction_strength")]
    pub min_direction_strength: Decimal,
    pub cooldown_secs: u64,
    pub max_lob_snapshot_age_secs: u64,
    #[serde(default = "default_lob_ml_model_type")]
    pub model_type: String,
    #[serde(default)]
    pub model_path: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub model_sha256: Option<String>,
    #[serde(default)]
    pub model_trained_at: Option<String>,
    #[serde(default)]
    pub model_auc: Option<f64>,
    #[serde(default)]
    pub feature_offsets: Vec<f32>,
    #[serde(default)]
    pub feature_scales: Vec<f32>,
    #[serde(default = "default_window_fallback_weight")]
    pub _window_fallback_weight_compat: Decimal,
    #[serde(default = "default_ev_exit_buffer")]
    pub ev_exit_buffer: Decimal,
    #[serde(default = "default_ev_exit_vol_scale")]
    pub ev_exit_vol_scale: Decimal,
    pub risk_params: AgentRiskParams,
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_oracle_lag_buffer_secs")]
    pub oracle_lag_buffer_secs: u64,
    #[serde(default = "default_max_spread_pct")]
    pub max_spread_pct: Decimal,
    #[serde(default = "default_force_settle_only_5m")]
    pub force_settle_only_5m: bool,
}

fn default_lob_ml_model_type() -> String {
    "onnx".to_string()
}

fn default_window_fallback_weight() -> Decimal {
    dec!(0.10)
}

fn default_ev_exit_buffer() -> Decimal {
    dec!(0.005)
}

fn default_ev_exit_vol_scale() -> Decimal {
    dec!(0.02)
}

fn default_taker_fee_rate() -> Decimal {
    dec!(0.02)
}

fn default_entry_slippage_bps() -> Decimal {
    dec!(10)
}

fn default_use_price_to_beat() -> bool {
    true
}

fn default_require_price_to_beat() -> bool {
    true
}

fn default_model_blend_weight() -> Decimal {
    dec!(0.80)
}

fn default_min_direction_strength() -> Decimal {
    dec!(0.05)
}

fn default_entry_late_window_secs_5m() -> u64 {
    180
}

fn default_entry_late_window_secs_15m() -> u64 {
    180
}

impl Default for CryptoLobMlConfig {
    fn default() -> Self {
        Self {
            agent_id: "crypto_lob_ml".into(),
            name: "Crypto LOB ML".into(),
            coins: vec!["BTC".into(), "ETH".into(), "SOL".into(), "XRP".into(), "DOGE".into(), "HYPE".into(), "BNB".into()],
            event_refresh_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 900,
            max_time_remaining_secs_5m: default_max_time_remaining_secs_5m(),
            max_time_remaining_secs_15m: default_max_time_remaining_secs_15m(),
            prefer_close_to_end: true,
            default_shares: 50,
            exit_edge_floor: default_exit_edge_floor(),
            exit_price_band: default_exit_price_band(),
            trailing_pullback_pct: default_trailing_pullback_pct(),
            trailing_time_decay: default_trailing_time_decay(),
            exit_mode: default_exit_mode(),
            min_hold_secs: 20,
            min_edge: dec!(0.02),
            max_entry_price: dec!(0.70),
            entry_side_policy: default_entry_side_policy(),
            entry_late_window_secs_5m: default_entry_late_window_secs_5m(),
            entry_late_window_secs_15m: default_entry_late_window_secs_15m(),
            taker_fee_rate: default_taker_fee_rate(),
            entry_slippage_bps: default_entry_slippage_bps(),
            use_price_to_beat: default_use_price_to_beat(),
            require_price_to_beat: default_require_price_to_beat(),
            model_blend_weight: default_model_blend_weight(),
            min_direction_strength: default_min_direction_strength(),
            cooldown_secs: 30,
            max_lob_snapshot_age_secs: 2,
            model_type: default_lob_ml_model_type(),
            model_path: None,
            model_version: None,
            model_sha256: None,
            model_trained_at: None,
            model_auc: None,
            feature_offsets: vec![],
            feature_scales: vec![],
            _window_fallback_weight_compat: default_window_fallback_weight(),
            ev_exit_buffer: default_ev_exit_buffer(),
            ev_exit_vol_scale: default_ev_exit_vol_scale(),
            risk_params: AgentRiskParams::conservative(),
            heartbeat_interval_secs: 5,
            oracle_lag_buffer_secs: default_oracle_lag_buffer_secs(),
            max_spread_pct: default_max_spread_pct(),
            force_settle_only_5m: default_force_settle_only_5m(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_lob_ml_config_defaults_match_bootstrap_expectations() {
        let cfg = CryptoLobMlConfig::default();
        assert_eq!(cfg.agent_id, "crypto_lob_ml");
        assert_eq!(cfg.exit_mode, CryptoLobMlExitMode::EvExit);
        assert_eq!(
            cfg.entry_side_policy,
            CryptoLobMlEntrySidePolicy::LaggingOnly
        );
        assert!(cfg.use_price_to_beat);
        assert!(cfg.require_price_to_beat);
    }
}
