use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use tracing::{debug, trace};

use crate::strategy::backtest_recorder::{BacktestSignal, SignalType};
use crate::strategy::momentum::Direction;

use super::{ActiveWindowInfo, LiquidityVacuumBacktestEngine, LiquidityVacuumPosition};

#[derive(Debug, Clone)]
struct CommonSignalState {
    spot_price: Decimal,
    price_move: Decimal,
    volume_ratio: Decimal,
    flow_component: Decimal,
    deviation_abs: Decimal,
    deviation_zscore: Option<Decimal>,
    liquidity_depth: Option<u64>,
}

impl LiquidityVacuumBacktestEngine {
    fn compute_common_signal_state(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
    ) -> Option<CommonSignalState> {
        let state = self.symbol_state.get_mut(symbol)?;
        state.prune_old(
            ts,
            self.config.window_secs,
            self.config.volume_baseline_samples,
        );
        state.reset_daily_counter_if_needed(ts);

        let ema = state.ema.warm_value()?;
        let expected_price = ema * (Decimal::ONE + self.config.sentiment_offset);
        if expected_price <= Decimal::ZERO {
            return None;
        }
        let signed_deviation = (state.spot.price - expected_price) / expected_price;
        let deviation_abs = signed_deviation.abs();
        let deviation_zscore =
            state.record_deviation_sample(signed_deviation, self.config.zscore_lookback_samples);

        let price_move = state.spot.momentum(self.config.window_secs)?.abs();

        let _current_vol = state.maybe_sample_volume_window(
            ts,
            self.config.window_secs,
            self.config.volume_baseline_samples,
        );
        let volume_ratio = state.volume_ratio()?;
        let flow_component = state.flow_component(ts, self.config.window_secs)?;

        Some(CommonSignalState {
            spot_price: state.spot.price,
            price_move,
            volume_ratio,
            flow_component,
            deviation_abs,
            deviation_zscore,
            liquidity_depth: state.latest_lob_depth,
        })
    }

