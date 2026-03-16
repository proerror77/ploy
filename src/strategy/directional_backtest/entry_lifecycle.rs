use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use tracing::{debug, trace};

use crate::strategy::backtest_recorder::{BacktestSignal, SignalType};
use crate::strategy::momentum::Direction;

use super::{ActiveWindowInfo, DirectionalBacktestEngine};

impl DirectionalBacktestEngine {
    // ─── Entry logic (momentum + fair value + edge) ─────────

    pub(super) fn try_directional_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
        // 1. Need: active events with S0, spot price history, PM asks
        let windows: Vec<ActiveWindowInfo> = match self.active_events.get(symbol) {
            Some(w) if !w.is_empty() => w.clone(),
            _ => {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: String::new(),
                        timestamp: ts,
                        p_hat: None,
                        ev_net: None,
                        sigma: None,
                        market_price: None,
                        spot_price: None,
                        s0: None,
                        time_remaining_secs: None,
                        filter_reason: Some("no_active_event".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "no_active_event",
                );
                return;
            }
        };

        // Shared preconditions: spot price with momentum, PM quotes
        let (spot_price, momentum) = match self.spot_prices.get(symbol) {
            Some(s) => {
                // Use weighted momentum (10s/30s/60s) like live engine,
                // fall back to 30s single-timeframe
                let mom = s.weighted_momentum().or_else(|| s.momentum(30));
                (s.price, mom)
            }
            None => {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: String::new(),
                        timestamp: ts,
                        p_hat: None,
                        ev_net: None,
                        sigma: None,
                        market_price: None,
                        spot_price: None,
                        s0: None,
                        time_remaining_secs: None,
                        filter_reason: Some("no_spot_data".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "no_spot_data",
                );
                return;
            }
        };

