use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use tracing::{debug, trace};

use crate::strategy::gamma_scalping::greeks::binary_greeks;
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;

use super::{ActiveWindowInfo, ArbPositionState, StaggeredArbBacktestEngine, StaggeredArbPosition};
use crate::strategy::backtest_recorder::{BacktestSignal, SignalType};

impl StaggeredArbBacktestEngine {
    pub(super) fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
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
                    window.event_slug, up_ask, down_ask
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

    pub(super) fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        spot_vol_info: (Option<f64>, f64),
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }

        let window_start = window.end_time - Duration::seconds(window.window_duration_secs as i64);
        let elapsed_since_start = (ts - window_start).num_seconds();
        if elapsed_since_start < 0 {
            return;
        }
        if self.config.entry_after_start_min_secs > 0
            && elapsed_since_start < self.config.entry_after_start_min_secs as i64
        {
            return;
        }

        let (ua, da) = match (up_ask, down_ask) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };
        let quote_state = self.event_quote_state(&window.event_slug, up_ask, down_ask, ts);
        if !self
            .config
            .pm_quote_is_fresh(quote_state.up.last_seen_at, ts)
            || !self
                .config
                .pm_quote_is_fresh(quote_state.down.last_seen_at, ts)
        {
            trace!("Skipping: PM quotes stale for {}", window.event_slug);
            return;
        }

        if ua < self.config.min_ask_price || da < self.config.min_ask_price {
            return;
        }

        if ua + da < self.config.min_entry_sum {
            return;
        }

        let current_sum = ua + da;
        if self.config.max_initial_sum > Decimal::ZERO && current_sum > self.config.max_initial_sum
        {
            return;
        }

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
                sigma, self.config.min_entry_sigma
            );
            return;
        }
        if self.config.max_entry_sigma > 0.0 && sigma > self.config.max_entry_sigma {
            trace!(
                "Skipping: sigma {:.6} > max_entry_sigma {:.6}",
                sigma, self.config.max_entry_sigma
            );
            return;
        }

        let p_hat = estimate_probability(window.s0, st, sigma, time_remaining, self.config.mu);

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

        if let Some(ref g) = greeks {
            if self.config.min_gamma > 0.0 && g.gamma.abs() < self.config.min_gamma {
                trace!(
                    "Skipping: gamma {:.6} < min_gamma {:.6}",
                    g.gamma.abs(),
                    self.config.min_gamma
                );
                return;
            }

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
                    g.fair_value, self.config.max_fair_value_distance
                );
                return;
            }
        }

        let predicted_up = if self.config.reverse_signal {
            p_hat < 0.5
        } else {
            p_hat > 0.5
        };

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
        let required_obi_strength = self.config.obi_confirm_threshold
            + premium_sum_excess * self.config.premium_sum_obi_slope;
        if !self
            .config
            .obi_confirms_direction(predicted_up, obi, required_obi_strength)
        {
            trace!(
                "Skipping: OBI {:.4} not aligned with required {:.4}",
                obi, required_obi_strength
            );
            return;
        }
        let obi_persistent =
            self.config
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
                elapsed_since_start, allowed_entry_window_secs, strong_obi_bonus_active
            );
            return;
        }

        let (leg1_dir, leg1_ask) = if predicted_up {
            (Direction::Up, ua)
        } else {
            (Direction::Down, da)
        };
        let other_quote_state = if predicted_up {
            quote_state.down
        } else {
            quote_state.up
        };
        if !self
            .config
            .entry_quote_is_persistent(other_quote_state.first_seen_at, ts)
        {
            trace!(
                "Skipping: opposite ask not persistent yet for {}",
                window.event_slug
            );
            return;
        }

        if leg1_ask > self.config.max_leg1_price_now(strong_obi_bonus_active) {
            return;
        }

        let target_leg2 = self.config.merge_target_sum - leg1_ask;
        if target_leg2 <= Decimal::ZERO {
            return;
        }

        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return;
            }
        }

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

        let already_in = self
            .positions
            .iter()
            .any(|p| p.event_slug == window.event_slug && p.state == ArbPositionState::Leg1Filled);
        if already_in {
            return;
        }

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

        let shares = if self.config.delta_weighted_sizing {
            if let Some(ref g) = greeks {
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
                self.equity, total_cost
            );
            return;
        }

        self.equity -= total_cost;

        let leg1_fill_time = sim_result.fill_time;
        let window_duration = (window.end_time - leg1_fill_time).num_seconds() as f64;
        let max_wait_by_pct = (window_duration * self.config.max_wait_pct) as i64;
        let max_wait = (self.config.max_wait_secs as i64).min(max_wait_by_pct);
        let wait_deadline = leg1_fill_time + Duration::seconds(max_wait.max(0));

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
            protective_stop_armed_at: None,
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
}