    pub(super) fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let windows = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => return,
        };

        let common = match self.compute_common_signal_state(symbol, ts) {
            Some(s) => s,
            None => return,
        };

        if common.price_move <= self.config.price_move_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "price_move_below_threshold",
            );
            return;
        }

        if common.volume_ratio <= self.config.volume_multiplier_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(common.volume_ratio),
                Some(common.spot_price),
                None,
                "volume_ratio_below_threshold",
            );
            return;
        }

        for window in windows {
            self.try_entry_for_window(symbol, &window, &common, ts);
        }
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        window: &ActiveWindowInfo,
        common: &CommonSignalState,
        ts: DateTime<Utc>,
    ) {
        let quote = match self.quotes_by_event.get(&window.event_slug) {
            Some(q) => q.clone(),
            None => return,
        };

        let (up_ask, down_ask, up_ts, down_ts) =
            match (quote.up_ask, quote.down_ask, quote.up_ts, quote.down_ts) {
                (Some(u), Some(d), Some(ut), Some(dt)) => (u, d, ut, dt),
                _ => return,
            };

        if !super::is_valid_binary_quote_price(up_ask)
            || !super::is_valid_binary_quote_price(down_ask)
        {
            let invalid_px = if !super::is_valid_binary_quote_price(up_ask) {
                up_ask
            } else {
                down_ask
            };
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                Some(invalid_px),
                "invalid_binary_quote_bounds",
            );
            return;
        }

        let up_age_ms = (ts - up_ts).num_milliseconds();
        let down_age_ms = (ts - down_ts).num_milliseconds();
        if up_age_ms > self.config.max_quote_age_ms || down_age_ms > self.config.max_quote_age_ms {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "stale_quote",
            );
            return;
        }

        let time_remaining = (window.end_time - ts).num_seconds();
        if time_remaining <= self.config.force_exit_before_resolution_secs as i64 {
            self.record_filtered(
                symbol,
                "",
                ts,
                None,
                Some(common.spot_price),
                None,
                "too_close_to_resolution",
            );
            return;
        }

        let ask_sum = up_ask + down_ask;
        if ask_sum <= Decimal::ZERO {
            return;
        }

        let spread_proxy_bps = ((ask_sum - Decimal::ONE).abs() * dec!(10000))
            .to_u32()
            .unwrap_or(0);
        if spread_proxy_bps > self.config.max_spread_bps {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(Decimal::from(spread_proxy_bps)),
                Some(common.spot_price),
                None,
                "spread_proxy_too_wide",
            );
            return;
        }

        if let Some(depth) = common.liquidity_depth {
            if depth < self.config.min_liquidity_shares {
                self.record_filtered(
                    symbol,
                    "",
                    ts,
                    Some(Decimal::from(depth)),
                    Some(common.spot_price),
                    None,
                    "insufficient_liquidity",
                );
                return;
            }
        }

        let book_skew = (up_ask - down_ask) / (up_ask + down_ask);
        let crowd_vote =
            self.config.flow_weight * common.flow_component + self.config.book_weight * book_skew;
        if crowd_vote.abs() < self.config.order_concentration_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(crowd_vote),
                Some(common.spot_price),
                None,
                "crowd_vote_below_threshold",
            );
            return;
        }

        let deviation = common.deviation_abs;
        if self.config.entry_zscore_threshold > Decimal::ZERO {
            let zscore = match common.deviation_zscore {
                Some(z) => z,
                None => {
                    self.record_filtered(
                        symbol,
                        "",
                        ts,
                        None,
                        Some(common.spot_price),
                        None,
                        "zscore_unavailable",
                    );
                    return;
                }
            };

            if zscore <= self.config.entry_zscore_threshold {
                self.record_filtered(
                    symbol,
                    "",
                    ts,
                    Some(zscore),
                    Some(common.spot_price),
                    None,
                    "zscore_below_threshold",
                );
                return;
            }
        } else if deviation <= self.config.entry_deviation_threshold {
            self.record_filtered(
                symbol,
                "",
                ts,
                Some(deviation),
                Some(common.spot_price),
                None,
                "deviation_below_threshold",
            );
            return;
        }

        let direction = if crowd_vote > Decimal::ZERO {
            Direction::Down
        } else {
            Direction::Up
        };
        let entry_price = match direction {
            Direction::Up => up_ask,
            Direction::Down => down_ask,
        };
        let fair_up_prob = super::fair_up_probability_from_spot(common.spot_price, window.s0);
        let fair_price = match direction {
            Direction::Up => fair_up_prob,
            Direction::Down => Decimal::ONE - fair_up_prob,
        };
        let expected_edge = fair_price - entry_price;
        let estimated_roundtrip_fee =
            self.fee_model.fee_shares(Decimal::ONE, entry_price) * entry_price * dec!(2);
        let min_required_edge = estimated_roundtrip_fee + self.config.min_edge_buffer;
        if expected_edge <= min_required_edge {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                Some(expected_edge),
                Some(common.spot_price),
                Some(entry_price),
                "edge_below_cost",
            );
            return;
        }

        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                self.record_filtered(
                    symbol,
                    &format!("{direction}"),
                    ts,
                    None,
                    Some(common.spot_price),
                    Some(entry_price),
                    "cooldown",
                );
                return;
            }
        }

        if self.positions.len() >= self.config.max_concurrent_positions {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "max_positions",
            );
            return;
        }

        if self
            .positions
            .iter()
            .any(|p| p.event_slug == window.event_slug && p.direction == direction)
        {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "already_holding",
            );
            return;
        }

        if let Some(state) = self.symbol_state.get_mut(symbol) {
            state.reset_daily_counter_if_needed(ts);
            if self.config.max_daily_trades > 0
                && state.daily_trade_count >= self.config.max_daily_trades
            {
                self.record_filtered(
                    symbol,
                    &format!("{direction}"),
                    ts,
                    None,
                    Some(common.spot_price),
                    Some(entry_price),
                    "max_daily_trades",
                );
                return;
            }
        }

        let depth_for_fill = common.liquidity_depth.unwrap_or(10_000);
        let sim = self.execution_sim.simulate_buy(
            entry_price,
            ts,
            self.config.shares_per_trade,
            depth_for_fill,
        );
        if sim.filled_shares == 0 {
            self.record_filtered(
                symbol,
                &format!("{direction}"),
                ts,
                None,
                Some(common.spot_price),
                Some(entry_price),
                "no_fill",
            );
            return;
        }

        let entry_cost = Decimal::from(sim.filled_shares) * sim.fill_price;
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(sim.filled_shares), sim.fill_price)
            * sim.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
            trace!(
                "Skipping entry: insufficient equity {} < {}",
                self.equity, total_entry_cost
            );
            return;
        }

        self.equity -= total_entry_cost;
        self.last_entry_time.insert(symbol.to_string(), ts);
        if let Some(state) = self.symbol_state.get_mut(symbol) {
            state.daily_trade_count += 1;
        }

        self.positions.push(LiquidityVacuumPosition {
            symbol: symbol.to_string(),
            event_slug: window.event_slug.clone(),
            direction,
            entry_price: sim.fill_price,
            entry_time: ts,
            shares: sim.filled_shares,
            event_end_time: window.end_time,
            latest_pm_price: entry_price,
            entry_crowd_vote: crowd_vote,
            entry_deviation: deviation,
            s0: window.s0,
        });

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{direction}"),
            timestamp: ts,
            p_hat: Some(
                ((crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            ev_net: Some(deviation.to_f64().unwrap_or(0.0)),
            sigma: Some(crowd_vote.to_f64().unwrap_or(0.0)),
            market_price: Some(sim.fill_price),
            spot_price: Some(common.spot_price),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining as f64),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "ENTRY {} {} @ {:.4} vote={:.3} dev={:.2}%",
            symbol,
            direction,
            sim.fill_price,
            crowd_vote,
            deviation * dec!(100)
        );
    }

    fn record_filtered(
        &mut self,
        symbol: &str,
        direction: &str,
        ts: DateTime<Utc>,
        metric: Option<Decimal>,
        spot_price: Option<Decimal>,
        market_price: Option<Decimal>,
        reason: &str,
    ) {
        self.recorder.record_filtered(
            &BacktestSignal {
                signal_type: SignalType::Filtered,
                symbol: symbol.to_string(),
                direction: direction.to_string(),
                timestamp: ts,
                p_hat: None,
                ev_net: metric.and_then(|v| v.to_f64()),
                sigma: None,
                market_price,
                spot_price,
                s0: None,
                time_remaining_secs: None,
                filter_reason: Some(reason.to_string()),
                exit_reason: None,
                exit_price: None,
            },
            reason,
        );
    }
}
