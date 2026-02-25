//! OpenClaw meta-agent configuration

use serde::{Deserialize, Serialize};

/// Top-level config for the OpenClaw meta-agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawConfig {
    /// Agent identifier (used for coordinator registration)
    #[serde(default = "default_agent_id")]
    pub agent_id: String,

    /// Whether OpenClaw is enabled (defaults to false — opt-in)
    #[serde(default)]
    pub enabled: bool,

    /// Regime detection interval (seconds)
    #[serde(default = "default_regime_tick")]
    pub regime_tick_secs: u64,

    /// Performance evaluation interval (seconds)
    #[serde(default = "default_perf_tick")]
    pub perf_tick_secs: u64,

    /// Capital reallocation interval (seconds)
    #[serde(default = "default_alloc_tick")]
    pub alloc_tick_secs: u64,

    /// Rolling performance window (seconds)
    #[serde(default = "default_perf_window")]
    pub perf_window_secs: u64,

    /// BTC symbol for regime detection
    #[serde(default = "default_btc_symbol")]
    pub btc_symbol: String,

    /// Regime detection parameters
    #[serde(default)]
    pub regime: RegimeConfig,

    /// Capital allocation parameters
    #[serde(default)]
    pub allocator: AllocatorConfig,

    /// Temporal straddle parameters
    #[serde(default)]
    pub straddle: StraddleConfig,
}

fn default_agent_id() -> String {
    "openclaw".to_string()
}
fn default_regime_tick() -> u64 {
    15
}
fn default_perf_tick() -> u64 {
    30
}
fn default_alloc_tick() -> u64 {
    120
}
fn default_perf_window() -> u64 {
    3600
}
fn default_btc_symbol() -> String {
    "BTCUSDT".to_string()
}

impl Default for OpenClawConfig {
    fn default() -> Self {
        Self {
            agent_id: default_agent_id(),
            enabled: false,
            regime_tick_secs: default_regime_tick(),
            perf_tick_secs: default_perf_tick(),
            alloc_tick_secs: default_alloc_tick(),
            perf_window_secs: default_perf_window(),
            btc_symbol: default_btc_symbol(),
            regime: RegimeConfig::default(),
            allocator: AllocatorConfig::default(),
            straddle: StraddleConfig::default(),
        }
    }
}

/// Regime detection thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeConfig {
    /// Short-term vol window (seconds) — used for spike detection
    #[serde(default = "default_vol_short")]
    pub vol_short_secs: u64,

    /// Long-term vol window (seconds) — used for baseline
    #[serde(default = "default_vol_long")]
    pub vol_long_secs: u64,

    /// Vol ratio threshold: short/long > this → HighVol
    #[serde(default = "default_high_vol_ratio")]
    pub high_vol_ratio: f64,

    /// Vol ratio threshold: short/long < this → LowVol
    #[serde(default = "default_low_vol_ratio")]
    pub low_vol_ratio: f64,

    /// Trend consistency window (number of 1s ticks to evaluate direction)
    #[serde(default = "default_trend_window")]
    pub trend_window_secs: u64,

    /// Minimum directional consistency (0.0-1.0) to declare Trending
    #[serde(default = "default_trend_threshold")]
    pub trend_threshold: f64,

    /// Number of consecutive same-regime readings before transition
    #[serde(default = "default_confirmation_count")]
    pub confirmation_count: u32,
}

fn default_vol_short() -> u64 {
    60
}
fn default_vol_long() -> u64 {
    300
}
fn default_high_vol_ratio() -> f64 {
    1.5
}
fn default_low_vol_ratio() -> f64 {
    0.7
}
fn default_trend_window() -> u64 {
    120
}
fn default_trend_threshold() -> f64 {
    0.65
}
fn default_confirmation_count() -> u32 {
    2
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            vol_short_secs: default_vol_short(),
            vol_long_secs: default_vol_long(),
            high_vol_ratio: default_high_vol_ratio(),
            low_vol_ratio: default_low_vol_ratio(),
            trend_window_secs: default_trend_window(),
            trend_threshold: default_trend_threshold(),
            confirmation_count: default_confirmation_count(),
        }
    }
}

/// Capital allocation parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocatorConfig {
    /// Minimum score difference to trigger reallocation
    #[serde(default = "default_realloc_threshold")]
    pub realloc_threshold: f64,

    /// Maximum fraction of capital allocated to any single agent
    #[serde(default = "default_max_single_allocation")]
    pub max_single_allocation: f64,

    /// Cooldown after pausing an agent (seconds) before it can be resumed
    #[serde(default = "default_pause_cooldown_secs")]
    pub pause_cooldown_secs: u64,

    /// Sharpe weight in composite score
    #[serde(default = "default_sharpe_weight")]
    pub sharpe_weight: f64,

    /// Win rate weight in composite score
    #[serde(default = "default_win_rate_weight")]
    pub win_rate_weight: f64,

    /// Drawdown weight in composite score (inverted: lower drawdown = higher score)
    #[serde(default = "default_drawdown_weight")]
    pub drawdown_weight: f64,
}

fn default_realloc_threshold() -> f64 {
    0.1
}
fn default_max_single_allocation() -> f64 {
    0.6
}
fn default_pause_cooldown_secs() -> u64 {
    300
}
fn default_sharpe_weight() -> f64 {
    0.4
}
fn default_win_rate_weight() -> f64 {
    0.3
}
fn default_drawdown_weight() -> f64 {
    0.3
}

impl Default for AllocatorConfig {
    fn default() -> Self {
        Self {
            realloc_threshold: default_realloc_threshold(),
            max_single_allocation: default_max_single_allocation(),
            pause_cooldown_secs: default_pause_cooldown_secs(),
            sharpe_weight: default_sharpe_weight(),
            win_rate_weight: default_win_rate_weight(),
            drawdown_weight: default_drawdown_weight(),
        }
    }
}

/// Temporal leg straddle parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StraddleConfig {
    /// Enable temporal straddle coordination
    #[serde(default)]
    pub enabled: bool,

    /// Minimum price move (%) from Leg1 entry to trigger Leg2
    #[serde(default = "default_leg2_trigger_move_pct")]
    pub leg2_trigger_move_pct: f64,

    /// Maximum time (seconds) to wait for Leg2 trigger after Leg1 fill
    #[serde(default = "default_leg2_max_wait_secs")]
    pub leg2_max_wait_secs: u64,

    /// Maximum combined cost for both legs (must be < 1.0 for guaranteed profit)
    #[serde(default = "default_max_combined_cost")]
    pub max_combined_cost: f64,
}

fn default_leg2_trigger_move_pct() -> f64 {
    2.0
}
fn default_leg2_max_wait_secs() -> u64 {
    600
}
fn default_max_combined_cost() -> f64 {
    0.97
}

impl Default for StraddleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            leg2_trigger_move_pct: default_leg2_trigger_move_pct(),
            leg2_max_wait_secs: default_leg2_max_wait_secs(),
            max_combined_cost: default_max_combined_cost(),
        }
    }
}
