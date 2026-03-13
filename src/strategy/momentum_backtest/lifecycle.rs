use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::strategy::momentum::{Direction, MomentumSignal};

use super::MomentumBacktestEngine;

#[derive(Debug, Clone)]
pub(super) struct BacktestPosition {
    pub(super) symbol: String,
    pub(super) direction: Direction,
    pub(super) entry_price: Decimal,
    pub(super) entry_time: DateTime<Utc>,
    pub(super) shares: u64,
    pub(super) latest_pm_price: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BacktestClosedTrade {
    pub(super) symbol: String,
    pub(super) direction: String,
    pub(super) entry_time: DateTime<Utc>,
    pub(super) exit_time: DateTime<Utc>,
    pub(super) entry_price: Decimal,
    pub(super) exit_price: Decimal,
    pub(super) shares: u64,
    pub(super) pnl: Decimal,
    pub(super) won: bool,
    pub(super) holding_secs: i64,
}

impl MomentumBacktestEngine {
    pub(super) fn try_entry(&mut self, signal: &MomentumSignal, ts: DateTime<Utc>) {
        if let Some(last) = self.last_entry_time.get(&signal.symbol) {
            let elapsed = (ts - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                return;
            }
        }

        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        let already_holding = self.positions.iter().any(|p| {
            p.symbol == signal.symbol
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&signal.direction)
        });
        if already_holding {
            return;
        }

        let sim_result = self.execution_sim.simulate_buy(
            signal.pm_price,
            ts,
            self.config.momentum_config.shares_per_trade,
            10_000,
        );

        let cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        if cost > self.equity {
            debug!(
                "Skipping entry: insufficient equity ({} < {})",
                self.equity, cost
            );
            return;
        }

        self.equity -= cost;

        self.positions.push(BacktestPosition {
            symbol: signal.symbol.clone(),
            direction: signal.direction,
            entry_price: sim_result.fill_price,
            entry_time: ts,
            shares: sim_result.filled_shares,
            latest_pm_price: signal.pm_price,
        });

        self.last_entry_time.insert(signal.symbol.clone(), ts);
    }

    pub(super) fn check_exits(&mut self, ts: DateTime<Utc>) {
        let mut to_close = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            let holding_secs = (ts - pos.entry_time).num_seconds();

            if holding_secs > 900 {
                to_close.push((i, pos.latest_pm_price));
                continue;
            }

            if pos.latest_pm_price > pos.entry_price * dec!(1.30) {
                to_close.push((i, pos.latest_pm_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, ts);
        }
    }

    pub(super) fn resolve_positions(&mut self, symbol: &str, up_won: bool, ts: DateTime<Utc>) {
        let mut to_close = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol == symbol {
                let exit_price = match (&pos.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((i, exit_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, ts);
        }
    }

    fn close_position(&mut self, idx: usize, exit_price: Decimal, ts: DateTime<Utc>) {
        let pos = self.positions.remove(idx);
        let sim_result = self
            .execution_sim
            .simulate_sell(exit_price, ts, pos.shares, 10_000);

        let proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        self.equity += proceeds;

        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price;
        let holding_secs = (ts - pos.entry_time).num_seconds();

        self.closed_trades.push(BacktestClosedTrade {
            symbol: pos.symbol,
            direction: pos.direction.to_string(),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: sim_result.fill_price,
            shares: pos.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
        });
    }
}
