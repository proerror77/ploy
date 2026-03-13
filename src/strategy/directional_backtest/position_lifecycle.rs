use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::strategy::backtest_recorder::{BacktestSignal, PendingTrade, SignalType};
use crate::strategy::momentum::Direction;

use super::{DirectionalBacktestEngine, DirectionalClosedTrade};

impl DirectionalBacktestEngine {
    // ─── Exit logic (directional, NOT arb) ───────────────────

    fn check_exits(&mut self, ts: DateTime<Utc>) {
        let mut to_close: Vec<(usize, Decimal, &str)> = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            let time_remaining = (pos.event_end_time - ts).num_seconds() as f64;

            // A. Hold to settlement — no early exit at all. Binary options settle
            // at $1.00 or $0.00, so unrealized PnL fluctuations are noise.
            // The only meaningful exit is settlement itself.
            // When enabled, skip ALL exit checks (time_stop, hard_stop) and wait
            // for the EventState settlement event to close the position.
            if self.config.hold_to_settlement {
                continue;
            }

            // B. Time stop: <N secs remaining AND position is underwater.
            //    Use unrealized PnL (market price vs entry price), NOT the probability model.
            //    The prob model returns ~0.5 always, which would incorrectly exit winners
            //    (pm_price > 0.5 → ev_now < 0 → false exit signal).
            if time_remaining <= self.config.time_stop_secs as f64 && time_remaining > 0.0 {
                let unrealized_per_share = pos.latest_pm_price - pos.entry_price;
                if unrealized_per_share < Decimal::ZERO {
                    to_close.push((i, pos.latest_pm_price, "time_stop"));
                    continue;
                }
            }

            // C. Hard stop: unrealized loss exceeds max
            let unrealized = (pos.latest_pm_price - pos.entry_price) * Decimal::from(pos.shares);
            if unrealized < Decimal::ZERO && unrealized.abs() > self.config.hard_stop_usd {
                to_close.push((i, pos.latest_pm_price, "hard_stop"));
                continue;
            }
        }

        // Close in reverse order to preserve indices
        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, reason) in to_close {
            self.close_position(idx, exit_price, reason, ts);
        }
    }

    // ─── Settlement ──────────────────────────────────────────

    fn resolve_positions(
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
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, "settlement", ts);
        }
    }

    // ─── Close position ──────────────────────────────────────

    fn close_position(&mut self, idx: usize, exit_price: Decimal, reason: &str, ts: DateTime<Utc>) {
        let pos = self.positions.remove(idx);

        // For settlement ($1 or $0), no need to simulate — it's binary payout.
        // Fee at settlement ($1 or $0): p*(1-p) = 0, so settlement fee = $0.
        // For early exits, simulate sell via ExecutionSimulator + exit fee.
        let (final_price, proceeds, _exit_fee) = if reason == "settlement" {
            let p = exit_price;
            // At $1.00 or $0.00, the parabolic fee curve = 0 (p*(1-p) = 0)
            (p, p * Decimal::from(pos.shares), Decimal::ZERO)
        } else {
            let sim_result = self
                .execution_sim
                .simulate_sell(exit_price, ts, pos.shares, 10_000);
            let raw_proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
            // Taker fee on sell: shares × price × feeRate × (p*(1-p))^exponent
            let sell_fee = self.fee_model.fee_shares(
                Decimal::from(sim_result.filled_shares),
                sim_result.fill_price,
            ) * sim_result.fill_price;
            (sim_result.fill_price, raw_proceeds - sell_fee, sell_fee)
        };

        self.equity += proceeds;

        // Entry fee was already deducted from equity at entry time,
        // so PnL = proceeds - (shares × entry_price) already reflects the entry fee implicitly.
        // But we also need to account for exit fee in the PnL.
        let entry_fee = self
            .fee_model
            .fee_shares(Decimal::from(pos.shares), pos.entry_price)
            * pos.entry_price;
        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price - entry_fee;
        let holding_secs = (ts - pos.entry_time).num_seconds();

        self.closed_trades.push(DirectionalClosedTrade {
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: final_price,
            shares: pos.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: pos.entry_p_hat,
            entry_ev_net: pos.entry_ev_net,
            s0: pos.s0,
            entry_sigma: pos.entry_sigma,
        });

        // Record exit signal and trade
        self.recorder.record_exit(&BacktestSignal {
            signal_type: SignalType::Exit,
            symbol: pos.symbol.clone(),
            direction: format!("{}", pos.direction),
            timestamp: ts,
            p_hat: Some(pos.entry_p_hat),
            ev_net: Some(pos.entry_ev_net),
            sigma: Some(pos.entry_sigma),
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
            won: pnl > Decimal::ZERO,
            holding_secs,
            exit_reason: reason.to_string(),
            entry_p_hat: Some(pos.entry_p_hat),
            entry_ev_net: Some(pos.entry_ev_net),
            entry_sigma: Some(pos.entry_sigma),
            s0: Some(pos.s0),
        });
    }

    /// Force-close remaining positions at latest PM price (data exhausted).
    fn close_remaining_positions(&mut self) {
        let ts = self.data_range_end.unwrap_or(Utc::now());
        let indices: Vec<usize> = (0..self.positions.len()).rev().collect();
        for idx in indices {
            let price = self.positions[idx].latest_pm_price;
            self.close_position(idx, price, "data_exhausted", ts);
        }
    }
}
