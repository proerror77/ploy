use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::strategy::momentum::Direction;

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
    /// Require PM quotes used for entry/exit to be newer than this many seconds. 0 disables staleness gating.
    pub pm_quote_max_stale_secs: u64,
    /// Require the opposite-side PM ask to remain visible for this many seconds before opening Leg1.
    pub entry_quote_persistence_secs: u64,
    /// In the final N seconds, do not place hedge/exit trades. 0 disables this gate.
    pub no_trade_last_secs: u64,
    /// Maximum fraction of window duration to wait for Leg2
    pub max_wait_pct: f64,
    /// Minimum time remaining in window to enter
    pub min_time_remaining_secs: u64,
    // ── Risk control ──
    /// Maximum unrealized loss on Leg1 before buying Leg2 to cap the trade
    pub max_leg1_loss: Decimal,
    /// After a stop-loss breach, allow a short recovery window before forcing Leg2,
    /// unless the directional signal hard-flips against the position.
    pub protective_recovery_window_secs: u64,
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
            pm_quote_max_stale_secs: 10,
            entry_quote_persistence_secs: 8,
            no_trade_last_secs: 30,
            max_wait_pct: 0.30,
            min_time_remaining_secs: 45,
            max_leg1_loss: dec!(0.03),
            protective_recovery_window_secs: 0,
            force_complete_threshold: dec!(1.06),
            protective_close_threshold: dec!(1.06),
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
        if predicted_up { obi } else { -obi }
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

    pub(crate) fn pm_quote_is_fresh(
        &self,
        last_seen_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(last_seen_at) = last_seen_at else {
            return false;
        };
        if self.pm_quote_max_stale_secs == 0 {
            return true;
        }
        (now - last_seen_at).num_seconds().abs() <= self.pm_quote_max_stale_secs as i64
    }

    pub(crate) fn entry_quote_is_persistent(
        &self,
        first_seen_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(first_seen_at) = first_seen_at else {
            return false;
        };
        if self.entry_quote_persistence_secs == 0 {
            return true;
        }
        (now - first_seen_at).num_seconds() >= self.entry_quote_persistence_secs as i64
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

    pub(crate) fn obi_signal_hard_flipped(
        &self,
        leg1_direction: Direction,
        current_obi: Option<f64>,
    ) -> bool {
        let Some(current) = current_obi else {
            return false;
        };
        let directional_current = match leg1_direction {
            Direction::Up => current,
            Direction::Down => -current,
        };
        directional_current <= -self.obi_flip_exit_threshold
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
            pm_quote_max_stale_secs: timing
                .get("pm_quote_max_stale_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(10) as u64,
            entry_quote_persistence_secs: timing
                .get("entry_quote_persistence_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(8) as u64,
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
            protective_recovery_window_secs: risk
                .get("protective_recovery_window_secs")
                .and_then(|v| v.as_integer())
                .unwrap_or(0) as u64,
            force_complete_threshold: Decimal::try_from(
                risk.get("force_complete_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.06),
            )
            .unwrap_or(dec!(1.06)),
            protective_close_threshold: Decimal::try_from(
                risk.get("protective_close_threshold")
                    .and_then(|v| v.as_float())
                    .unwrap_or(1.06),
            )
            .unwrap_or(dec!(1.06)),
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

    pub(super) fn premium_sum_excess(&self, current_sum: Decimal) -> f64 {
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
