use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use tracing::info;

use super::{LiquidityVacuumBacktestEngine, LiquidityVacuumClosedTrade};
use crate::strategy::backtest_recorder::{BacktestSignal, PendingTrade, SignalType};
use crate::strategy::momentum::Direction;

impl LiquidityVacuumBacktestEngine {
    pub(super) fn check_exits(&mut self, symbol: &str, ts: DateTime<Utc>) {
        let (depth, deviation_zscore) = match self.symbol_state.get(symbol) {
            Some(state) => (
                state.latest_lob_depth.unwrap_or(10_000),
                state.latest_deviation_zscore(),
            ),
            None => return,
        };

        let mut to_close: Vec<(usize, Decimal, &'static str)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol != symbol || pos.entry_price <= Decimal::ZERO {
                continue;
            }

            let mark = pos.latest_pm_price;
            let held_secs = (ts - pos.entry_time).num_seconds();
            let min_hold_secs = dynamic_min_hold_secs(pos.entry_time, pos.event_end_time);
            let can_take_profit_exit = held_secs >= min_hold_secs;
            let pnl_pct = (mark - pos.entry_price) / pos.entry_price;

            if pnl_pct <= -self.config.stop_loss_pct {
                to_close.push((i, mark, "stop_loss"));
                continue;
            }

            if self.config.stop_loss_zscore_threshold > Decimal::ZERO
                && deviation_zscore.is_some_and(|z| z >= self.config.stop_loss_zscore_threshold)
            {
                to_close.push((i, mark, "stop_loss_zscore"));
                continue;
            }

            if can_take_profit_exit {
                if self.config.take_profit_zscore_threshold > Decimal::ZERO
                    && deviation_zscore
                        .is_some_and(|z| z <= self.config.take_profit_zscore_threshold)
                {
                    to_close.push((i, mark, "take_profit_zscore"));
                    continue;
                }

                if self.config.take_profit_ema_band_pct > Decimal::ZERO
                    && pnl_pct >= self.config.take_profit_ema_band_pct
                {
                    to_close.push((i, mark, "take_profit_pnl_target"));
                    continue;
                }
            }

            if self.config.max_holding_secs > 0 && held_secs >= self.config.max_holding_secs as i64
            {
                to_close.push((i, mark, "max_hold"));
                continue;
            }

            let remaining = (pos.event_end_time - ts).num_seconds();
            if remaining <= self.config.force_exit_before_resolution_secs as i64 {
                to_close.push((i, mark, "force_exit"));
                continue;
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, reason) in to_close {
            self.close_position(idx, exit_price, reason, ts, depth);
        }
    }

    pub(super) fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        let mut to_close = Vec::new();
        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol == symbol && pos.event_slug == event_slug {
                let exit_price = match (&pos.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((i, exit_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, px) in to_close {
            self.close_position(idx, px, "settlement", ts, 10_000);
        }
    }

    pub(super) fn close_position(
        &mut self,
        idx: usize,
        exit_price: Decimal,
        reason: &str,
        ts: DateTime<Utc>,
        liquidity: u64,
    ) {
        let pos = self.positions.remove(idx);

        let (final_price, proceeds) = if reason == "settlement" {
            (exit_price, exit_price * Decimal::from(pos.shares))
        } else {
            let sim = self
                .execution_sim
                .simulate_sell(exit_price, ts, pos.shares, liquidity);
            let gross = Decimal::from(sim.filled_shares) * sim.fill_price;
            let sell_fee = self
                .fee_model
                .fee_shares(Decimal::from(sim.filled_shares), sim.fill_price)
                * sim.fill_price;
            (sim.fill_price, gross - sell_fee)
        };

        self.equity += proceeds;

        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(pos.shares), pos.entry_price)
            * pos.entry_price;
        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price - entry_fee;
        let holding_secs = (ts - pos.entry_time).num_seconds();
        let won = pnl > Decimal::ZERO;

        self.closed_trades.push(LiquidityVacuumClosedTrade {
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares,
            pnl,
            won,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_crowd_vote: pos.entry_crowd_vote,
            entry_deviation: pos.entry_deviation,
            s0: pos.s0,
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            timestamp: ts,
            p_hat: Some(
                ((pos.entry_crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            ev_net: Some(pos.entry_deviation.to_f64().unwrap_or(0.0)),
            sigma: Some(pos.entry_crowd_vote.to_f64().unwrap_or(0.0)),
            market_price: Some(final_price),
            spot_price: None,
            s0: Some(pos.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(final_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol: pos.symbol,
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares as i32,
            pnl,
            won,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(
                ((pos.entry_crowd_vote + Decimal::ONE) / dec!(2))
                    .to_f64()
                    .unwrap_or(0.5),
            ),
            entry_ev_net: Some(pos.entry_deviation.to_f64().unwrap_or(0.0)),
            entry_sigma: Some(pos.entry_crowd_vote.to_f64().unwrap_or(0.0)),
            s0: Some(pos.s0),
        });
    }

    pub(super) fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or_else(Utc::now);
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let price = self.positions[idx].latest_pm_price;
            self.close_position(idx, price, "data_exhausted", ts, 10_000);
        }
    }

    pub fn print_liquidity_vacuum_summary(&self) {
        if self.closed_trades.is_empty() {
            info!("No liquidity-vacuum trades to summarize.");
            return;
        }

        let total = self.closed_trades.len();
        let sl_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "stop_loss" || t.exit_reason == "stop_loss_zscore")
            .count();
        let tp_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "take_profit" || t.exit_reason == "take_profit_zscore")
            .count();
        let max_hold_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "max_hold")
            .count();
        let settlement_count = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .count();

        let avg_vote = self
            .closed_trades
            .iter()
            .map(|t| t.entry_crowd_vote.to_f64().unwrap_or(0.0).abs())
            .sum::<f64>()
            / total as f64;
        let avg_dev = self
            .closed_trades
            .iter()
            .map(|t| t.entry_deviation.to_f64().unwrap_or(0.0))
            .sum::<f64>()
            / total as f64;

        info!(
            "Liquidity-vacuum summary: trades={} tp={} sl={} max_hold={} settlement={} avg|vote|={:.3} avg_dev={:.2}%",
            total,
            tp_count,
            sl_count,
            max_hold_count,
            settlement_count,
            avg_vote,
            avg_dev * 100.0
        );
    }
}

fn dynamic_min_hold_secs(entry_time: DateTime<Utc>, event_end_time: DateTime<Utc>) -> i64 {
    let ttl_secs = (event_end_time - entry_time).num_seconds().max(0);
    let mut min_hold = ttl_secs / 20;
    if min_hold < 5 {
        min_hold = 5;
    } else if min_hold > 30 {
        min_hold = 30;
    }
    min_hold
}
