use std::collections::HashMap;
use std::fmt;

use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use tracing::info;

use super::{LiquidityVacuumBacktestEngine, LiquidityVacuumClosedTrade};
use crate::strategy::backtest::BacktestResults;

impl LiquidityVacuumBacktestEngine {
    pub(super) fn build_results(&self) -> BacktestResults {
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total.saturating_sub(winning);
        let total_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();
        let win_rate = if total > 0 {
            winning as f64 / total as f64
        } else {
            0.0
        };
        let avg_pnl = if total > 0 {
            total_pnl / Decimal::from(total)
        } else {
            Decimal::ZERO
        };

        let wins: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.pnl)
            .collect();
        let losses: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.pnl)
            .collect();

        let avg_win = if wins.is_empty() {
            Decimal::ZERO
        } else {
            wins.iter().copied().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().copied().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };
        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().copied().sum();
        let total_losses_abs: Decimal = losses.iter().map(|value| value.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding_time_secs = if total > 0 {
            self.closed_trades
                .iter()
                .map(|trade| trade.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|trade| Decimal::from(trade.shares) * trade.entry_price)
            .sum();

        BacktestResults {
            start_time: self.data_range_start.unwrap_or_else(Utc::now),
            end_time: self.data_range_end.unwrap_or_else(Utc::now),
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: self.calculate_sharpe(),
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: self.equity_curve.clone(),
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        if self.closed_trades.len() < 2 {
            return 0.0;
        }

        let pnls: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|trade| trade.pnl.to_f64().unwrap_or(0.0))
            .collect();
        let n = pnls.len() as f64;
        let mean = pnls.iter().sum::<f64>() / n;
        let variance = pnls.iter().map(|pnl| (pnl - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std = variance.sqrt();
        if std < 1e-10 {
            return 0.0;
        }

        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std) * trades_per_year.sqrt()
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
            .filter(|trade| {
                trade.exit_reason == "stop_loss" || trade.exit_reason == "stop_loss_zscore"
            })
            .count();
        let tp_count = self
            .closed_trades
            .iter()
            .filter(|trade| {
                trade.exit_reason == "take_profit" || trade.exit_reason == "take_profit_zscore"
            })
            .count();
        let max_hold_count = self
            .closed_trades
            .iter()
            .filter(|trade| trade.exit_reason == "max_hold")
            .count();
        let settlement_count = self
            .closed_trades
            .iter()
            .filter(|trade| trade.exit_reason == "settlement")
            .count();

        let avg_vote = self
            .closed_trades
            .iter()
            .map(|trade| trade.entry_crowd_vote.to_f64().unwrap_or(0.0).abs())
            .sum::<f64>()
            / total as f64;
        let avg_dev = self
            .closed_trades
            .iter()
            .map(|trade| trade.entry_deviation.to_f64().unwrap_or(0.0))
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

impl fmt::Display for LiquidityVacuumClosedTrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} @ {} -> {} pnl={} reason={}",
            self.entry_time,
            self.symbol,
            self.direction,
            self.entry_price,
            self.exit_price,
            self.pnl,
            self.exit_reason
        )
    }
}
