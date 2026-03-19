use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use tracing::{debug, trace};

use crate::strategy::backtest_recorder::{BacktestSignal, SignalType};
use crate::strategy::momentum::Direction;
use crate::strategy::probability::estimate_probability;

use super::{ActiveWindowInfo, GarchProbabilityBacktestEngine, OpenPosition};

impl GarchProbabilityBacktestEngine {
    pub(super) fn try_entry(&mut self, symbol: &str, ts: DateTime<Utc>) {
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

        let st = match self.spot_prices.get(symbol) {
            Some(s) => s.price,
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

        let sigma_15m = self
            .vol_models
            .get(symbol)
            .map(|m| m.sigma_15m())
            .unwrap_or(self.config.initial_sigma_15m)
            .max(self.config.vol_floor_15m);
        let sigma_eff_15m = (sigma_15m * sigma_15m
            + self.config.basis_sigma_15m * self.config.basis_sigma_15m)
            .sqrt();

        for window in windows {
            let (up_ask, down_ask) = self
                .pm_asks_by_event
                .get(&window.event_slug)
                .copied()
                .unwrap_or((None, None));
            self.try_entry_for_window(symbol, ts, &window, st, sigma_eff_15m, up_ask, down_ask);
        }
    }

    fn try_entry_for_window(
        &mut self,
        symbol: &str,
        ts: DateTime<Utc>,
        window: &ActiveWindowInfo,
        st: Decimal,
        sigma_15m: f64,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) {
        let time_remaining = (window.end_time - ts).num_seconds() as f64;
        if time_remaining <= 0.0 || time_remaining < self.config.min_time_remaining_secs as f64 {
            return;
        }
        if time_remaining > self.config.max_time_remaining_secs as f64 {
            return;
        }

        let p_up = estimate_probability(window.s0, st, sigma_15m, time_remaining, self.config.mu);

        let mut best: Option<(Direction, Decimal, f64, f64)> = None;

        if let Some(ask) = up_ask {
            if ask >= self.config.min_entry_price && ask <= self.config.max_entry_price {
                let fair = p_up.clamp(0.0, 1.0);
                let edge = self.edge_after_costs(fair, ask);
                best = Some((Direction::Up, ask, fair, edge));
            }
        }

        if let Some(ask) = down_ask {
            if ask >= self.config.min_entry_price && ask <= self.config.max_entry_price {
                let fair = (1.0 - p_up).clamp(0.0, 1.0);
                let edge = self.edge_after_costs(fair, ask);
                if best.as_ref().map(|b| edge > b.3).unwrap_or(true) {
                    best = Some((Direction::Down, ask, fair, edge));
                }
            }
        }

        let Some((direction, market_ask, fair_value, edge)) = best else {
            return;
        };

        if edge < self.config.entry_threshold {
            self.recorder.record_filtered(
                &BacktestSignal {
                    signal_type: SignalType::Filtered,
                    symbol: symbol.to_string(),
                    direction: format!("{}", direction),
                    timestamp: ts,
                    p_hat: Some(fair_value),
                    ev_net: Some(edge),
                    sigma: Some(sigma_15m),
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

        if let Some(last) = self.last_entry_time.get(symbol) {
            if (ts - *last).num_seconds() < self.config.cooldown_secs as i64 {
                return;
            }
        }

        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        let already_holding = self.positions.iter().any(|p| {
            p.event_slug == window.event_slug
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&direction)
        });
        if already_holding {
            return;
        }

        let depth = self.config.market_depth_shares.max(1);
        let sim_result =
            self.execution_sim
                .simulate_buy(market_ask, ts, self.config.shares_per_trade, depth);

        if sim_result.filled_shares == 0 {
            return;
        }

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

        self.positions.push(OpenPosition {
            symbol: symbol.to_string(),
            event_slug: window.event_slug.clone(),
            direction,
            entry_price: sim_result.fill_price,
            entry_time: ts,
            shares: sim_result.filled_shares,
            s0: window.s0,
            event_end_time: window.end_time,
            entry_p_hat: fair_value,
            entry_ev_net: edge,
            entry_sigma_15m: sigma_15m,
            latest_pm_price: market_ask,
        });
        self.last_entry_time.insert(symbol.to_string(), ts);

        self.recorder.record_entry(&BacktestSignal {
            signal_type: SignalType::Entry,
            symbol: symbol.to_string(),
            direction: format!("{}", direction),
            timestamp: ts,
            p_hat: Some(fair_value),
            ev_net: Some(edge),
            sigma: Some(sigma_15m),
            market_price: Some(sim_result.fill_price),
            spot_price: Some(st),
            s0: Some(window.s0),
            time_remaining_secs: Some(time_remaining),
            filter_reason: None,
            exit_reason: None,
            exit_price: None,
        });

        debug!(
            "ENTRY {} {} @ {:.4} | fv={:.3} edge={:.3} sigma15m={:.4} window={}s",
            symbol,
            direction,
            sim_result.fill_price,
            fair_value,
            edge,
            sigma_15m,
            window.window_duration_secs
        );
    }

    fn edge_after_costs(&self, fair_value: f64, market_ask: Decimal) -> f64 {
        let best_bid = (market_ask - self.config.assumed_spread).max(dec!(0.01));
        let depth = self.config.market_depth_shares.max(1);
        let depth_ratio = Decimal::from(self.config.shares_per_trade) / Decimal::from(depth);
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let fee_per_share_usd = market_ask * cost.taker_fee;
        let spread_plus_slip = cost.spread_cost + cost.depth_slippage;

        let ask_f = market_ask.to_f64().unwrap_or(0.5);
        let total_cost_f =
            fee_per_share_usd.to_f64().unwrap_or(0.01) + spread_plus_slip.to_f64().unwrap_or(0.01);
        fair_value - ask_f - total_cost_f
    }
}