        // Try entry on each active event window independently
        for window in windows {
            let (up_ask, down_ask) = self
                .pm_asks_by_event
                .get(&window.event_slug)
                .copied()
                .unwrap_or((None, None));
            self.try_entry_for_window(symbol, ts, &window, spot_price, momentum, up_ask, down_ask);
        }
    }

    /// Attempt entry on a specific event window using momentum-based fair value.
    /// Mirrors the live MomentumDetector.check() → estimate_fair_value() → edge logic.
    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        momentum: Option<Decimal>,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        // 2. Time remaining — must be within [min, max] window
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }
        if time_remaining > self.config.max_time_remaining_secs as f64 {
            return;
        }

        // 3. Momentum — need sufficient history
        let momentum = match momentum {
            Some(m) => m,
            None => return, // insufficient price history
        };

        // 4. Minimum momentum threshold
        if momentum.abs() < self.config.min_momentum {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: String::new(),
                    timestamp: ts,
                    p_hat: None,
                    ev_net: None,
                    sigma: None,
                    market_price: None,
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("momentum_below_threshold".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "momentum_below_threshold",
            );
            return;
        }

        // 5. Direction from momentum sign + select PM price
        let (direction, market_ask) = if momentum > Decimal::ZERO {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return,
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return,
            }
        };

        // 6. Fair value estimation (sigmoid mapping, same as live engine)
        let mut fair_value = Self::estimate_fair_value(momentum);

        // 6b. Adjust for price_to_beat if enabled
        if self.config.use_price_to_beat {
            let time_remaining_secs = time_remaining as i64;
            fair_value = Self::adjust_fair_value_for_price_to_beat(
                fair_value,
                momentum,
                st,
                window.s0,
                time_remaining_secs,
                window.end_time,
            );
        }

        // 7. Price bounds check
        if market_ask > self.config.max_entry_price || market_ask < self.config.min_entry_price {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(fair_value.to_f64().unwrap_or(0.5)),
                    ev_net: None,
                    sigma: None,
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("price_bounds".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "price_bounds",
            );
            return;
        }

        // 8. Edge = fair_value - pm_ask - fees
        let best_bid = (market_ask - dec!(0.02)).max(dec!(0.01));
        let depth_ratio = Decimal::from(self.config.shares_per_trade) / dec!(10000);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let fee_per_share_usd = market_ask * cost.taker_fee;
        let spread_plus_slip = cost.spread_cost + cost.depth_slippage;

        let fair_value_f = fair_value.to_f64().unwrap_or(0.5);
        let market_ask_f = market_ask.to_f64().unwrap_or(0.5);
        let total_cost_f =
            fee_per_share_usd.to_f64().unwrap_or(0.01) + spread_plus_slip.to_f64().unwrap_or(0.01);
        let edge = fair_value_f - market_ask_f - total_cost_f;

        if edge < self.config.entry_threshold {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(fair_value_f),
                    ev_net: Some(edge),
                    sigma: None,
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("edge_below_threshold".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "edge_below_threshold",
            );
            return;
        }

        // 9. Cooldown check
        if let Some(last) = self.last_entry_time.get(symbol) {
            let elapsed = (ts - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                self.recorder.record_filtered(
                    &BacktestSignal {
                        signal_type: SignalType::Filtered,
                        symbol: symbol.to_string(),
                        direction: format!("{}", direction),
                        timestamp: ts,
                        p_hat: Some(fair_value_f),
                        ev_net: Some(edge),
                        sigma: None,
                        market_price: Some(market_ask),
                        spot_price: Some(st),
                        s0: Some(window.s0),
                        time_remaining_secs: Some(time_remaining),
                        filter_reason: Some("cooldown".to_string()),
                        exit_reason: None,
                        exit_price: None,
                    },
                    "cooldown",
                );
                return;
            }
        }

        // 10. Max positions check
        if self.positions.len() >= self.config.max_concurrent_positions {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(fair_value_f),
                    ev_net: Some(edge),
                    sigma: None,
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("max_positions".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "max_positions",
            );
            return;
        }

        // 11. Don't enter if already holding same event+direction
        let already_holding = self.positions.iter().any(|p| {
            p.event_slug == window.event_slug
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&direction)
        });
        if already_holding {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(fair_value_f),
                    ev_net: Some(edge),
                    sigma: None,
                    market_price: Some(market_ask),
                    spot_price: Some(st),
                    s0: Some(window.s0),
                    time_remaining_secs: Some(time_remaining),
                    filter_reason: Some("already_holding".to_string()),
                    exit_reason: None,
                    exit_price: None,
                },
                "already_holding",
            );
            return;
        }

        // 12. Execute entry via ExecutionSimulator
        let sim_result =
            self.execution_sim
                .simulate_buy(market_ask, ts, self.config.shares_per_trade, 10_000);

        let entry_cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        let entry_fee = self.fee_model.fee_shares(
            Decimal::from(sim_result.filled_shares),
            sim_result.fill_price,
        ) * sim_result.fill_price;
        let total_entry_cost = entry_cost + entry_fee;
        if total_entry_cost > self.equity {
            trace!(
                "Skipping entry: insufficient equity ({} < {})",
                self.equity, total_entry_cost
            );
            return;
        }

        self.equity -= total_entry_cost;

        self.positions.push(super::DirectionalPosition {
            symbol: symbol.to_string(),
            direction,
            entry_price: sim_result.fill_price,
            entry_time: ts,
            shares: sim_result.filled_shares,
            event_slug: window.event_slug.clone(),
            s0: window.s0,
            event_end_time: window.end_time,
            entry_p_hat: fair_value_f,
            entry_ev_net: edge,
            entry_sigma: momentum.to_f64().unwrap_or(0.0), // store momentum in sigma field
            latest_pm_price: market_ask,
        });

        self.last_entry_time.insert(symbol.to_string(), ts);

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", direction),
            timestamp: ts,
            p_hat: Some(fair_value_f),
            ev_net: Some(edge),
            sigma: Some(momentum.to_f64().unwrap_or(0.0)),
            market_price: Some(sim_result.fill_price),
            spot_price: Some(st),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "ENTRY {} {} @ {:.4} | fv={:.3} edge={:.3} mom={:.4}%",
            symbol,
            direction,
            sim_result.fill_price,
            fair_value_f,
            edge,
            momentum.to_f64().unwrap_or(0.0) * 100.0
        );
    }

    // ─── Fair value estimation (from live MomentumDetector) ───

    /// Sigmoid-like mapping from momentum to fair value.
    /// Mirrors `MomentumDetector::estimate_fair_value()` in momentum.rs.
    fn estimate_fair_value(momentum: Decimal) -> Decimal {
        let abs_momentum = momentum.abs();
        let momentum_factor = if abs_momentum < dec!(0.001) {
            // Very small moves: linear scaling (0.1% → 5%)
            abs_momentum * dec!(50)
        } else if abs_momentum < dec!(0.005) {
            // Medium moves: moderate scaling (0.5% → ~21%)
            dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40)
        } else {
            // Large moves: diminishing returns (1% → ~36%)
            dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30)
        };
        // Cap at 90%
        (dec!(0.50) + momentum_factor).min(dec!(0.90))
    }

    /// Adjust fair value based on distance to price_to_beat and time remaining.
    /// Mirrors `MomentumDetector::estimate_fair_value_with_price_to_beat()`.
    fn adjust_fair_value_for_price_to_beat(
        base_fv: Decimal,
        momentum: Decimal,
        current_price: Decimal,
        price_to_beat: Decimal,
        time_remaining_secs: i64,
        _end_time: DateTime<Utc>,
    ) -> Decimal {
        if price_to_beat <= Decimal::ZERO {
            return base_fv;
        }

        let distance_pct = (current_price - price_to_beat) / price_to_beat;

        // time_factor: fraction of time elapsed. Near expiry → time_factor → 1.0
        let time_factor = (Decimal::ONE - Decimal::from(time_remaining_secs.max(0)) / dec!(900))
            .max(Decimal::ZERO);

        let direction_matches = (momentum > Decimal::ZERO && distance_pct > Decimal::ZERO)
            || (momentum < Decimal::ZERO && distance_pct < Decimal::ZERO);

        if direction_matches {
            let boost = distance_pct.abs() * time_factor * dec!(0.5);
            (base_fv + boost).min(dec!(0.95))
        } else {
            let reduction = distance_pct.abs() * dec!(0.3);
            (base_fv - reduction).max(dec!(0.35))
        }
    }
}
