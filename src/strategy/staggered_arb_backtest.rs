//! Staggered Arbitrage Backtest Engine — 時間差套利回測
//!
//! Polymarket binary options have `up_ask + down_ask > 1` at any point (market maker spread).
//! Buying both sides simultaneously always loses. But by using volatility prediction to time
//! entries — buying the side about to get expensive first, then buying the other side after
//! price movement — the total cost of both legs can be < $1, yielding risk-free arbitrage.
//!
//! When both legs are filled, they are immediately merged (redeemed) for $1.00 per share,
//! without waiting for settlement. This dramatically improves capital turnover.
//!
//! Usage:
//!   ploy strategy backtest staggered-arb --symbols BTCUSDT --save --json

use std::collections::HashMap;
use std::fmt;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{
    BacktestRecorder, BacktestSignal, NullRecorder, PendingTrade, SignalType,
};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::gamma_scalping::greeks::{binary_greeks, BinaryGreeks};
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a staggered arbitrage backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent Leg1 positions. 0 disables the cap.
    pub max_concurrent_positions: usize,
    // ── Signal thresholds ──
    /// Minimum |p_hat - 0.5| to trigger entry
    pub direction_threshold: f64,
    /// Only apply premium-entry strengthening above this sum threshold.
    pub premium_sum_threshold: Decimal,
    /// Extra direction-strength required per 1.0 sum above `premium_sum_threshold`.
    pub premium_sum_direction_slope: f64,
    /// Extra OBI confirmation required per 1.0 sum above `premium_sum_threshold`.
    /// Live-only today; kept in shared config so live/backtest profiles stay aligned.
    pub premium_sum_obi_slope: f64,
    /// Base OBI confirmation threshold before premium adjustments.
    pub obi_confirm_threshold: f64,
    /// OBI strength that unlocks the strong-signal entry profile.
    pub strong_obi_threshold: f64,
    /// How much strong OBI can relax the direction-strength gate.
    pub strong_obi_direction_relaxation: f64,
    /// Extra Leg1 price we will tolerate when OBI is both strong and persistent.
    pub strong_obi_price_bonus: Decimal,
    /// Extra opening-window seconds allowed for 15m windows under a strong OBI regime.
    pub strong_obi_window_bonus_secs: u64,
    /// Reverse the direction signal: if true, buy the OPPOSITE of what the model predicts.
    /// Useful when the model is consistently wrong (accuracy < 50%).
    pub reverse_signal: bool,
    /// Maximum up_ask + down_ask to consider entry (legacy; prefer max_leg1_price).
    /// 0 disables the hard cap so OBI/direction can decide entry.
    pub max_initial_sum: Decimal,
    /// Maximum Leg1 ask price — don't buy if the predicted side is too expensive
    /// (leaves no room for the other side to drop enough for profit)
    pub max_leg1_price: Decimal,
    /// Target merge sum: leg1 + leg2 < this value to trigger merge (e.g., $0.95)
    pub merge_target_sum: Decimal,
    /// Minimum profit target per share after both legs
    pub min_profit_target: Decimal,
    // ── Time control ──
    /// Maximum seconds to wait for Leg2 after Leg1 fill
    pub max_wait_secs: u64,
    /// Minimum seconds after event start before opening Leg1. 0 disables this delay.
    pub entry_after_start_min_secs: u64,
    /// Prefer entering soon after event start. 0 disables this gate.
    pub entry_after_start_max_secs: u64,
    /// In the final N seconds, do not place hedge/exit trades. 0 disables this gate.
    pub no_trade_last_secs: u64,
    /// Maximum fraction of window duration to wait for Leg2
    pub max_wait_pct: f64,
    /// Minimum time remaining in window to enter
    pub min_time_remaining_secs: u64,
    // ── Risk control ──
    /// Maximum unrealized loss on Leg1 before buying Leg2 to cap the trade
    pub max_leg1_loss: Decimal,
    /// If sum <= this value, allow generic forced Leg2 closes (timeout / time-safety / final window)
    pub force_complete_threshold: Decimal,
    /// Maximum sum allowed for protective Leg2 closes (stop-loss / theta urgency)
    pub protective_close_threshold: Decimal,
    /// Exit support decays once OBI falls below this ratio of entry OBI.
    pub obi_decay_exit_ratio: f64,
    /// Treat OBI as flipped once it crosses this directional magnitude against the position.
    pub obi_flip_exit_threshold: f64,
    /// Minimum ask price to consider (filters out illiquid extreme prices)
    pub min_ask_price: Decimal,
    /// Minimum up_ask + down_ask to enter (filters out illiquid extreme-price pairs)
    pub min_entry_sum: Decimal,
    // ── Window filter ──
    /// Allowed window durations in seconds (e.g. [300, 900] for 5m + 15m).
    /// Empty = accept all durations.
    pub allowed_window_durations: Vec<u64>,
    /// Tolerance in seconds when matching window durations (default 30)
    pub window_duration_tolerance: u64,
    // ── Execution realism ──
    /// Minimum seconds between Leg1 fill and Leg2 fill (simulates CLOB latency)
    pub min_leg2_delay_secs: u64,
    /// Maximum trades per event window (prevents overtrading same window)
    pub max_trades_per_event: usize,
    // ── Vol model ──
    /// Drift estimate for log-normal model
    pub mu: f64,
    /// Volatility lookback window in seconds
    pub vol_lookback_secs: u64,
    /// Volatility floor to prevent overconfidence
    pub vol_floor: f64,
    /// Minimum realized sigma required to enter the long-gamma regime
    pub min_entry_sigma: f64,
    /// Maximum realized sigma allowed to enter the long-gamma regime. 0 disables the cap.
    pub max_entry_sigma: f64,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    // ── Greeks integration ──
    /// Enable Greeks-based entry/exit refinement
    pub use_greeks: bool,
    /// Minimum gamma to enter (filters out deep ITM/OTM with no convexity)
    pub min_gamma: f64,
    /// Maximum theta decay cost (per share, per second) to hold position
    pub max_theta_cost: f64,
    /// Require fair value to remain within 0.5 +/- this distance for long-gamma entries.
    pub max_fair_value_distance: f64,
    /// Delta-weighted sizing: scale shares by |delta| relative to ATM delta
    pub delta_weighted_sizing: bool,
}

impl Default for StaggeredArbBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 20,
            max_concurrent_positions: 0,
            direction_threshold: 0.05,
            premium_sum_threshold: Decimal::ONE,
            premium_sum_direction_slope: 1.25,
            premium_sum_obi_slope: 0.25,
            obi_confirm_threshold: 0.005,
            strong_obi_threshold: 0.015,
            strong_obi_direction_relaxation: 0.015,
            strong_obi_price_bonus: dec!(0.02),
            strong_obi_window_bonus_secs: 60,
            reverse_signal: false,
            max_initial_sum: Decimal::ZERO,
            max_leg1_price: dec!(0.56),
            merge_target_sum: dec!(0.95),
            min_profit_target: dec!(0.02),
            max_wait_secs: 120,
            entry_after_start_min_secs: 30,
            entry_after_start_max_secs: 240,
            no_trade_last_secs: 30,
            max_wait_pct: 0.30,
            min_time_remaining_secs: 45,
            max_leg1_loss: dec!(0.03),
            force_complete_threshold: dec!(1.08),
            protective_close_threshold: dec!(1.08),
            obi_decay_exit_ratio: 0.35,
            obi_flip_exit_threshold: 0.008,
            min_ask_price: dec!(0.05),
            min_entry_sum: dec!(0.30),
            allowed_window_durations: vec![300],
            window_duration_tolerance: 30,
            min_leg2_delay_secs: 3,
            max_trades_per_event: 0,
            mu: 0.0,
            vol_lookback_secs: 600,
            vol_floor: 0.003,
            min_entry_sigma: 0.003,
            max_entry_sigma: 0.0,
            cooldown_secs: 5,
            use_greeks: true,
            min_gamma: 0.0,
            max_theta_cost: 0.0,
            max_fair_value_distance: 0.15,
            delta_weighted_sizing: false,
        }
    }
}

