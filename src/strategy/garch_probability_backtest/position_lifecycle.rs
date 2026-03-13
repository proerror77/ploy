use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::strategy::backtest_recorder::{BacktestSignal, PendingTrade, SignalType};
use crate::strategy::momentum::Direction;

use super::{GarchProbabilityBacktestEngine, GarchProbabilityClosedTrade};

impl GarchProbabilityBacktestEngine {
    pub(super) fn resolve_positions(
        &mut self,
        symbol: &str,
        event_slug: &str,
        up_won: bool,
        ts: DateTime<Utc>,
    ) {
        let mut to_close = Vec::new();
        for (index, position) in self.positions.iter().enumerate() {
            if position.symbol == symbol && position.event_slug == event_slug {
                let exit_price = match (&position.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((index, exit_price));
            }
        }
        to_close.sort_by(|lhs, rhs| rhs.0.cmp(&lhs.0));
        for (index, exit_price) in to_close {
            self.close_position(index, exit_price, "settlement", ts);
        }
    }

    fn close_position(&mut self, idx: usize, exit_price: Decimal, reason: &str, ts: DateTime<Utc>) {
        let position = self.positions.remove(idx);

        let depth = self.config.market_depth_shares.max(1);
        let (final_price, proceeds, _exit_fee) = if reason == "settlement" {
            let price = exit_price;
            (price, price * Decimal::from(position.shares), Decimal::ZERO)
        } else {
            let sim_result =
                self.execution_sim
                    .simulate_sell(exit_price, ts, position.shares, depth);
            let raw_proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
            let sell_fee = self.fee_model.fee_shares(
                Decimal::from(sim_result.filled_shares),
                sim_result.fill_price,
            ) * sim_result.fill_price;
            (sim_result.fill_price, raw_proceeds - sell_fee, sell_fee)
        };

        self.equity += proceeds;

        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(position.shares), position.entry_price)
            * position.entry_price;
        let pnl = proceeds - Decimal::from(position.shares) * position.entry_price - entry_fee;
        let holding_secs = (ts - position.entry_time).num_seconds();

        self.closed_trades.push(GarchProbabilityClosedTrade {
            symbol: position.symbol.clone(),
            direction: format!("{}", position.direction),
            entry_time: position.entry_time,
            exit_time: ts,
            entry_price: position.entry_price,
            exit_price: final_price,
            shares: position.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: position.entry_p_hat,
            entry_ev_net: position.entry_ev_net,
            s0: position.s0,
            entry_sigma_15m: position.entry_sigma_15m,
        });

        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: position.symbol.clone(),
            direction: format!("{}", position.direction),
            timestamp: ts,
            p_hat: Some(position.entry_p_hat),
            ev_net: Some(position.entry_ev_net),
            sigma: Some(position.entry_sigma_15m),
            market_price: Some(final_price),
            spot_price: None,
            s0: Some(position.s0),
            time_remaining_secs: None,
            filter_reason: None,
            exit_reason: Some(reason.to_string()),
            exit_price: Some(final_price),
        });

        self.recorder.record_trade(&PendingTrade {
            symbol: position.symbol.clone(),
            direction: format!("{}", position.direction),
            entry_time: position.entry_time,
            exit_time: ts,
            entry_price: position.entry_price,
            exit_price: final_price,
            shares: position.shares as i32,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(position.entry_p_hat),
            entry_ev_net: Some(position.entry_ev_net),
            entry_sigma: Some(position.entry_sigma_15m),
            s0: Some(position.s0),
        });
    }

    pub(super) fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let price = self.positions[idx].latest_pm_price;
            self.close_position(idx, price, "data_exhausted", ts);
        }
    }
}
