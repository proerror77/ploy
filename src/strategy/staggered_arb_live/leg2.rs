//! Live Leg2 close/merge decisions, forced-close handling, and paper close flow.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use tracing::info;

use super::lifecycle::{LiveOrderTrack, PaperPositionState, PaperTrade};
use super::{
    Direction, StaggeredArbAdapter, StrategyAction, StrategyEvent, StrategyEventType,
    crypto_submit_intent, estimate_probability, polymarket_order_meets_minimum,
};
use crate::domain::Side;

impl StaggeredArbAdapter {
    pub(super) fn forced_close_allowed(
        &self,
        current_sum: Decimal,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> bool {
        let threshold = self.config.backtest_config.force_close_threshold_now(
            time_remaining_secs,
            window_duration_secs,
            in_final_window,
        );
        threshold <= Decimal::ZERO || current_sum <= threshold
    }

    pub(super) fn protective_close_allowed(
        &self,
        current_sum: Decimal,
        time_remaining_secs: f64,
        window_duration_secs: u64,
        in_final_window: bool,
    ) -> bool {
        let threshold = self.config.backtest_config.protective_close_threshold_now(
            time_remaining_secs,
            window_duration_secs,
            in_final_window,
        );
        threshold <= Decimal::ZERO || current_sum <= threshold
    }

    pub(super) fn premium_sum_excess(&self, current_sum: Decimal) -> f64 {
        let threshold = self.config.backtest_config.premium_sum_threshold;
        if current_sum <= threshold {
            0.0
        } else {
            (current_sum - threshold).to_f64().unwrap_or(0.0).max(0.0)
        }
    }

    pub(super) fn current_window_greeks(
        &self,
        symbol: &str,
        event_id: &str,
        time_remaining: f64,
    ) -> Option<super::super::gamma_scalping::greeks::BinaryGreeks> {
        let bc = &self.config.backtest_config;
        if !bc.use_greeks || time_remaining <= 0.0 {
            return None;
        }
        let window = self
            .active_windows
            .get(symbol)
            .and_then(|ws| ws.iter().find(|w| w.event_id == event_id))?;
        let s0 = window.open_price?;
        if s0 <= Decimal::ZERO {
            return None;
        }
        let st = self.spot_prices.get(symbol)?.price;
        let sigma = self.current_sigma_for_symbol(symbol, bc);
        super::super::gamma_scalping::greeks::binary_greeks(
            st.to_f64().unwrap_or(0.0),
            s0.to_f64().unwrap_or(0.0),
            sigma,
            time_remaining,
            window.window_secs as f64,
        )
    }

    pub(super) fn check_leg2_opportunities(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();
        let bc = self.config.backtest_config.clone();
        let mut leg2_skip_batch: HashMap<&'static str, u64> = HashMap::new();

        let mut leg2_fills: Vec<(usize, Decimal, String)> = Vec::new();
        let mut protective_arm_updates: Vec<(usize, Option<DateTime<Utc>>)> = Vec::new();
        let mut saw_event_quotes = false;

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.state != PaperPositionState::Leg1Filled {
                continue;
            }

            let pm_asks = match self.pm_asks_by_event.get(&pos.event_id) {
                Some(a) => {
                    saw_event_quotes = true;
                    *a
                }
                None => {
                    *leg2_skip_batch.entry("missing_event_quotes").or_default() += 1;
                    continue;
                }
            };
            let quote_state = self.event_quote_state(&pos.event_id, pm_asks.0, pm_asks.1, ts);

            if self.pending_leg2_positions.contains(&i) {
                *leg2_skip_batch.entry("leg2_order_pending").or_default() += 1;
                continue;
            }

            let (time_remaining, window_secs, window_open) = match self.active_windows.get(symbol) {
                Some(windows) => windows
                    .iter()
                    .find(|w| w.event_id == pos.event_id)
                    .map(|w| {
                        (
                            (w.end_time - ts).num_seconds() as f64,
                            w.window_secs,
                            w.open_price,
                        )
                    })
                    .unwrap_or((f64::MAX, 0, None)),
                None => (f64::MAX, 0, None),
            };
            let current_greeks = self.current_window_greeks(symbol, &pos.event_id, time_remaining);
            let current_obi = self
                .binance_l2_obi_5
                .get(symbol)
                .map(|value| value.to_f64().unwrap_or(0.0));
            let in_final_window = bc.no_trade_last_secs > 0
                && time_remaining <= bc.no_trade_last_secs as f64
                && time_remaining > 0.0;
            let displacement_supportive = window_open
                .filter(|open| *open > Decimal::ZERO)
                .and_then(|open| {
                    self.spot_prices
                        .get(symbol)
                        .map(|sp| ((sp.price - open) / open).to_f64().unwrap_or(0.0))
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
                .unwrap_or(!bc.use_greeks);

            let (other_ask, other_state, leg1_mark, leg1_mark_state) = match pos.leg1_direction {
                Direction::Up => (pm_asks.1, quote_state.down, pm_asks.0, quote_state.up),
                Direction::Down => (pm_asks.0, quote_state.up, pm_asks.1, quote_state.down),
            };
            if !bc.pm_quote_is_fresh(other_state.last_seen_at, ts) {
                *leg2_skip_batch.entry("stale_other_ask").or_default() += 1;
                continue;
            }
            let other_ask = match other_ask {
                Some(a) if a >= bc.min_ask_price => a,
                Some(_) => {
                    *leg2_skip_batch.entry("other_ask_below_min").or_default() += 1;
                    continue;
                }
                None => {
                    *leg2_skip_batch.entry("missing_other_ask").or_default() += 1;
                    continue;
                }
            };

            let current_sum = pos.leg1_price + other_ask;
            let all_in_sum = (pos.leg1_price + other_ask) * (Decimal::ONE + self.config.fee_rate);
            let net_profit_per_share = Decimal::ONE - all_in_sum;
            let secs_since_leg1 = (ts - pos.leg1_time).num_seconds();
            let leg2_ready = secs_since_leg1 >= bc.min_leg2_delay_secs as i64;
            if !leg2_ready {
                *leg2_skip_batch.entry("min_leg2_delay").or_default() += 1;
                continue;
            }

            if !in_final_window && current_sum <= bc.merge_target_sum && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            if let Some(ref g) = current_greeks {
                if !in_final_window && current_sum < Decimal::ONE {
                    let gamma_urgency = g.gamma.abs().min(1.0);
                    let adjusted_target = bc.min_profit_target
                        * Decimal::from_f64(1.0 - gamma_urgency * 0.8).unwrap_or(Decimal::ONE);
                    if current_sum < bc.merge_target_sum + adjusted_target {
                        leg2_fills.push((i, other_ask, "merge".to_string()));
                        continue;
                    }
                }

                if bc.max_theta_cost > 0.0 {
                    let theta_cost_remaining = g.theta.abs() * time_remaining.max(0.0);
                    if theta_cost_remaining > bc.max_theta_cost {
                        if !in_final_window && current_sum <= Decimal::ONE {
                            leg2_fills.push((i, other_ask, "merge".to_string()));
                            continue;
                        }
                        if self.protective_close_allowed(
                            current_sum,
                            time_remaining,
                            window_secs,
                            in_final_window,
                        ) {
                            leg2_fills.push((i, other_ask, "protective_theta".to_string()));
                            continue;
                        }
                        *leg2_skip_batch
                            .entry("protective_threshold_blocked")
                            .or_default() += 1;
                        continue;
                    }
                }
            }

            if !in_final_window && net_profit_per_share >= bc.min_profit_target && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            if !in_final_window && net_profit_per_share > Decimal::ZERO && leg2_ready {
                leg2_fills.push((i, other_ask, "merge".to_string()));
                continue;
            }

            if bc.max_leg1_loss > Decimal::ZERO && leg2_ready {
                let leg1_mark = if bc.pm_quote_is_fresh(leg1_mark_state.last_seen_at, ts) {
                    leg1_mark
                } else {
                    None
                };
                if let Some(mark) = leg1_mark {
                    let leg1_loss = (pos.leg1_price - mark).max(Decimal::ZERO);
                    if leg1_loss >= bc.max_leg1_loss {
                        let obi_supportive = bc.obi_signal_still_supportive(
                            pos.leg1_direction,
                            pos.entry_obi,
                            current_obi,
                        );
                        if obi_supportive && displacement_supportive && greeks_supportive {
                            protective_arm_updates.push((i, None));
                            *leg2_skip_batch
                                .entry("protective_signal_still_supportive")
                                .or_default() += 1;
                            continue;
                        }
                        let hard_signal_broken = bc
                            .obi_signal_hard_flipped(pos.leg1_direction, current_obi)
                            || (!displacement_supportive && !greeks_supportive);
                        let armed_at = pos.protective_stop_armed_at.unwrap_or(ts);
                        let recovery_elapsed = (ts - armed_at).num_seconds();
                        let recovery_expired = bc.protective_recovery_window_secs == 0
                            || recovery_elapsed >= bc.protective_recovery_window_secs as i64;
                        if !hard_signal_broken && !recovery_expired {
                            protective_arm_updates.push((i, Some(armed_at)));
                            *leg2_skip_batch
                                .entry("protective_recovery_window")
                                .or_default() += 1;
                            continue;
                        }
                        protective_arm_updates.push((i, None));
                        if self.protective_close_allowed(
                            current_sum,
                            time_remaining,
                            window_secs,
                            in_final_window,
                        ) {
                            leg2_fills.push((i, other_ask, "protective_stop_loss".to_string()));
                        } else {
                            *leg2_skip_batch
                                .entry("protective_threshold_blocked")
                                .or_default() += 1;
                        }
                        continue;
                    } else if pos.protective_stop_armed_at.is_some() {
                        protective_arm_updates.push((i, None));
                    }
                }
            }

            if ts >= pos.wait_deadline && leg2_ready {
                if self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    leg2_fills.push((i, other_ask, "forced_timeout".to_string()));
                } else {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                }
                continue;
            }

            if time_remaining < bc.min_time_remaining_secs as f64 && leg2_ready {
                if self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    leg2_fills.push((i, other_ask, "forced_time_safety".to_string()));
                } else {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                }
                continue;
            }

            if in_final_window && leg2_ready {
                let window_info = self
                    .active_windows
                    .get(symbol)
                    .and_then(|ws| ws.iter().find(|w| w.event_id == pos.event_id));
                let s0 = window_info.and_then(|w| w.open_price);
                let window_secs = window_info.map(|w| w.window_secs).unwrap_or(300);
                let st = self.spot_prices.get(symbol).map(|s| s.price);
                let sigma = self
                    .spot_prices
                    .get(symbol)
                    .and_then(|s| s.volatility(bc.vol_lookback_secs))
                    .and_then(|v| v.to_f64())
                    .map(|tick_vol| {
                        let n = self
                            .spot_prices
                            .get(symbol)
                            .map(|s| s.history_len().min(5000) as f64)
                            .unwrap_or(100.0);
                        (tick_vol * n.sqrt()).max(bc.vol_floor)
                    })
                    .unwrap_or(bc.vol_floor);

                match (s0, st) {
                    (Some(s0_val), Some(st_val)) if s0_val > Decimal::ZERO => {
                        let p_hat =
                            estimate_probability(s0_val, st_val, sigma, time_remaining, bc.mu);
                        let p_win = match pos.leg1_direction {
                            Direction::Up => p_hat,
                            Direction::Down => 1.0 - p_hat,
                        };
                        let displacement =
                            ((st_val - s0_val) / s0_val).to_f64().unwrap_or(0.0).abs();
                        let near_strike = displacement < 0.001;
                        let vol_time_ratio =
                            sigma / (time_remaining / window_secs as f64).max(0.01);
                        let high_vol_regime = vol_time_ratio > 0.05;
                        info!(
                            "[STAG-ARB] FINAL WINDOW CLOSE {} {} p_win={:.3} disp={:.4} near_strike={} high_vol={} — buying Leg2",
                            symbol,
                            pos.leg1_direction,
                            p_win,
                            displacement,
                            near_strike,
                            high_vol_regime,
                        );
                    }
                    _ => {
                        info!(
                            "[STAG-ARB] FINAL WINDOW CLOSE {} {} without price context — buying Leg2",
                            symbol, pos.leg1_direction,
                        );
                    }
                }
                if !self.forced_close_allowed(
                    current_sum,
                    time_remaining,
                    window_secs,
                    in_final_window,
                ) {
                    *leg2_skip_batch
                        .entry("force_threshold_blocked")
                        .or_default() += 1;
                    continue;
                }
                leg2_fills.push((i, other_ask, "forced_final_window".to_string()));
                continue;
            }
        }

        if !saw_event_quotes {
            self.bump_leg2_skip_for_symbol(symbol, "missing_pm_quotes");
        }

        for (idx, armed_at) in protective_arm_updates {
            if let Some(pos) = self.positions.get_mut(idx) {
                pos.protective_stop_armed_at = armed_at;
            }
        }

        leg2_fills.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, other_ask, reason) in leg2_fills {
            if let Some(action) = self.fill_leg2(idx, other_ask, &reason, ts) {
                actions.push(action);
            }
        }
        for (reason, count) in leg2_skip_batch {
            *self.leg2_skip_counts.entry(reason.to_string()).or_default() += count;
            *self
                .leg2_skip_counts_by_symbol
                .entry(symbol.to_string())
                .or_default()
                .entry(reason.to_string())
                .or_default() += count;
        }
        actions
    }

    pub(super) fn fill_leg2(
        &mut self,
        idx: usize,
        other_ask: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
    ) -> Option<StrategyAction> {
        let pos = &self.positions[idx];
        let symbol = pos.symbol.clone();
        let already_filled = Self::leg2_filled_shares(pos);
        let shares = Self::leg2_remaining_shares(pos);
        if shares == 0 {
            return None;
        }
        if !polymarket_order_meets_minimum(other_ask, shares) {
            self.bump_leg2_skip_for_symbol(&symbol, "leg2_residual_below_venue_minimum");
            return None;
        }

        if self.dry_run {
            let leg2_fee = other_ask * Decimal::from(shares) * self.config.fee_rate;
            let leg2_cost = other_ask * Decimal::from(shares) + leg2_fee;

            if leg2_cost > self.equity {
                return None;
            }
            self.equity -= leg2_cost;

            let payout = Decimal::from(shares) * Decimal::ONE;
            let total_cost = Decimal::from(pos.leg1_shares) * pos.leg1_price
                + pos.leg1_fee
                + other_ask * Decimal::from(shares)
                + leg2_fee;
            let pnl = payout - total_cost;
            self.equity += payout;

            let duration_secs = (ts - pos.leg1_time).num_seconds();
            let symbol = pos.symbol.clone();
            let event_id = pos.event_id.clone();
            let direction = pos.leg1_direction.clone();
            let leg1_price = pos.leg1_price;
            let opened_at = pos.leg1_time;

            let pos = &mut self.positions[idx];
            pos.leg2_price = Some(other_ask);
            pos.leg2_shares = Some(shares);
            pos.leg2_fee = Some(leg2_fee);
            pos.leg2_time = Some(ts);
            pos.state = if reason == "merge" {
                PaperPositionState::Merged
            } else {
                PaperPositionState::ForcedComplete
            };

            self.closed_trades.push(PaperTrade {
                symbol: symbol.clone(),
                event_id,
                direction: direction.clone(),
                leg1_price,
                leg2_price: other_ask,
                total_cost,
                payout,
                pnl,
                exit_reason: reason.to_string(),
                duration_secs,
                opened_at,
                closed_at: ts,
            });

            let tag = if reason == "merge" {
                "COMPLETE"
            } else {
                "FORCED"
            };
            let msg = format!(
                "[STAG-ARB] {} {} cost=${:.4} payout=${:.4} pnl={}{:.4} wait={}s reason={} (paper)",
                tag,
                symbol,
                total_cost,
                payout,
                if pnl >= Decimal::ZERO { "+" } else { "" },
                pnl,
                duration_secs,
                reason,
            );
            info!("{}", msg);

            Some(StrategyAction::LogEvent {
                event: StrategyEvent::new(StrategyEventType::CycleCompleted, msg),
            })
        } else {
            let symbol = pos.symbol.clone();
            let event_id = pos.event_id.clone();
            let up_token = pos.up_token.clone();
            let down_token = pos.down_token.clone();
            let leg2_direction = match pos.leg1_direction {
                Direction::Up => Direction::Down,
                Direction::Down => Direction::Up,
            };

            let token_id = match leg2_direction {
                Direction::Up => up_token.clone(),
                Direction::Down => down_token.clone(),
            };

            let side = match leg2_direction {
                Direction::Up => Side::Up,
                Direction::Down => Side::Down,
            };

            let close_mode = if reason == "merge" { "merge" } else { "forced" };
            let client_order_id = format!(
                "stag_leg2_{}_{}_{}",
                close_mode,
                event_id,
                Utc::now().timestamp_millis()
            );

            self.live_orders.insert(
                client_order_id.clone(),
                LiveOrderTrack {
                    event_id: event_id.clone(),
                    condition_id: pos.condition_id.clone(),
                    symbol: symbol.clone(),
                    up_token,
                    down_token,
                    direction: leg2_direction,
                    token_id: token_id.clone(),
                    leg: 2,
                    price: other_ask,
                    shares,
                    position_idx: Some(idx),
                    close_reason: Some(reason.to_string()),
                    submitted_at: ts,
                    cancel_requested_at: None,
                    exchange_order_id: None,
                    acknowledged_filled_qty: already_filled,
                    entry_obi: pos.entry_obi,
                },
            );
            self.pending_leg2_positions.insert(idx);

            let tag = if reason == "merge" {
                "COMPLETE"
            } else {
                "FORCED"
            };
            let msg = format!(
                "[STAG-ARB] LEG2 {} SUBMIT {} @ {:.2}¢ ({} shares, ${:.2}) reason={} filled={}/{}",
                tag,
                symbol,
                other_ask * dec!(100),
                shares,
                other_ask.to_f64().unwrap_or(0.0) * shares as f64,
                reason,
                already_filled,
                pos.leg1_shares,
            );
            info!("{}", msg);

            Some(crypto_submit_intent(
                client_order_id,
                event_id,
                token_id,
                side,
                shares,
                other_ask,
                10,
            ))
        }
    }
}