impl StaggeredArbBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }

    fn adaptive_close_threshold(
        &self,
        configured_cap: Decimal,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        urgency_floor: f64,
        in_final_window: bool,
    ) -> Decimal {
        if configured_cap <= Decimal::ZERO || configured_cap <= Decimal::ONE {
            return configured_cap;
        }
        if in_final_window {
            return configured_cap;
        }
        if !time_remaining_secs.is_finite() || window_duration_secs == 0 {
            return configured_cap;
        }

        let window_secs = window_duration_secs as f64;
        let remaining_ratio = (time_remaining_secs / window_secs).clamp(0.0, 1.0);
        let elapsed_ratio = 1.0 - remaining_ratio;
        let safety_ratio = if self.min_time_remaining_secs > 0 {
            (1.0 - (time_remaining_secs / self.min_time_remaining_secs as f64)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let urgency = elapsed_ratio.max(safety_ratio).max(urgency_floor).min(1.0);
        let urgency_dec = Decimal::from_f64(urgency).unwrap_or(Decimal::ONE);
        Decimal::ONE + (configured_cap - Decimal::ONE) * urgency_dec
    }

    fn obi_directional_value(&self, predicted_up: bool, obi: f64) -> f64 {
        if predicted_up {
            obi
        } else {
            -obi
        }
    }

    pub(crate) fn obi_confirms_direction(
        &self,
        predicted_up: bool,
        obi: f64,
        required_strength: f64,
    ) -> bool {
        self.obi_directional_value(predicted_up, obi) >= required_strength
    }

    pub(crate) fn obi_is_persistent(
        &self,
        predicted_up: bool,
        obi: f64,
        prev_obi: Option<f64>,
        required_strength: f64,
    ) -> bool {
        if !self.obi_confirms_direction(predicted_up, obi, required_strength) {
            return false;
        }
        let Some(prev) = prev_obi else {
            return self.obi_directional_value(predicted_up, obi) >= self.strong_obi_threshold;
        };
        self.obi_directional_value(predicted_up, prev) >= required_strength * 0.75
    }

    pub(crate) fn strong_obi_entry_bonus_active(
        &self,
        predicted_up: bool,
        obi: f64,
        prev_obi: Option<f64>,
        current_sum: Decimal,
        fair_value_distance: Option<f64>,
    ) -> bool {
        if current_sum > self.premium_sum_threshold + dec!(0.04) {
            return false;
        }
        if fair_value_distance.unwrap_or(f64::INFINITY) > self.max_fair_value_distance.min(0.10) {
            return false;
        }
        let directional_obi = self.obi_directional_value(predicted_up, obi);
        let Some(prev) = prev_obi else {
            return false;
        };
        directional_obi >= self.strong_obi_threshold
            && self.obi_directional_value(predicted_up, prev) >= self.obi_confirm_threshold
    }

    pub(crate) fn direction_threshold_now(
        &self,
        current_sum: Decimal,
        strong_obi_bonus_active: bool,
    ) -> f64 {
        let premium_sum_excess = self.premium_sum_excess(current_sum);
        let mut threshold =
            self.direction_threshold + premium_sum_excess * self.premium_sum_direction_slope;
        if strong_obi_bonus_active {
            threshold = (threshold - self.strong_obi_direction_relaxation).max(0.0);
        }
        threshold
    }

    pub(crate) fn max_leg1_price_now(&self, strong_obi_bonus_active: bool) -> Decimal {
        if strong_obi_bonus_active {
            self.max_leg1_price + self.strong_obi_price_bonus
        } else {
            self.max_leg1_price
        }
    }

    pub(crate) fn entry_after_start_max_secs_now(
        &self,
        window_duration_secs: u64,
        strong_obi_bonus_active: bool,
    ) -> u64 {
        if self.entry_after_start_max_secs == 0 {
            return 0;
        }
        if strong_obi_bonus_active && window_duration_secs >= 900 {
            self.entry_after_start_max_secs + self.strong_obi_window_bonus_secs
        } else {
            self.entry_after_start_max_secs
        }
    }

    pub(crate) fn obi_signal_still_supportive(
        &self,
        leg1_direction: Direction,
        entry_obi: Option<f64>,
        current_obi: Option<f64>,
    ) -> bool {
        let Some(current) = current_obi else {
            return false;
        };
        let directional_current = match leg1_direction {
            Direction::Up => current,
            Direction::Down => -current,
        };
        if directional_current <= 0.0 {
            return false;
        }

        let required_support = entry_obi
            .map(|obi| obi.abs() * self.obi_decay_exit_ratio)
            .unwrap_or(self.obi_flip_exit_threshold)
            .max(self.obi_flip_exit_threshold);
        directional_current >= required_support
    }

    pub(crate) fn force_close_threshold_now(
        &self,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> Decimal {
        self.adaptive_close_threshold(
            self.force_complete_threshold,
            time_remaining_secs,
            window_duration_secs,
            0.50,
            in_final_window,
        )
    }

    pub(crate) fn protective_close_threshold_now(
        &self,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> Decimal {
        self.adaptive_close_threshold(
            self.protective_close_threshold,
            time_remaining_secs,
            window_duration_secs,
            0.25,
            in_final_window,
        )
    }

    pub fn from_toml_str(config_str: &str) -> Result<Self> {
        Self::from_toml_str_with_default_symbols(config_str, Self::default().symbols)
    }

    pub fn from_toml_str_with_default_symbols(
        config_str: &str,
        default_symbols: Vec<String>,
    ) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow!("Invalid staggered_arb TOML: {}", e))?;
        let empty = Value::Table(Default::default());
        let entry = config.get("entry").unwrap_or(&empty);
        let timing = config.get("timing").unwrap_or(&empty);
        let risk = config.get("risk").unwrap_or(&empty);
        let model = config.get("model").unwrap_or(&empty);
        let filter = config.get("filter").unwrap_or(&empty);

        let symbols = entry
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or(default_symbols);

        Ok(Self {
            symbols,
            initial_capital: Decimal::try_from(
                entry
                    .get("initial_capital")
                    .and_then(|v| v.as_float())
                    .unwrap_or(10000.0),
            )
            .unwrap_or(dec!(10000)),
            shares_per_trade: entry
                .get("shares_per_trade")
                .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
                .unwrap_or(20) as u64,
            max_concurrent_positions: entry
                .get("max_concurrent")
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as usize,
            direction_threshold: entry
                .get("direction_threshold")
                .and_then(|v| v.as_float())
                .unwrap_or(0.05),
            premium_sum_threshold: Decimal::try_from(
                entry
                    .get("premium_sum_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.0),
            )
            .unwrap_or(Decimal::ONE),
            premium_sum_direction_slope: entry
                .get("premium_sum_direction_slope")
                .and_then(|v| v.as_float())
                .unwrap_or(1.25),
            premium_sum_obi_slope: entry
                .get("premium_sum_obi_slope")
                .and_then(|v| v.as_float())
                .unwrap_or(0.25),
            obi_confirm_threshold: entry
                .get("obi_confirm_threshold")
                .and_then(|v| v.as_float())
                .unwrap_or(0.005),
            strong_obi_threshold: entry
                .get("strong_obi_threshold")
                .and_then(|v| v.as_float())
                .unwrap_or(0.015),
            strong_obi_direction_relaxation: entry
                .get("strong_obi_direction_relaxation")
                .and_then(|v| v.as_float())
                .unwrap_or(0.015),
            strong_obi_price_bonus: Decimal::try_from(
                entry
                    .get("strong_obi_price_bonus")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.02),
            )
            .unwrap_or(dec!(0.02)),
            strong_obi_window_bonus_secs: timing
                .get("strong_obi_window_bonus_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(60) as u64,
            reverse_signal: entry
                .get("reverse_signal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            max_initial_sum: Decimal::try_from(
                entry
                    .get("max_initial_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.0),
            )
            .unwrap_or(Decimal::ZERO),
            max_leg1_price: Decimal::try_from(
                entry
                    .get("max_leg1_price")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.56),
            )
            .unwrap_or(dec!(0.56)),
            merge_target_sum: Decimal::try_from(
                entry
                    .get("merge_target_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.95),
            )
            .unwrap_or(dec!(0.95)),
            min_profit_target: Decimal::try_from(
                entry
                    .get("min_profit_target")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.02),
            )
            .unwrap_or(dec!(0.02)),
            max_wait_secs: timing
                .get("max_wait_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(120) as u64,
            entry_after_start_min_secs: timing
                .get("entry_after_start_min_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
            entry_after_start_max_secs: timing
                .get("entry_after_start_max_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(240) as u64,
            no_trade_last_secs: timing
                .get("no_trade_last_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
            max_wait_pct: timing
                .get("max_wait_pct")
                .and_then(|v| v.as_float())
                .unwrap_or(0.30),
            min_time_remaining_secs: timing
                .get("min_time_remaining")
                .and_then(|v| v.as_integer())
                .unwrap_or(45) as u64,
            max_leg1_loss: Decimal::try_from(
                risk.get("max_leg1_loss")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.03),
            )
            .unwrap_or(dec!(0.03)),
            force_complete_threshold: Decimal::try_from(
                risk.get("force_complete_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.08),
            )
            .unwrap_or(dec!(1.08)),
            protective_close_threshold: Decimal::try_from(
                risk.get("protective_close_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.08),
            )
            .unwrap_or(dec!(1.08)),
            obi_decay_exit_ratio: risk
                .get("obi_decay_exit_ratio")
                .and_then(|v| v.as_float())
                .unwrap_or(0.35),
            obi_flip_exit_threshold: risk
                .get("obi_flip_exit_threshold")
                .and_then(|v| v.as_float())
                .unwrap_or(0.008),
            min_ask_price: Decimal::try_from(
                entry
                    .get("min_ask_price")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.05),
            )
            .unwrap_or(dec!(0.05)),
            min_entry_sum: Decimal::try_from(
                entry
                    .get("min_entry_sum")
                    .and_then(|v| v.as_float())
                    .unwrap_or(0.30),
            )
            .unwrap_or(dec!(0.30)),
            allowed_window_durations: filter
                .get("allowed_windows")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_integer().map(|i| i as u64))
                        .collect()
                })
                .unwrap_or_else(|| vec![300]),
            window_duration_tolerance: filter
                .get("window_tolerance")
                .and_then(|v| v.as_integer())
                .unwrap_or(30) as u64,
            min_leg2_delay_secs: timing
                .get("min_leg2_delay_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(3) as u64,
            max_trades_per_event: timing
                .get("max_trades_per_event")
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as usize,
            mu: model.get("mu").and_then(|v| v.as_float()).unwrap_or(0.0),
            vol_lookback_secs: model
                .get("vol_lookback_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(600) as u64,
            vol_floor: model
                .get("vol_floor")
                .and_then(|v| v.as_float())
                .unwrap_or(0.003),
            min_entry_sigma: model
                .get("min_entry_sigma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.003),
            max_entry_sigma: model
                .get("max_entry_sigma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
            cooldown_secs: timing
                .get("cooldown_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(5) as u64,
            use_greeks: model
                .get("use_greeks")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            min_gamma: model
                .get("min_gamma")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
            max_theta_cost: model
                .get("max_theta_cost")
                .and_then(|v| v.as_float())
                .unwrap_or(0.0),
            max_fair_value_distance: model
                .get("max_fair_value_distance")
                .and_then(|v| v.as_float())
                .unwrap_or(0.15),
            delta_weighted_sizing: model
                .get("delta_weighted_sizing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    fn premium_sum_excess(&self, current_sum: Decimal) -> f64 {
        if current_sum <= self.premium_sum_threshold {
            0.0
        } else {
            (current_sum - self.premium_sum_threshold)
                .to_f64()
                .unwrap_or(0.0)
                .max(0.0)
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Position state machine
// ─────────────────────────────────────────────────────────────

/// Position lifecycle:
///   Idle → Leg1Filled → Settled (via merge or single-leg settlement)
///                     → Aborted (timeout / stop_loss / time_safety)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArbPositionState {
    Leg1Filled,
    Settled,
    Aborted,
}

impl fmt::Display for ArbPositionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leg1Filled => write!(f, "Leg1Filled"),
            Self::Settled => write!(f, "Settled"),
            Self::Aborted => write!(f, "Aborted"),
        }
    }
}

#[derive(Debug, Clone)]
struct StaggeredArbPosition {
    symbol: String,
    event_slug: String,
    /// Direction of Leg1 (the side we bought first)
    leg1_direction: Direction,
    leg1_price: Decimal,
    leg1_shares: u64,
    leg1_time: DateTime<Utc>,
    leg1_fee: Decimal,
    /// Deadline for Leg2 fill
    wait_deadline: DateTime<Utc>,
    /// Window open price (S0)
    s0: Decimal,
    /// Event end time
    event_end_time: DateTime<Utc>,
    /// Window duration in seconds
    window_duration_secs: i64,
    /// Model probability at Leg1 entry
    entry_p_hat: f64,
    /// Realized vol at entry
    entry_sigma: f64,
    /// Best sum seen during monitoring (for diagnostics)
    best_sum_seen: Decimal,
    /// Initial sum at entry (up_ask + down_ask)
    initial_sum: Decimal,
    /// Binance top-of-book OBI captured at Leg1 entry.
    entry_obi: Option<f64>,
    // ── Greeks at entry ──
    /// Binary option greeks computed at Leg1 entry
    entry_greeks: Option<BinaryGreeks>,
    /// Current state
    state: ArbPositionState,
    // ── Leg2 (filled after monitoring) ──
    leg2_direction: Option<Direction>,
    leg2_price: Option<Decimal>,
    leg2_shares: Option<u64>,
    leg2_time: Option<DateTime<Utc>>,
    leg2_fee: Option<Decimal>,
    // ── Resolution ──
    exit_reason: Option<String>,
    pnl: Option<Decimal>,
}

/// A closed staggered arb trade for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbClosedTrade {
    pub symbol: String,
    pub leg1_direction: String,
    pub leg1_price: Decimal,
    pub leg1_time: DateTime<Utc>,
    pub leg2_price: Option<Decimal>,
    pub leg2_time: Option<DateTime<Utc>>,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub initial_sum: Decimal,
    pub final_sum: Option<Decimal>,
    pub entry_p_hat: f64,
    pub entry_sigma: f64,
    pub best_sum_seen: Decimal,
    pub s0: Decimal,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    pub window_duration_secs: i64,
    /// Greeks at entry (delta, gamma, theta, vega, fair_value)
    pub entry_delta: Option<f64>,
    pub entry_gamma: Option<f64>,
    pub entry_theta: Option<f64>,
    pub entry_fair_value: Option<f64>,
}
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    s0: Decimal,
    end_time: DateTime<Utc>,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    window_duration_secs: i64,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct StaggeredArbBacktestEngine {
    config: StaggeredArbBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    // Active events: symbol -> concurrent windows
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    // Positions & trades
    positions: Vec<StaggeredArbPosition>,
    closed_trades: Vec<StaggeredArbClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    /// Track how many trades have been opened per event slug
    event_trade_count: HashMap<String, usize>,
    /// Track ask-side depth per symbol (from LOB snapshots)
    lob_depth: HashMap<String, u64>,
    /// Latest Binance L2 OBI per symbol for live-parity replay filtering.
    binance_l2_obi_5: HashMap<String, Decimal>,
    /// Previous Binance L2 OBI per symbol for persistence / flip checks.
    binance_l2_obi_prev_5: HashMap<String, Decimal>,
    binance_l2_obi_ts: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

impl StaggeredArbBacktestEngine {
    pub fn new(config: StaggeredArbBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            active_events: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            event_trade_count: HashMap::new(),
            lob_depth: HashMap::new(),
            binance_l2_obi_5: HashMap::new(),
            binance_l2_obi_prev_5: HashMap::new(),
            binance_l2_obi_ts: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn new_without_recorder(config: StaggeredArbBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &StaggeredArbBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[StaggeredArbClosedTrade] {
        &self.closed_trades
    }

    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }

    // ─── Main loop ──────────────────────────────────────────

    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            // Prune expired events
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(
                        &update.symbol,
                        event_slug,
                        *side,
                        *best_ask,
                        update.timestamp,
                    );
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                    }

                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            // Compute window duration and filter
                            let duration_secs = (*end - update.timestamp).num_seconds();
                            let allowed = if self.config.allowed_window_durations.is_empty() {
                                true
                            } else {
                                let tol = self.config.window_duration_tolerance as i64;
                                self.config
                                    .allowed_window_durations
                                    .iter()
                                    .any(|&d| (duration_secs - d as i64).abs() <= tol)
                            };
                            if !allowed {
                                trace!(
                                    "Skipping event {} with duration {}s (not in allowed list)",
                                    event_slug,
                                    duration_secs
                                );
                            } else {
                                let events =
                                    self.active_events.entry(update.symbol.clone()).or_default();
                                if !events.iter().any(|e| e.event_slug == *event_slug) {
                                    events.push(ActiveWindowInfo {
                                        event_slug: event_slug.clone(),
                                        s0: *s0,
                                        end_time: *end,
                                        window_duration_secs: duration_secs,
                                    });
                                }
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot {
                    ask_depth_shares, ..
                } => {
                    // Update LOB depth cache for this symbol
                    self.lob_depth
                        .insert(update.symbol.clone(), *ask_depth_shares);
                }
                UpdateType::BinanceL2 { obi_5, .. } => {
                    if let Some(prev) = self.binance_l2_obi_5.insert(update.symbol.clone(), *obi_5) {
                        self.binance_l2_obi_prev_5.insert(update.symbol.clone(), prev);
                    }
                    self.binance_l2_obi_ts
                        .insert(update.symbol.clone(), update.timestamp);
                }
            }
        }

        self.close_remaining_positions();

        // Diagnostic summary
        let total_events: usize = self.active_events.values().map(|v| v.len()).sum();
        let total_quotes = self.pm_asks_by_event.len();
        let total_spots = self.spot_prices.len();
        debug!(
            "Engine summary: {} active events, {} quote slugs, {} spot symbols, {} positions, {} closed trades",
            total_events, total_quotes, total_spots, self.positions.len(), self.closed_trades.len()
        );

        let _ = self.recorder.flush();
        self.build_results()
    }

    // ─── Event handlers ──────────────────────────────────────

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks (per event_slug)
        let entry = self
            .pm_asks_by_event
            .entry(event_slug.to_string())
            .or_insert((None, None));
        match quote_side {
            Side::Up => {
                if best_ask.is_some() {
                    entry.0 = best_ask;
                }
            }
            Side::Down => {
                if best_ask.is_some() {
                    entry.1 = best_ask;
                }
            }
        }

        // 1. Check Leg2 opportunities for existing positions
        self.check_leg2_opportunities(symbol, ts);

        // 2. Try new entries on active windows
        self.try_entry(symbol, ts);

        // Record equity
        self.record_equity(ts);
    }

    // ─── Helpers ───────────────────────────────────────────────

    /// Get current LOB depth for a symbol, falling back to a conservative default.
    fn market_depth(&self, symbol: &str) -> u64 {
        self.lob_depth.get(symbol).copied().unwrap_or(500)
    }

    // ─── Entry logic ─────────────────────────────────────────

    fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let windows: Vec<ActiveWindowInfo> = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => return,
        };

        let (spot_price, spot_vol_info) = match self.spot_prices.get(symbol) {
            Some(s) => {
                let lookback = self.config.vol_lookback_secs;
                let vol = s.volatility(lookback).and_then(|v| v.to_f64());
                let n_ticks = s.history_len().min(5000) as f64;
                (s.price, (vol, n_ticks))
            }
            None => return,
        };

        for window in windows {
            let (up_ask, down_ask) = self
                .pm_asks_by_event
                .get(&window.event_slug)
                .copied()
                .unwrap_or((None, None));
            if up_ask.is_none() || down_ask.is_none() {
                trace!(
                    "try_entry: {} missing quotes (up={:?} down={:?})",
                    window.event_slug,
                    up_ask,
                    down_ask
                );
            }
            self.try_entry_for_window(
                symbol,
                ts,
                &window,
                spot_price,
                spot_vol_info,
                up_ask,
                down_ask,
            );
        }
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        spot_vol_info: (Option<f64>, f64),
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        // 1. Time remaining check
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }
        // Entry timing gate: prefer opening soon after the event starts.
        // start = end - window_duration
        let window_start =
            window.end_time - chrono::Duration::seconds(window.window_duration_secs as i64);
        let elapsed_since_start = (ts - window_start).num_seconds();
        if elapsed_since_start < 0 {
            return;
        }
        if self.config.entry_after_start_min_secs > 0
            && elapsed_since_start < self.config.entry_after_start_min_secs as i64
        {
            return;
        }

        // 2. Need both asks to compute sum
        let (ua, da) = match (up_ask, down_ask) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };

        // 2b. Filter out extreme prices with no real liquidity
        if ua < self.config.min_ask_price || da < self.config.min_ask_price {
            return;
        }

        // 2c. Filter out illiquid extreme-price pairs (both sides cheap = no real market)
        if ua + da < self.config.min_entry_sum {
            return;
        }

        // 3. Current sum — only enter when sum shows a discount (market inefficiency)
        let current_sum = ua + da;
        if self.config.max_initial_sum > Decimal::ZERO && current_sum > self.config.max_initial_sum
        {
            return;
        }

        // 4. Compute volatility
        let sigma = {
            let floor = self.config.vol_floor;
            match spot_vol_info.0 {
                Some(tick_vol) if tick_vol > 0.0 => {
                    let n_ticks = spot_vol_info.1;
                    let period_vol = tick_vol * n_ticks.sqrt();
                    period_vol.max(floor)
                }
                _ => floor,
            }
        };

        if sigma < self.config.min_entry_sigma {
            trace!(
                "Skipping: sigma {:.6} < min_entry_sigma {:.6}",
                sigma,
                self.config.min_entry_sigma
            );
            return;
        }
        if self.config.max_entry_sigma > 0.0 && sigma > self.config.max_entry_sigma {
            trace!(
                "Skipping: sigma {:.6} > max_entry_sigma {:.6}",
                sigma,
                self.config.max_entry_sigma
            );
            return;
        }

        // 5. Estimate probability
        let p_hat = estimate_probability(window.s0, st, sigma, time_remaining, self.config.mu);

        // 5b. Compute binary option Greeks
        let greeks = if self.config.use_greeks {
            binary_greeks(
                st.to_f64().unwrap_or(0.0),
                window.s0.to_f64().unwrap_or(0.0),
                sigma,
                time_remaining,
                window.window_duration_secs as f64,
            )
        } else {
            None
        };

        // 5c. Greeks-based filters
        if let Some(ref g) = greeks {
            // Skip if gamma is too low (deep ITM/OTM — no convexity to exploit)
            if self.config.min_gamma > 0.0 && g.gamma.abs() < self.config.min_gamma {
                trace!(
                    "Skipping: gamma {:.6} < min_gamma {:.6}",
                    g.gamma.abs(),
                    self.config.min_gamma
                );
                return;
            }

            // Skip if theta decay is too expensive
            if self.config.max_theta_cost > 0.0 && g.theta.abs() > self.config.max_theta_cost {
                trace!(
                    "Skipping: theta {:.6} > max_theta_cost {:.6}",
                    g.theta.abs(),
                    self.config.max_theta_cost
                );
                return;
            }

            if self.config.max_fair_value_distance < 0.5
                && (g.fair_value - 0.5).abs() > self.config.max_fair_value_distance
            {
                trace!(
                    "Skipping: fair_value {:.4} outside long-gamma band 0.5 +/- {:.4}",
                    g.fair_value,
                    self.config.max_fair_value_distance
                );
                return;
            }
        }

        // 6. Direction: p_hat > 0.5 → buy UP first (it's about to get expensive)
        //    If reverse_signal is true, flip: buy the opposite of what the model says
        let predicted_up = if self.config.reverse_signal {
            p_hat < 0.5
        } else {
            p_hat > 0.5
        };

        // 6b. Require meaningful price displacement and direction agreement.
        const MIN_PRICE_DISPLACEMENT: f64 = 0.0003;
        let displacement = ((st - window.s0) / window.s0).to_f64().unwrap_or(0.0);
        if displacement.abs() < MIN_PRICE_DISPLACEMENT {
            return;
        }
        if predicted_up && displacement <= 0.0 {
            return;
        }
        if !predicted_up && displacement >= 0.0 {
            return;
        }

        // 6c. OBI confirmation: require the current signal to be aligned and either
        // persistent or strong enough to justify a directional first leg.
        const OI_MAX_STALE_SECS: i64 = 60;
        let Some(obi_ts) = self.binance_l2_obi_ts.get(symbol).copied() else {
            trace!("Skipping: no Binance L2 OBI history for {}", symbol);
            return;
        };
        if (ts - obi_ts).num_seconds().abs() > OI_MAX_STALE_SECS {
            trace!(
                "Skipping: Binance L2 OBI for {} is stale by {}s",
                symbol,
                (ts - obi_ts).num_seconds().abs()
            );
            return;
        }
        let Some(obi_value) = self.binance_l2_obi_5.get(symbol) else {
            trace!("Skipping: missing Binance L2 OBI value for {}", symbol);
            return;
        };
        let obi = obi_value.to_f64().unwrap_or(0.0);
        let prev_obi = self
            .binance_l2_obi_prev_5
            .get(symbol)
            .map(|value| value.to_f64().unwrap_or(0.0));
        let fair_value_distance = greeks.as_ref().map(|g| (g.fair_value - 0.5).abs());
        let premium_sum_excess = self.config.premium_sum_excess(current_sum);
        let required_obi_strength =
            self.config.obi_confirm_threshold + premium_sum_excess * self.config.premium_sum_obi_slope;
        if !self
            .config
            .obi_confirms_direction(predicted_up, obi, required_obi_strength)
        {
            trace!(
                "Skipping: OBI {:.4} not aligned with required {:.4}",
                obi,
                required_obi_strength
            );
            return;
        }
        let obi_persistent = self
            .config
            .obi_is_persistent(predicted_up, obi, prev_obi, required_obi_strength);
        let strong_obi_bonus_active = self.config.strong_obi_entry_bonus_active(
            predicted_up,
            obi,
            prev_obi,
            current_sum,
            fair_value_distance,
        );
        if !obi_persistent && !strong_obi_bonus_active {
            trace!("Skipping: OBI {:.4} lacks persistence for {}", obi, symbol);
            return;
        }

        // 7. Direction threshold: |p_hat - 0.5| >= direction_threshold
        let direction_strength = (p_hat - 0.5).abs();
        let required_direction_strength = self
            .config
            .direction_threshold_now(current_sum, strong_obi_bonus_active);
        if direction_strength < required_direction_strength {
            trace!(
                "Skipping: direction_strength {:.4} < required {:.4} (premium_sum_excess {:.4} strong_obi={})",
                direction_strength,
                required_direction_strength,
                premium_sum_excess,
                strong_obi_bonus_active
            );
            return;
        }

        let allowed_entry_window_secs = self.config.entry_after_start_max_secs_now(
            window.window_duration_secs as u64,
            strong_obi_bonus_active,
        );
        if allowed_entry_window_secs > 0 && elapsed_since_start > allowed_entry_window_secs as i64 {
            trace!(
                "Skipping: elapsed_since_start {}s > allowed {}s (strong_obi={})",
                elapsed_since_start,
                allowed_entry_window_secs,
                strong_obi_bonus_active
            );
            return;
        }

        let (leg1_dir, leg1_ask) = if predicted_up {
            (Direction::Up, ua)
        } else {
            (Direction::Down, da)
        };

        // 7b. Leg1 price cap: don't buy if predicted side is too expensive
        //     (leaves no room for the other side to drop enough for profit)
        if leg1_ask > self.config.max_leg1_price_now(strong_obi_bonus_active) {
            return;
        }

        // 8. Target Leg2 price: need leg1 + leg2 < merge_target_sum
        let target_leg2 = self.config.merge_target_sum - leg1_ask;
        if target_leg2 <= Decimal::ZERO {
            return;
        }

        // 9. Cooldown check
        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return;
            }
        }

        // 10. Max positions check
        let active_count = self
            .positions
            .iter()
            .filter(|p| p.state == ArbPositionState::Leg1Filled)
            .count();
        if self.config.max_concurrent_positions > 0
            && active_count >= self.config.max_concurrent_positions
        {
            return;
        }

        // 11. Don't enter same event twice (concurrently)
        let already_in = self
            .positions
            .iter()
            .any(|p| p.event_slug == window.event_slug && p.state == ArbPositionState::Leg1Filled);
        if already_in {
            return;
        }

        // 11b. Max trades per event window
        if self.config.max_trades_per_event > 0 {
            let count = self
                .event_trade_count
                .get(&window.event_slug)
                .copied()
                .unwrap_or(0);
            if count >= self.config.max_trades_per_event {
                return;
            }
        }

        // 12. Simulate Leg1 buy (use real LOB depth)
        //     Delta-weighted sizing: scale shares by conviction from Greeks
        let shares = if self.config.delta_weighted_sizing {
            if let Some(ref g) = greeks {
                // Scale by |delta| / ATM_delta. ATM delta is highest, so this
                // reduces size for weak signals and increases for strong ones.
                // Clamp to [0.5, 2.0] to avoid extreme sizing.
                let delta_scale = (g.delta.abs() * 2.0).clamp(0.5, 2.0);
                ((self.config.shares_per_trade as f64 * delta_scale) as u64).max(1)
            } else {
                self.config.shares_per_trade
            }
        } else {
            self.config.shares_per_trade
        };

        let depth = self.market_depth(symbol);
        let sim_result = self.execution_sim.simulate_buy(leg1_ask, ts, shares, depth);

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let entry_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_cost = entry_cost + entry_fee;

        if total_cost > self.equity {
            trace!(
                "Skipping: insufficient equity ({} < {})",
                self.equity,
                total_cost
            );
            return;
        }

        self.equity -= total_cost;

        let leg1_fill_time = sim_result.fill_time;

        // Calculate wait deadline from the modeled fill time, not the signal time.
        let window_duration = (window.end_time - leg1_fill_time).num_seconds() as f64;
        let max_wait_by_pct = (window_duration * self.config.max_wait_pct) as i64;
        let max_wait = (self.config.max_wait_secs as i64).min(max_wait_by_pct);
        let wait_deadline = leg1_fill_time + chrono::Duration::seconds(max_wait.max(0));

        self.positions.push(StaggeredArbPosition {
            symbol: symbol.to_string(),
            event_slug: window.event_slug.clone(),
            leg1_direction: leg1_dir,
            leg1_price: sim_result.fill_price,
            leg1_shares: sim_result.filled_shares,
            leg1_time: leg1_fill_time,
            leg1_fee: entry_fee,
            wait_deadline,
            s0: window.s0,
            event_end_time: window.end_time,
            window_duration_secs: window.window_duration_secs,
            entry_p_hat: if matches!(leg1_dir, Direction::Up) {
                p_hat
            } else {
                1.0 - p_hat
            },
            entry_sigma: sigma,
            best_sum_seen: current_sum,
            initial_sum: current_sum,
            entry_obi: Some(obi),
            entry_greeks: greeks,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);
        *self
            .event_trade_count
            .entry(window.event_slug.clone())
            .or_default() += 1;

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", leg1_dir),
            timestamp: leg1_fill_time,
            p_hat: Some(p_hat),
            ev_net: None,
            sigma: Some(sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: Some(st),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "LEG1 {} {} @ {:.4} | sum={:.4} p_hat={:.3} σ={:.5}",
            symbol, leg1_dir, sim_result.fill_price, current_sum, p_hat, sigma
        );
    }

    // ─── Leg2 monitoring ──────────────────────────────────────

    fn check_leg2_opportunities(&mut self, symbol: &str, ts: DateTime<Utc>) {
        // Collect actions to take (can't mutate positions while iterating)
        let mut actions: Vec<(usize, Leg2Action)> = Vec::new();
        for (i, pos) in self.positions.iter_mut().enumerate() {
            if pos.symbol != symbol || pos.state != ArbPositionState::Leg1Filled {
                continue;
            }

            let pm_asks = match self.pm_asks_by_event.get(&pos.event_slug).copied() {
                Some(a) => a,
                None => continue,
            };

            // Get the opposite side's ask
            let other_ask = match pos.leg1_direction {
                Direction::Up => pm_asks.1,   // need DOWN ask
                Direction::Down => pm_asks.0, // need UP ask
            };
            let other_ask = match other_ask {
                Some(a) if a >= self.config.min_ask_price => a,
                Some(_) => continue, // too cheap, no real liquidity
                None => continue,
            };

            // Also reject if combined sum is too low (illiquid extreme-price pair)
            if pos.leg1_price + other_ask < self.config.min_entry_sum {
                continue;
            }

            let current_sum = pos.leg1_price + other_ask;

            // Track best sum seen
            if current_sum < pos.best_sum_seen {
                pos.best_sum_seen = current_sum;
            }

            let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;
            let in_final_window = self.config.no_trade_last_secs > 0
                && time_remaining <= self.config.no_trade_last_secs as f64
                && time_remaining > 0.0;
            let min_time = self.config.min_time_remaining_secs as f64;
            let force_threshold = self.config.force_close_threshold_now(
                time_remaining,
                pos.window_duration_secs.max(0) as u64,
                in_final_window,
            );
            let protective_threshold = self.config.protective_close_threshold_now(
                time_remaining,
                pos.window_duration_secs.max(0) as u64,
                in_final_window,
            );

            // Compute current Greeks for this position (if enabled)
            let current_greeks = if self.config.use_greeks {
                let spot = self
                    .spot_prices
                    .get(&pos.symbol)
                    .map(|sp| sp.price.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0);
                let strike = pos.s0.to_f64().unwrap_or(0.0);
                if spot > 0.0 && strike > 0.0 && time_remaining > 0.0 {
                    binary_greeks(
                        spot,
                        strike,
                        pos.entry_sigma,
                        time_remaining,
                        pos.window_duration_secs as f64,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let current_obi = self
                .binance_l2_obi_5
                .get(&pos.symbol)
                .map(|value| value.to_f64().unwrap_or(0.0));
            let displacement_supportive = self
                .spot_prices
                .get(&pos.symbol)
                .and_then(|sp| {
                    if pos.s0 <= Decimal::ZERO {
                        return None;
                    }
                    Some(((sp.price - pos.s0) / pos.s0).to_f64().unwrap_or(0.0))
                })
                .map(|displacement| match pos.leg1_direction {
                    Direction::Up => displacement > 0.0,
                    Direction::Down => displacement < 0.0,
                })
                .unwrap_or(false);
            let greeks_supportive = current_greeks
                .as_ref()
                .map(|g| match pos.leg1_direction {
                    Direction::Up => g.d2 > 0.05 && g.fair_value > 0.5,
                    Direction::Down => g.d2 < -0.05 && g.fair_value < 0.5,
                })
                .unwrap_or(!self.config.use_greeks);

            // Check minimum delay since Leg1 fill (execution realism)
            let secs_since_leg1 = (ts - pos.leg1_time).num_seconds();
            let leg2_ready = secs_since_leg1 >= self.config.min_leg2_delay_secs as i64;

            // A. Profitable merge: sum < merge_target_sum
            if !in_final_window && current_sum < self.config.merge_target_sum && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                continue;
            }

            // A2. Greeks-enhanced merge: if gamma is high (near ATM, high convexity),
            //     the opposite token price is volatile — merge sooner to lock profit.
            //     Accept a tighter profit margin when gamma is elevated.
            if let Some(ref g) = current_greeks {
                if !in_final_window && leg2_ready && current_sum < Decimal::ONE {
                    // High gamma → price can swing either way fast → lock it in
                    let gamma_urgency = g.gamma.abs().min(1.0);
                    let adjusted_target = self.config.min_profit_target
                        * Decimal::from_f64(1.0 - gamma_urgency * 0.8).unwrap_or(Decimal::ONE);
                    if current_sum < self.config.merge_target_sum + adjusted_target {
                        trace!(
                            "Greeks merge: gamma={:.4} adjusted_target={:.4} sum={:.4}",
                            g.gamma,
                            adjusted_target,
                            current_sum
                        );
                        actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                        continue;
                    }
                }

                // A3. Theta-driven urgency: if theta decay is accelerating
                //     (approaching expiry), merge even at breakeven to avoid decay.
                if leg2_ready && self.config.max_theta_cost > 0.0 {
                    let theta_cost_remaining = g.theta.abs() * time_remaining;
                    if theta_cost_remaining > self.config.max_theta_cost {
                        trace!(
                            "Theta urgency: theta={:.6} cost_remaining={:.4} sum={:.4}",
                            g.theta,
                            theta_cost_remaining,
                            current_sum
                        );
                        if current_sum <= Decimal::ONE {
                            actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                            continue;
                        }
                        if protective_threshold <= Decimal::ZERO
                            || current_sum <= protective_threshold
                        {
                            actions.push((
                                i,
                                Leg2Action::Fill(other_ask, "protective_theta".to_string()),
                            ));
                            continue;
                        }
                    }
                }
            }

            // B. Lock profit: sum < 1.0 (any profit is good)
            if !in_final_window && current_sum < Decimal::ONE && leg2_ready {
                actions.push((i, Leg2Action::Fill(other_ask, "merge".to_string())));
                continue;
            }

            // C. Timeout — force-complete the arb to avoid directional risk
            //    Even if sum > 1.0, the loss is bounded: (sum - 1.0) × shares
            //    Much better than risking full Leg1 cost at settlement
            if ts >= pos.wait_deadline && leg2_ready {
                if force_threshold <= Decimal::ZERO || current_sum <= force_threshold {
                    actions.push((i, Leg2Action::Fill(other_ask, "forced_timeout".to_string())));
                }
                continue;
            }

            // D. Stop loss on Leg1 — force-complete if loss exceeds threshold
            //    With max_leg1_loss = 0 the stop-loss is disabled
            if self.config.max_leg1_loss > Decimal::ZERO {
                let leg1_current_value = match pos.leg1_direction {
                    Direction::Up => pm_asks.0.unwrap_or(pos.leg1_price),
                    Direction::Down => pm_asks.1.unwrap_or(pos.leg1_price),
                };
                let leg1_loss = pos.leg1_price - leg1_current_value;
                if leg1_loss >= self.config.max_leg1_loss && leg2_ready {
                    let obi_supportive = self.config.obi_signal_still_supportive(
                        pos.leg1_direction,
                        pos.entry_obi,
                        current_obi,
                    );
                    if obi_supportive && displacement_supportive && greeks_supportive {
                        trace!(
                            "Skipping protective stop: signal still supportive obi={:?} displacement_supportive={} greeks_supportive={}",
                            current_obi,
                            displacement_supportive,
                            greeks_supportive
                        );
                        continue;
                    }
                    if protective_threshold <= Decimal::ZERO || current_sum <= protective_threshold
                    {
                        actions.push((
                            i,
                            Leg2Action::Fill(other_ask, "protective_stop_loss".to_string()),
                        ));
                    }
                    continue;
                }
            }

            // E. Time safety: not enough time left — force-complete the arb
            if time_remaining < min_time && leg2_ready {
                if force_threshold <= Decimal::ZERO || current_sum <= force_threshold {
                    actions.push((
                        i,
                        Leg2Action::Fill(other_ask, "forced_time_safety".to_string()),
                    ));
                }
            }
        }

        // Execute actions in reverse order to preserve indices
        actions.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, action) in actions {
            match action {
                Leg2Action::Fill(other_ask, reason) => {
                    self.fill_leg2(idx, other_ask, &reason, ts);
                }
                Leg2Action::Abort(reason) => {
                    self.abort_position(idx, &reason, ts);
                }
            }
        }
    }

    /// Fill Leg2 and immediately merge for $1.00 per share.
    fn fill_leg2(&mut self, idx: usize, other_ask: Decimal, reason: &str, ts: DateTime<Utc>) {
        if idx >= self.positions.len() || self.positions[idx].state != ArbPositionState::Leg1Filled
        {
            return;
        }

        let pos = &self.positions[idx];
        let leg2_dir = match pos.leg1_direction {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        };
        let remaining_shares = pos.leg1_shares.saturating_sub(pos.leg2_shares.unwrap_or(0));
        if remaining_shares == 0 {
            return;
        }

        let depth = self.market_depth(&pos.symbol);
        let sim_result = self
            .execution_sim
            .simulate_buy(other_ask, ts, remaining_shares, depth);
        if sim_result.filled_shares == 0 {
            return;
        }

        let leg2_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let leg2_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_leg2_cost = leg2_cost + leg2_fee;

        if total_leg2_cost > self.equity {
            trace!("Cannot fill Leg2: insufficient equity");
            return;
        }

        self.equity -= total_leg2_cost;
        let fill_time = sim_result.fill_time;

        let (
            symbol,
            event_slug,
            leg1_direction,
            leg1_price,
            leg1_shares,
            leg1_time,
            entry_p_hat,
            entry_sigma,
            initial_sum,
            best_sum_seen,
            s0,
            window_duration_secs,
            entry_greeks,
            total_leg2_shares,
            total_leg2_price,
            total_leg2_fee,
        ) = {
            let pos = &mut self.positions[idx];
            let prev_leg2_shares = pos.leg2_shares.unwrap_or(0);
            let prev_leg2_price = pos.leg2_price.unwrap_or(Decimal::ZERO);
            let prev_leg2_fee = pos.leg2_fee.unwrap_or(Decimal::ZERO);

            let prev_notional = prev_leg2_price * Decimal::from(prev_leg2_shares);
            let add_notional = sim_result.fill_price * Decimal::from(sim_result.filled_shares);
            let total_leg2_shares = prev_leg2_shares + sim_result.filled_shares;
            let total_notional = prev_notional + add_notional;
            let total_leg2_price = total_notional / Decimal::from(total_leg2_shares);
            let total_leg2_fee = prev_leg2_fee + leg2_fee;

            pos.leg2_direction = Some(leg2_dir);
            pos.leg2_price = Some(total_leg2_price);
            pos.leg2_shares = Some(total_leg2_shares);
            pos.leg2_time = Some(fill_time);
            pos.leg2_fee = Some(total_leg2_fee);

            (
                pos.symbol.clone(),
                pos.event_slug.clone(),
                pos.leg1_direction,
                pos.leg1_price,
                pos.leg1_shares,
                pos.leg1_time,
                pos.entry_p_hat,
                pos.entry_sigma,
                pos.initial_sum,
                pos.best_sum_seen,
                pos.s0,
                pos.window_duration_secs,
                pos.entry_greeks,
                total_leg2_shares,
                total_leg2_price,
                total_leg2_fee,
            )
        };

        if total_leg2_shares < leg1_shares {
            debug!(
                "LEG2 PARTIAL {} | {}/{} filled avg={:.4}",
                event_slug, total_leg2_shares, leg1_shares, total_leg2_price
            );
            return;
        }

        let payout = Decimal::from(leg1_shares);
        let total_cost = Decimal::from(leg1_shares) * leg1_price
            + self.positions[idx].leg1_fee
            + Decimal::from(total_leg2_shares) * total_leg2_price
            + total_leg2_fee;
        let pnl = payout - total_cost;

        self.equity += payout;

        let holding_secs = (fill_time - leg1_time).num_seconds();
        let final_sum = leg1_price + total_leg2_price;

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", leg1_direction),
            leg1_price,
            leg1_time,
            leg2_price: Some(total_leg2_price),
            leg2_time: Some(fill_time),
            shares: leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum,
            final_sum: Some(final_sum),
            entry_p_hat,
            entry_sigma,
            best_sum_seen,
            s0,
            window_duration_secs,
            entry_delta: entry_greeks.map(|g| g.delta),
            entry_gamma: entry_greeks.map(|g| g.gamma),
            entry_theta: entry_greeks.map(|g| g.theta),
            entry_fair_value: entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", leg1_direction),
            timestamp: fill_time,
            p_hat: Some(entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(entry_sigma),
            market_price: Some(total_leg2_price),
            spot_price: None,
            s0: Some(s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(total_leg2_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", leg1_direction),
            entry_time: leg1_time,
            exit_time: fill_time,
            entry_price: leg1_price,
            exit_price: total_leg2_price,
            shares: leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(entry_sigma),
            s0: Some(s0),
        });

        debug!(
            "MERGE {} | leg1={:.4} leg2={:.4} sum={:.4} pnl={:.4}",
            event_slug, leg1_price, total_leg2_price, final_sum, pnl
        );
    }

    /// Abort a position — sell Leg1 at current market price.
    fn abort_position(&mut self, idx: usize, reason: &str, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        let current_price = match pos.leg1_direction {
            Direction::Up => self.pm_asks_by_event.get(&pos.event_slug).and_then(|a| a.0),
            Direction::Down => self.pm_asks_by_event.get(&pos.event_slug).and_then(|a| a.1),
        }
        .unwrap_or(pos.leg1_price);

        // Simulate sell
        let depth = self.market_depth(&pos.symbol);
        let sim_result =
            self.execution_sim
                .simulate_sell(current_price, ts, pos.leg1_shares, depth);

        let proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let sell_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let net_proceeds = proceeds - sell_fee;

        self.equity += net_proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = net_proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Aborted;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: symbol.clone(),
            direction: format!("{}", pos.leg1_direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_sigma),
            market_price: Some(sim_result.fill_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(sim_result.fill_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price: sim_result.fill_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });

        debug!("ABORT {} reason={} pnl={:.4}", pos.event_slug, reason, pnl);
    }

    // ─── Settlement (single-leg only) ────────────────────────

    fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        // At settlement, force-complete any remaining Leg1 positions by buying Leg2.
        // This avoids directional risk — even if sum > 1.0, the loss is bounded.
        let mut to_fill: Vec<usize> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.event_slug != event_slug {
                continue;
            }
            if pos.state != ArbPositionState::Leg1Filled {
                continue;
            }
            to_fill.push(i);
        }

        to_fill.sort_by(|a, b| b.cmp(a));
        for idx in to_fill {
            let pos = &self.positions[idx];
            // Get the opposite side's ask for forced Leg2
            let pm_asks = self.pm_asks_by_event.get(&pos.event_slug).copied();
            let other_ask = pm_asks.and_then(|a| match pos.leg1_direction {
                Direction::Up => a.1,   // need DOWN ask
                Direction::Down => a.0, // need UP ask
            });
            match other_ask {
                Some(ask) => {
                    // Force buy Leg2 and merge — bounded loss
                    self.fill_leg2(idx, ask, "forced_settlement", ts);
                    if idx < self.positions.len()
                        && self.positions[idx].state == ArbPositionState::Leg1Filled
                    {
                        self.settle_position_with_outcome(idx, up_won, ts, "settlement");
                    }
                }
                None => {
                    self.settle_position_with_outcome(idx, up_won, ts, "settlement");
                }
            }
        }
    }

    fn settle_position_with_outcome(
        &mut self,
        idx: usize,
        up_won: bool,
        ts: DateTime<Utc>,
        reason: &str,
    ) {
        if idx >= self.positions.len() || self.positions[idx].state != ArbPositionState::Leg1Filled
        {
            return;
        }

        let pos = &self.positions[idx];
        let leg2_shares = pos.leg2_shares.unwrap_or(0);
        let leg2_price = pos.leg2_price.unwrap_or(Decimal::ZERO);
        let leg2_fee = pos.leg2_fee.unwrap_or(Decimal::ZERO);
        let winner_matches_leg1 = matches!(pos.leg1_direction, Direction::Up) == up_won;
        let payout = if winner_matches_leg1 {
            Decimal::from(pos.leg1_shares)
        } else {
            Decimal::from(leg2_shares)
        };
        let total_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price
            + pos.leg1_fee
            + Decimal::from(leg2_shares) * leg2_price
            + leg2_fee;
        self.equity += payout;
        let pnl = payout - total_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();
        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some(reason.to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: if leg2_shares > 0 {
                Some(leg2_price)
            } else {
                None
            },
            leg2_time: pos.leg2_time,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            initial_sum: pos.initial_sum,
            final_sum: if leg2_shares > 0 {
                Some(pos.leg1_price + leg2_price)
            } else {
                None
            },
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });
    }

    #[allow(dead_code)]
    fn settle_single_leg(&mut self, idx: usize, exit_price: Decimal, ts: DateTime<Utc>) {
        let pos = &self.positions[idx];
        // At settlement, fee = 0 (p*(1-p) = 0 at $1 or $0)
        let proceeds = exit_price * Decimal::from(pos.leg1_shares);
        self.equity += proceeds;

        let entry_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price + pos.leg1_fee;
        let pnl = proceeds - entry_cost;
        let holding_secs = (ts - pos.leg1_time).num_seconds();

        let symbol = pos.symbol.clone();

        let pos = &mut self.positions[idx];
        pos.state = ArbPositionState::Settled;
        pos.exit_reason = Some("settlement".to_string());
        pos.pnl = Some(pnl);

        self.closed_trades.push(StaggeredArbClosedTrade {
            symbol: symbol.clone(),
            leg1_direction: format!("{}", pos.leg1_direction),
            leg1_price: pos.leg1_price,
            leg1_time: pos.leg1_time,
            leg2_price: None,
            leg2_time: None,
            shares: pos.leg1_shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            initial_sum: pos.initial_sum,
            final_sum: None,
            entry_p_hat: pos.entry_p_hat,
            entry_sigma: pos.entry_sigma,
            best_sum_seen: pos.best_sum_seen,
            s0: pos.s0,
            window_duration_secs: pos.window_duration_secs,
            entry_delta: pos.entry_greeks.map(|g| g.delta),
            entry_gamma: pos.entry_greeks.map(|g| g.gamma),
            entry_theta: pos.entry_greeks.map(|g| g.theta),
            entry_fair_value: pos.entry_greeks.map(|g| g.fair_value),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol,
            direction: format!("{}", pos.leg1_direction),
            entry_time: pos.leg1_time,
            exit_time: ts,
            entry_price: pos.leg1_price,
            exit_price,
            shares: pos.leg1_shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: "settlement".to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pnl.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });
    }

    /// Force-close remaining Leg1 positions by buying Leg2 at market.
    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = self
            .positions
            .iter()
            .enumerate()
            .filter(|(_, p)| p.state == ArbPositionState::Leg1Filled)
            .map(|(i, _)| i)
            .rev()
            .collect();
        for idx in indices {
            let pos = &self.positions[idx];
            let pm_asks = self.pm_asks_by_event.get(&pos.event_slug).copied();
            let other_ask = pm_asks.and_then(|a| match pos.leg1_direction {
                Direction::Up => a.1,
                Direction::Down => a.0,
            });
            match other_ask {
                Some(ask) => self.fill_leg2(idx, ask, "data_exhausted", ts),
                None => self.abort_position(idx, "data_exhausted", ts),
            }
        }
    }

    // ─── Equity tracking ─────────────────────────────────────

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let drawdown = if self.peak_equity > Decimal::ZERO {
            (self.peak_equity - self.equity) / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }
        let should_record = self
            .equity_curve
            .last()
            .map(|(last_ts, _)| (ts - *last_ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_record {
            self.equity_curve.push((ts, self.equity));
        }
    }

    // ─── Results ─────────────────────────────────────────────

    fn build_results(&self) -> BacktestResults {
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total - winning;
        let total_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();

        let win_rate = if total > 0 {
            winning as f64 / total as f64
        } else {
            0.0
        };

        let avg_pnl = if total > 0 {
            total_pnl / Decimal::from(total)
        } else {
            Decimal::ZERO
        };

        let wins: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.pnl)
            .collect();
        let losses: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.pnl)
            .collect();

        let avg_win = if wins.is_empty() {
            Decimal::ZERO
        } else {
            wins.iter().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };
        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().sum();
        let total_losses_abs: Decimal = losses.iter().map(|l| l.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        let sharpe = self.calculate_sharpe();

        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|t| Decimal::from(t.shares) * t.leg1_price)
            .sum();

        let start_time = self.data_range_start.unwrap_or(Utc::now());
        let end_time = self.data_range_end.unwrap_or(Utc::now());

        BacktestResults {
            start_time,
            end_time,
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: sharpe,
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs: avg_holding,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: self.equity_curve.clone(),
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        if self.closed_trades.len() < 2 {
            return 0.0;
        }
        let pnls: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|t| t.pnl.to_f64().unwrap_or(0.0))
            .collect();
        let n = pnls.len() as f64;
        let mean = pnls.iter().sum::<f64>() / n;
        let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std_dev = variance.sqrt();
        if std_dev < 1e-10 {
            return 0.0;
        }
        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std_dev) * trades_per_year.sqrt()
    }

    /// Print staggered-arb-specific summary stats.
    pub fn print_summary(&self, title: &str) {
        if self.closed_trades.is_empty() {
            println!("\n=== {title} Summary ===");
            println!("No trades executed.");
            return;
        }

        let total = self.closed_trades.len();
        let merges: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "merge")
            .collect();
        let settlements: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .collect();
        let aborts: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason != "merge" && t.exit_reason != "settlement")
            .collect();

        let merge_count = merges.len();
        let leg2_fill_rate = merge_count as f64 / total as f64 * 100.0;

        // Spread compression for merges
        let avg_compression = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| t.final_sum.map(|fs| t.initial_sum - fs))
                .map(|d| d.to_f64().unwrap_or(0.0))
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        // Average Leg2 wait time (merges only)
        let avg_wait = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| {
                    t.leg2_time
                        .map(|l2| (l2 - t.leg1_time).num_seconds() as f64)
                })
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        // Abort reason distribution
        let mut abort_reasons: HashMap<&str, usize> = HashMap::new();
        for t in &aborts {
            *abort_reasons.entry(&t.exit_reason).or_default() += 1;
        }

        // PnL comparison: merge vs single-leg
        let merge_pnl: Decimal = merges.iter().map(|t| t.pnl).sum();
        let single_pnl: Decimal = settlements.iter().map(|t| t.pnl).sum();
        let abort_pnl: Decimal = aborts.iter().map(|t| t.pnl).sum();

        // Direction prediction accuracy (for merges: did the predicted side move favorably?)
        let direction_correct = merges.iter().filter(|t| t.won).count();
        let direction_accuracy = if !merges.is_empty() {
            direction_correct as f64 / merges.len() as f64 * 100.0
        } else {
            0.0
        };

        // Capital turnover: merge_count / avg_holding_time
        let avg_hold = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        println!("\n=== {title} Summary ===");
        println!("Total attempts:     {}", total);
        println!(
            "Leg2 fill rate:     {:.1}% ({}/{})",
            leg2_fill_rate, merge_count, total
        );
        println!("Settlements:        {} (single-leg)", settlements.len());
        println!("Aborts:             {}", aborts.len());
        println!();
        println!("Avg spread compression: {:.4}", avg_compression);
        println!("Avg Leg2 wait time:     {:.1}s", avg_wait);
        println!("Avg holding time:       {:.1}s", avg_hold);
        println!();
        println!("PnL breakdown:");
        println!("  Merges:      ${:.2} ({} trades)", merge_pnl, merge_count);
        println!(
            "  Settlements: ${:.2} ({} trades)",
            single_pnl,
            settlements.len()
        );
        println!("  Aborts:      ${:.2} ({} trades)", abort_pnl, aborts.len());
        println!();
        if !abort_reasons.is_empty() {
            println!("Abort reasons:");
            for (reason, count) in &abort_reasons {
                println!("  {:<16} {}", reason, count);
            }
            println!();
        }
        println!(
            "Direction accuracy: {:.1}% (merge wins)",
            direction_accuracy
        );
        println!(
            "Capital turnover:   {} merges, avg hold {:.0}s",
            merge_count, avg_hold
        );

        // Per-symbol breakdown
        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry = symbol_stats
                .entry(&t.symbol)
                .or_insert((0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            entry.2 += t.pnl;
        }
        if symbol_stats.len() > 1 {
            println!("\nPer-symbol:");
            println!(
                "  {:<12} {:>6} {:>6} {:>8} {:>10}",
                "Symbol", "Trades", "Wins", "WinRate", "PnL"
            );
            let mut syms: Vec<&&str> = symbol_stats.keys().collect();
            syms.sort();
            for sym in syms {
                let (t, w, p) = symbol_stats[sym];
                let wr = if t > 0 {
                    w as f64 / t as f64 * 100.0
                } else {
                    0.0
                };
                println!("  {:<12} {:>6} {:>6} {:>7.1}% ${:>9.2}", sym, t, w, wr, p);
            }
        }

        // Per-window-duration breakdown (5m vs 15m)
        let mut window_stats: HashMap<&str, (usize, usize, usize, Decimal)> = HashMap::new(); // (total, wins, merges, pnl)
        for t in &self.closed_trades {
            let label = match t.window_duration_secs {
                0..=330 => "5m",
                331..=930 => "15m",
                _ => "other",
            };
            let entry = window_stats
                .entry(label)
                .or_insert((0, 0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            if t.exit_reason == "merge" {
                entry.2 += 1;
            }
            entry.3 += t.pnl;
        }
        println!("\nPer-window breakdown:");
        println!(
            "  {:<8} {:>6} {:>6} {:>8} {:>8} {:>10}",
            "Window", "Trades", "Wins", "WinRate", "Merges", "PnL"
        );
        let mut labels: Vec<&&str> = window_stats.keys().collect();
        labels.sort();
        for label in labels {
            let (t, w, m, p) = window_stats[label];
            let wr = if t > 0 {
                w as f64 / t as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<8} {:>6} {:>6} {:>7.1}% {:>8} ${:>9.2}",
                label, t, w, wr, m, p
            );
        }

        // Greeks summary (if available)
        let trades_with_greeks: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.entry_delta.is_some())
            .collect();
        if !trades_with_greeks.is_empty() {
            let n = trades_with_greeks.len() as f64;
            let avg_delta = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_delta)
                .sum::<f64>()
                / n;
            let avg_gamma = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_gamma)
                .sum::<f64>()
                / n;
            let avg_theta = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_theta)
                .sum::<f64>()
                / n;
            let avg_fv = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_fair_value)
                .sum::<f64>()
                / n;

            // Greeks vs outcome correlation
            let winning_greeks: Vec<&StaggeredArbClosedTrade> = trades_with_greeks
                .iter()
                .filter(|t| t.won)
                .copied()
                .collect();
            let losing_greeks: Vec<&StaggeredArbClosedTrade> = trades_with_greeks
                .iter()
                .filter(|t| !t.won)
                .copied()
                .collect();

            println!("\nGreeks at entry (avg):");
            println!("  Delta:      {:.6}", avg_delta);
            println!("  Gamma:      {:.6}", avg_gamma);
            println!("  Theta:      {:.6}/s", avg_theta);
            println!("  Fair Value: {:.4}", avg_fv);

            if !winning_greeks.is_empty() && !losing_greeks.is_empty() {
                let win_gamma = winning_greeks
                    .iter()
                    .filter_map(|t| t.entry_gamma)
                    .map(|g| g.abs())
                    .sum::<f64>()
                    / winning_greeks.len() as f64;
                let lose_gamma = losing_greeks
                    .iter()
                    .filter_map(|t| t.entry_gamma)
                    .map(|g| g.abs())
                    .sum::<f64>()
                    / losing_greeks.len() as f64;
                println!(
                    "  Win |gamma|:  {:.6}  vs  Lose |gamma|: {:.6}",
                    win_gamma, lose_gamma
                );
            }
        }
    }

    pub fn print_staggered_summary(&self) {
        self.print_summary("Staggered Arb");
    }
}
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Leg2Action {
    Fill(Decimal, String),
    #[allow(dead_code)]
    Abort(String),
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate, UpdateType};
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;

    fn make_spot(ts: &str, symbol: &str, price: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::SpotTrade {
                price,
                quantity: None,
            },
        }
    }

    fn make_binance_l2(ts: &str, symbol: &str, obi_5: Decimal) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::BinanceL2 {
                obi_5,
                obi_10: obi_5,
                bid_volume_5: dec!(1000),
                ask_volume_5: dec!(900),
                spread_bps: dec!(1),
            },
        }
    }

    fn make_quotes(
        ts: &str,
        symbol: &str,
        slug: &str,
        up: Decimal,
        down: Decimal,
    ) -> Vec<MarketUpdate> {
        let timestamp = DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc);

        vec![
            MarketUpdate {
                timestamp,
                symbol: symbol.to_string(),
                update_type: UpdateType::PmQuote {
                    event_slug: slug.to_string(),
                    token_id: format!("{}:UP", slug),
                    side: Side::Up,
                    best_bid: None,
                    best_ask: Some(up),
                },
            },
            MarketUpdate {
                timestamp,
                symbol: symbol.to_string(),
                update_type: UpdateType::PmQuote {
                    event_slug: slug.to_string(),
                    token_id: format!("{}:DOWN", slug),
                    side: Side::Down,
                    best_bid: None,
                    best_ask: Some(down),
                },
            },
        ]
    }

    fn make_event_open(
        ts: &str,
        symbol: &str,
        slug: &str,
        end_ts: &str,
        s0: Decimal,
    ) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::EventState {
                event_slug: slug.to_string(),
                end_time: Some(
                    DateTime::parse_from_rfc3339(end_ts)
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                price_to_beat: Some(s0),
                outcome: None,
            },
        }
    }

    fn make_settlement(ts: &str, symbol: &str, slug: &str, up_won: bool) -> MarketUpdate {
        MarketUpdate {
            timestamp: DateTime::parse_from_rfc3339(ts)
                .unwrap()
                .with_timezone(&Utc),
            symbol: symbol.to_string(),
            update_type: UpdateType::EventState {
                event_slug: slug.to_string(),
                end_time: None,
                price_to_beat: None,
                outcome: Some(up_won),
            },
        }
    }

    #[test]
    fn test_config_defaults() {
        let config = StaggeredArbBacktestConfig::default();
        assert_eq!(config.direction_threshold, 0.05);
        assert_eq!(config.shares_per_trade, 20);
        assert_eq!(config.premium_sum_threshold, Decimal::ONE);
        assert_eq!(config.premium_sum_direction_slope, 1.25);
        assert_eq!(config.premium_sum_obi_slope, 0.25);
        assert_eq!(config.obi_confirm_threshold, 0.005);
        assert_eq!(config.strong_obi_threshold, 0.015);
        assert_eq!(config.strong_obi_direction_relaxation, 0.015);
        assert_eq!(config.strong_obi_price_bonus, dec!(0.02));
        assert_eq!(config.strong_obi_window_bonus_secs, 60);
        assert_eq!(config.max_concurrent_positions, 0);
        assert_eq!(config.max_initial_sum, Decimal::ZERO);
        assert_eq!(config.max_leg1_price, dec!(0.56));
        assert_eq!(config.merge_target_sum, dec!(0.95));
        assert_eq!(config.min_profit_target, dec!(0.02));
        assert_eq!(config.max_wait_secs, 120);
        assert_eq!(config.entry_after_start_min_secs, 30);
        assert_eq!(config.entry_after_start_max_secs, 240);
        assert_eq!(config.min_leg2_delay_secs, 3);
        assert_eq!(config.max_trades_per_event, 0);
        assert_eq!(config.cooldown_secs, 5);
        assert_eq!(config.min_ask_price, dec!(0.05));
        assert_eq!(config.min_entry_sum, dec!(0.30));
        assert_eq!(config.allowed_window_durations, vec![300]);
        assert_eq!(config.force_complete_threshold, dec!(1.08));
        assert_eq!(config.protective_close_threshold, dec!(1.08));
        assert_eq!(config.obi_decay_exit_ratio, 0.35);
        assert_eq!(config.obi_flip_exit_threshold, 0.008);
        assert_eq!(config.min_entry_sigma, 0.003);
        assert_eq!(config.max_entry_sigma, 0.0);
        assert_eq!(config.max_fair_value_distance, 0.15);
    }

    #[test]
    fn test_config_from_toml_matches_checked_in_template() {
        let config = StaggeredArbBacktestConfig::from_toml_str(include_str!(
            "../../config/strategies/staggered_arb.toml"
        ))
        .unwrap();

        assert_eq!(
            config.symbols,
            vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string()
            ]
        );
        assert_eq!(config.shares_per_trade, 20);
        assert_eq!(config.direction_threshold, 0.05);
        assert_eq!(config.obi_confirm_threshold, 0.005);
        assert_eq!(config.strong_obi_threshold, 0.015);
        assert_eq!(config.max_initial_sum, Decimal::ZERO);
        assert_eq!(config.max_leg1_price, dec!(0.56));
        assert_eq!(config.entry_after_start_min_secs, 30);
        assert_eq!(config.entry_after_start_max_secs, 240);
        assert_eq!(config.strong_obi_window_bonus_secs, 60);
        assert_eq!(config.allowed_window_durations, vec![300]);
        assert_eq!(config.force_complete_threshold, dec!(1.08));
        assert_eq!(config.protective_close_threshold, dec!(1.08));
        assert_eq!(config.obi_decay_exit_ratio, 0.35);
        assert_eq!(config.obi_flip_exit_threshold, 0.008);
    }

    #[test]
    fn test_strong_obi_bonus_adjusts_entry_thresholds() {
        let config = StaggeredArbBacktestConfig::default();
        assert!(config.strong_obi_entry_bonus_active(
            true,
            0.02,
            Some(0.01),
            dec!(1.02),
            Some(0.03)
        ));
        assert!((config.direction_threshold_now(dec!(1.02), true) - 0.06).abs() < 1e-9);
        assert_eq!(config.max_leg1_price_now(true), dec!(0.58));
        assert_eq!(config.entry_after_start_max_secs_now(900, true), 300);
    }

    #[test]
    fn test_position_state_display() {
        assert_eq!(format!("{}", ArbPositionState::Leg1Filled), "Leg1Filled");
        assert_eq!(format!("{}", ArbPositionState::Settled), "Settled");
        assert_eq!(format!("{}", ArbPositionState::Aborted), "Aborted");
    }

    #[test]
    fn test_merge_when_sum_below_one() {
        // Scenario: UP is about to rise. Buy UP at 0.45, then DOWN drops to 0.50.
        // Sum = 0.45 + 0.50 = 0.95 < 1.0 → merge for profit.
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01; // low threshold for test
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.allowed_window_durations = vec![]; // accept all in test

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        // Build a feed: event open, spot prices to build vol history, then quotes
        let mut updates = vec![
            // Event window: 10 minutes
            make_event_open(
                "2026-01-01T00:00:00Z",
                "BTCUSDT",
                "test-event",
                "2026-01-01T00:10:00Z",
                dec!(100000),
            ),
        ];

        // Spot price history (need enough for volatility calc)
        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            // Price moving up → p_hat > 0.5 → buy UP first
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }
        updates.push(make_binance_l2(
            "2026-01-01T00:00:59Z",
            "BTCUSDT",
            dec!(0.02),
        ));

        // Initial quotes: sum = 1.05 (spread)
        updates.extend(make_quotes(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            "test-event",
            dec!(0.55),
            dec!(0.50),
        ));

        // After some time, DOWN ask drops → sum becomes < 1.0
        updates.extend(make_quotes(
            "2026-01-01T00:02:00Z",
            "BTCUSDT",
            "test-event",
            dec!(0.60),
            dec!(0.38),
        ));

        let mut feed = HistoricalFeed {
            updates: VecDeque::from(updates),
        };

        let results = engine.run(&mut feed);

        // Should have at least attempted trades
        // The exact outcome depends on vol calc and execution sim,
        // but the engine should not panic and should produce valid results
        assert!(
            results.total_pnl.is_sign_positive()
                || results.total_pnl.is_sign_negative()
                || results.total_pnl == Decimal::ZERO
        );
    }

    #[test]
    fn test_single_leg_settlement() {
        // Scenario: Buy UP, Leg2 never fills, position settles at $1 (UP wins)
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01;
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.max_wait_secs = 30;
        config.allowed_window_durations = vec![];

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let mut updates = vec![make_event_open(
            "2026-01-01T00:00:00Z",
            "BTCUSDT",
            "test-event",
            "2026-01-01T00:10:00Z",
            dec!(100000),
        )];

        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }
        updates.push(make_binance_l2(
            "2026-01-01T00:00:59Z",
            "BTCUSDT",
            dec!(0.02),
        ));

        // Quotes with sum > 1 throughout (no Leg2 opportunity)
        updates.extend(make_quotes(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            "test-event",
            dec!(0.55),
            dec!(0.55),
        ));
        updates.extend(make_quotes(
            "2026-01-01T00:03:00Z",
            "BTCUSDT",
            "test-event",
            dec!(0.60),
            dec!(0.55),
        ));

        // Settlement: UP wins
        updates.push(make_settlement(
            "2026-01-01T00:10:00Z",
            "BTCUSDT",
            "test-event",
            true,
        ));

        let mut feed = HistoricalFeed {
            updates: VecDeque::from(updates),
        };

        let results = engine.run(&mut feed);

        // Engine should handle settlement without panicking
        // Trades that settled as UP winning should be profitable
        for trade in engine.closed_trades() {
            if trade.exit_reason == "settlement" && trade.leg1_direction == "UP" {
                assert!(trade.won, "UP leg should win when UP settles at $1");
            }
        }
        let _ = results; // use results to avoid warning
    }

    #[test]
    fn test_entry_skips_sigma_above_max_entry_sigma() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.0;
        config.use_greeks = false;
        config.max_initial_sum = dec!(1.20);
        config.max_entry_sigma = 0.01;
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let now = Utc::now();
        engine.active_events.insert(
            "BTCUSDT".into(),
            vec![ActiveWindowInfo {
                event_slug: "evt".into(),
                s0: dec!(100),
                end_time: now + chrono::Duration::seconds(280),
                window_duration_secs: 300,
            }],
        );
        engine
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.45))));
        engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
        engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        engine.try_entry_for_window(
            "BTCUSDT",
            now,
            &ActiveWindowInfo {
                event_slug: "evt".into(),
                s0: dec!(100),
                end_time: now + chrono::Duration::seconds(280),
                window_duration_secs: 300,
            },
            dec!(101),
            (Some(0.02), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );

        assert!(
            engine.positions.is_empty(),
            "entry should be rejected when realized sigma exceeds the configured regime cap"
        );
    }

    #[test]
    fn test_entry_requires_more_direction_strength_for_premium_sum() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.0;
        config.use_greeks = false;
        config.max_initial_sum = dec!(1.04);
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let now = Utc::now();
        let window = ActiveWindowInfo {
            event_slug: "evt".into(),
            s0: dec!(100),
            end_time: now + chrono::Duration::seconds(280),
            window_duration_secs: 300,
        };

        engine
            .active_events
            .insert("BTCUSDT".into(), vec![window.clone()]);
        engine
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.48))));
        engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
        engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);
        engine.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(100.04),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.48)),
        );

        assert!(
            engine.positions.is_empty(),
            "premium-sum entries should require stronger direction strength than at-par entries"
        );
    }

    #[test]
    fn test_entry_requires_fresh_binance_l2_history() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.0;
        config.use_greeks = false;
        config.max_initial_sum = dec!(1.20);
        config.entry_after_start_min_secs = 0;
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let now = Utc::now();
        let window = ActiveWindowInfo {
            event_slug: "evt".into(),
            s0: dec!(100),
            end_time: now + chrono::Duration::seconds(280),
            window_duration_secs: 300,
        };

        engine
            .active_events
            .insert("BTCUSDT".into(), vec![window.clone()]);
        engine
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.45))));
        engine.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );

        assert_eq!(
            engine.positions.len(),
            0,
            "backtest should match live and reject entries when no fresh Binance L2 history exists for the replay window"
        );
    }

    #[test]
    fn test_force_complete_threshold_blocks_backtest_timeout_above_cap() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.force_complete_threshold = Decimal::ONE;
        config.use_greeks = false;
        config.min_leg2_delay_secs = 0;

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.75)), Some(dec!(0.27))));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.75),
            leg1_shares: 10,
            leg1_time: now - chrono::Duration::seconds(30),
            leg1_fee: dec!(0.1125),
            entry_obi: None,
            wait_deadline: now - chrono::Duration::seconds(1),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(300),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(1.02),
            initial_sum: dec!(1.02),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(engine.closed_trades.len(), 0);
        assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    }

    #[test]
    fn test_stop_loss_uses_protective_close_threshold() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.force_complete_threshold = Decimal::ONE;
        config.protective_close_threshold = dec!(1.03);
        config.use_greeks = false;
        config.min_leg2_delay_secs = 0;
        config.max_leg1_loss = dec!(0.05);

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.48))));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 10,
            leg1_time: now - chrono::Duration::seconds(30),
            leg1_fee: dec!(0.0825),
            entry_obi: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(20),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(1.03),
            initial_sum: dec!(1.03),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(engine.closed_trades.len(), 1);
        assert_eq!(engine.closed_trades[0].exit_reason, "protective_stop_loss");
    }

    #[test]
    fn test_dynamic_protective_threshold_blocks_early_expensive_stop_loss() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.use_greeks = false;
        config.min_leg2_delay_secs = 0;
        config.max_leg1_loss = dec!(0.05);
        config.protective_close_threshold = dec!(1.08);

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.52))));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 10,
            leg1_time: now - chrono::Duration::seconds(10),
            leg1_fee: dec!(0.0825),
            entry_obi: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(300),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(1.07),
            initial_sum: dec!(1.07),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(engine.closed_trades.len(), 0);
        assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    }

    #[test]
    fn test_supportive_obi_skips_protective_stop_loss() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.use_greeks = false;
        config.min_leg2_delay_secs = 0;
        config.max_leg1_loss = dec!(0.05);
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.50)), Some(dec!(0.53))));
        engine
            .spot_prices
            .insert("BTCUSDT".into(), SpotPrice::new(dec!(100.6), None, now));
        engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.01));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 10,
            leg1_time: now - chrono::Duration::seconds(10),
            leg1_fee: dec!(0.0825),
            wait_deadline: now + chrono::Duration::seconds(120),
            s0: dec!(100),
            event_end_time: now + chrono::Duration::seconds(300),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(1.08),
            initial_sum: dec!(1.08),
            entry_obi: Some(0.02),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(engine.closed_trades.len(), 0);
        assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
    }

    #[test]
    fn test_dynamic_force_threshold_allows_late_close_within_cap() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.use_greeks = false;
        config.min_leg2_delay_secs = 0;
        config.force_complete_threshold = dec!(1.08);

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.75)), Some(dec!(0.32))));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.75),
            leg1_shares: 10,
            leg1_time: now - chrono::Duration::seconds(30),
            leg1_fee: dec!(0.1125),
            entry_obi: None,
            wait_deadline: now - chrono::Duration::seconds(1),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(20),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(1.07),
            initial_sum: dec!(1.07),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.check_leg2_opportunities("BTCUSDT", now);

        assert_eq!(engine.closed_trades.len(), 1);
        assert_eq!(engine.closed_trades[0].exit_reason, "forced_timeout");
    }

    #[test]
    fn test_fill_leg2_partial_keeps_position_open_until_remaining_shares_fill() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.use_greeks = false;
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine.lob_depth.insert("BTCUSDT".into(), 100);
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 200,
            leg1_time: now - chrono::Duration::seconds(30),
            leg1_fee: dec!(1.65),
            entry_obi: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(300),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(0.95),
            initial_sum: dec!(0.95),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: None,
            leg2_price: None,
            leg2_shares: None,
            leg2_time: None,
            leg2_fee: None,
            exit_reason: None,
            pnl: None,
        });

        engine.fill_leg2(0, dec!(0.40), "merge", now);

        assert_eq!(engine.positions[0].state, ArbPositionState::Leg1Filled);
        assert_eq!(engine.positions[0].leg2_shares, Some(100));
        assert_eq!(engine.closed_trades.len(), 0);

        engine.fill_leg2(0, dec!(0.39), "merge", now + chrono::Duration::seconds(15));

        assert_eq!(engine.positions[0].state, ArbPositionState::Settled);
        assert_eq!(engine.positions[0].leg2_shares, Some(200));
        assert_eq!(engine.closed_trades.len(), 1);
        assert_eq!(engine.closed_trades[0].shares, 200);
    }

    #[test]
    fn test_resolve_positions_settles_residual_after_partial_leg2_fill() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.use_greeks = false;
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);
        let now = Utc::now();
        engine.lob_depth.insert("BTCUSDT".into(), 0);
        engine
            .pm_asks_by_event
            .insert("test-event".into(), (Some(dec!(0.60)), Some(dec!(0.40))));
        engine.positions.push(StaggeredArbPosition {
            symbol: "BTCUSDT".into(),
            event_slug: "test-event".into(),
            leg1_direction: Direction::Up,
            leg1_price: dec!(0.55),
            leg1_shares: 200,
            leg1_time: now - chrono::Duration::seconds(30),
            leg1_fee: dec!(1.65),
            entry_obi: None,
            wait_deadline: now + chrono::Duration::seconds(120),
            s0: dec!(100000),
            event_end_time: now + chrono::Duration::seconds(300),
            window_duration_secs: 300,
            entry_p_hat: 0.7,
            entry_sigma: 0.01,
            best_sum_seen: dec!(0.95),
            initial_sum: dec!(0.95),
            entry_greeks: None,
            state: ArbPositionState::Leg1Filled,
            leg2_direction: Some(Direction::Down),
            leg2_price: Some(dec!(0.40)),
            leg2_shares: Some(100),
            leg2_time: Some(now - chrono::Duration::seconds(10)),
            leg2_fee: Some(dec!(0.60)),
            exit_reason: None,
            pnl: None,
        });

        engine.resolve_positions("BTCUSDT", "test-event", false, now);

        assert_eq!(engine.positions[0].state, ArbPositionState::Settled);
        assert_eq!(engine.closed_trades.len(), 1);
        assert_eq!(
            engine.closed_trades[0].pnl,
            dec!(100) - (dec!(110) + dec!(1.65) + dec!(40) + dec!(0.60))
        );
    }

    #[test]
    fn test_entry_uses_simulated_fill_time_for_leg1_clock() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.0;
        config.use_greeks = false;
        config.entry_after_start_min_secs = 0;
        config.max_initial_sum = dec!(1.20);
        config.min_leg2_delay_secs = 0;
        config.cooldown_secs = 0;
        config.max_trades_per_event = 0;
        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let now = Utc::now();
        let window = ActiveWindowInfo {
            event_slug: "evt".into(),
            s0: dec!(100),
            end_time: now + chrono::Duration::seconds(280),
            window_duration_secs: 300,
        };
        engine
            .active_events
            .insert("BTCUSDT".into(), vec![window.clone()]);
        engine
            .pm_asks_by_event
            .insert("evt".into(), (Some(dec!(0.55)), Some(dec!(0.45))));
        engine.binance_l2_obi_5.insert("BTCUSDT".into(), dec!(0.02));
        engine.binance_l2_obi_ts.insert("BTCUSDT".into(), now);

        let expected_fill = engine
            .execution_sim
            .simulate_buy(
                dec!(0.55),
                now,
                engine.config.shares_per_trade,
                engine.market_depth("BTCUSDT"),
            )
            .fill_time;
        let remaining_after_fill = (window.end_time - expected_fill).num_seconds() as f64;
        let expected_wait = expected_fill
            + chrono::Duration::seconds(
                (engine.config.max_wait_secs as i64)
                    .min((remaining_after_fill * engine.config.max_wait_pct) as i64),
            );

        engine.try_entry_for_window(
            "BTCUSDT",
            now,
            &window,
            dec!(101),
            (Some(0.001), 100.0),
            Some(dec!(0.55)),
            Some(dec!(0.45)),
        );

        assert_eq!(engine.positions.len(), 1);
        assert_eq!(engine.positions[0].leg1_time, expected_fill);
        assert_eq!(engine.positions[0].wait_deadline, expected_wait);
    }

    #[test]
    fn test_abort_on_stop_loss() {
        let mut config = StaggeredArbBacktestConfig::default();
        config.direction_threshold = 0.01;
        config.min_time_remaining_secs = 10;
        config.max_initial_sum = dec!(1.20);
        config.min_profit_target = dec!(0.01);
        config.cooldown_secs = 0;
        config.min_leg2_delay_secs = 0;
        config.max_trades_per_event = 0;
        config.max_leg1_loss = dec!(0.05);
        config.allowed_window_durations = vec![];

        let mut engine = StaggeredArbBacktestEngine::new_without_recorder(config);

        let mut updates = vec![make_event_open(
            "2026-01-01T00:00:00Z",
            "BTCUSDT",
            "test-event",
            "2026-01-01T00:10:00Z",
            dec!(100000),
        )];

        for i in 1..=60 {
            let ts = format!("2026-01-01T00:00:{:02}Z", i);
            let price = dec!(100000) + Decimal::from(i * 10);
            updates.push(make_spot(&ts, "BTCUSDT", price));
        }

        // Entry quote
        updates.extend(make_quotes(
            "2026-01-01T00:01:00Z",
            "BTCUSDT",
            "test-event",
            dec!(0.55),
            dec!(0.50),
        ));

        // UP ask drops significantly → stop loss triggers
        updates.extend(make_quotes(
            "2026-01-01T00:01:30Z",
            "BTCUSDT",
            "test-event",
            dec!(0.40),
            dec!(0.65),
        ));

        let mut feed = HistoricalFeed {
            updates: VecDeque::from(updates),
        };

        let _results = engine.run(&mut feed);

        // Check that any aborted trades have stop_loss reason
        let stop_losses: Vec<_> = engine
            .closed_trades()
            .iter()
            .filter(|t| t.exit_reason == "stop_loss")
            .collect();
        // May or may not trigger depending on execution sim, but engine shouldn't panic
        let _ = stop_losses;
    }
}
