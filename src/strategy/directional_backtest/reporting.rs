use std::collections::HashMap;
use std::fmt;

use chrono::Utc;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::info;

use super::DirectionalBacktestEngine;
use crate::strategy::backtest::BacktestResults;

impl DirectionalBacktestEngine {
    pub(super) fn build_results(&self) -> BacktestResults {
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total - winning;
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
            wins.iter().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };

        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().sum();
        let total_losses_abs: Decimal = losses.iter().map(|loss| loss.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding = if total > 0 {
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
            start_time: self.data_range_start.unwrap_or(Utc::now()),
            end_time: self.data_range_end.unwrap_or(Utc::now()),
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
            avg_holding_time_secs: avg_holding,
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
        let std_dev = variance.sqrt();

        if std_dev < 1e-10 {
            return 0.0;
        }

        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std_dev) * trades_per_year.sqrt()
    }

    pub fn print_directional_summary(&self) {
        if self.closed_trades.is_empty() {
            info!("No trades to summarize.");
            return;
        }

        let total = self.closed_trades.len();
        let settled = self
            .closed_trades
            .iter()
            .filter(|trade| trade.exit_reason == "settlement")
            .count();
        let settlement_rate = settled as f64 / total as f64 * 100.0;

        let mut exit_counts: HashMap<&str, usize> = HashMap::new();
        for trade in &self.closed_trades {
            *exit_counts.entry(&trade.exit_reason).or_default() += 1;
        }

        let winner_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|trade| trade.won)
            .map(|trade| trade.entry_p_hat)
            .collect();
        let loser_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|trade| !trade.won)
            .map(|trade| trade.entry_p_hat)
            .collect();

        let avg_winner_p = if winner_p.is_empty() {
            0.0
        } else {
            winner_p.iter().sum::<f64>() / winner_p.len() as f64
        };
        let avg_loser_p = if loser_p.is_empty() {
            0.0
        } else {
            loser_p.iter().sum::<f64>() / loser_p.len() as f64
        };

        let ev_nets: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|trade| trade.entry_ev_net)
            .collect();
        let avg_ev = ev_nets.iter().sum::<f64>() / total as f64;

        let up_trades = self
            .closed_trades
            .iter()
            .filter(|trade| trade.direction == "UP")
            .count();
        let down_trades = total - up_trades;
        let up_wins = self
            .closed_trades
            .iter()
            .filter(|trade| trade.direction == "UP" && trade.won)
            .count();
        let down_wins = self
            .closed_trades
            .iter()
            .filter(|trade| trade.direction == "DOWN" && trade.won)
            .count();

        println!("\n=== Directional Backtest Summary ===");
        println!(
            "Settlement rate:  {:.1}% ({}/{})",
            settlement_rate, settled, total
        );
        println!("Exit reasons:");
        for (reason, count) in &exit_counts {
            println!("  {:<16} {}", reason, count);
        }
        println!("\nCalibration:");
        println!("  Avg p_hat winners:  {:.3}", avg_winner_p);
        println!("  Avg p_hat losers:   {:.3}", avg_loser_p);
        println!("  Avg EV_net at entry: {:.4}", avg_ev);
        println!("\nDirection breakdown:");
        println!(
            "  UP:   {} trades, {} wins ({:.1}%)",
            up_trades,
            up_wins,
            if up_trades > 0 {
                up_wins as f64 / up_trades as f64 * 100.0
            } else {
                0.0
            }
        );
        println!(
            "  DOWN: {} trades, {} wins ({:.1}%)",
            down_trades,
            down_wins,
            if down_trades > 0 {
                down_wins as f64 / down_trades as f64 * 100.0
            } else {
                0.0
            }
        );

        let sigmas: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|trade| trade.entry_sigma)
            .collect();
        let avg_sigma = sigmas.iter().sum::<f64>() / sigmas.len().max(1) as f64;
        let min_sigma = sigmas.iter().cloned().fold(f64::MAX, f64::min);
        let max_sigma = sigmas.iter().cloned().fold(f64::MIN, f64::max);
        println!("\nVolatility:");
        println!("  Avg σ at entry: {:.5}", avg_sigma);
        println!("  Min σ: {:.5}  Max σ: {:.5}", min_sigma, max_sigma);

        let hold_times: Vec<i64> = self
            .closed_trades
            .iter()
            .map(|trade| trade.holding_secs)
            .collect();
        let avg_hold = hold_times.iter().sum::<i64>() as f64 / hold_times.len().max(1) as f64;
        let min_hold = hold_times.iter().min().copied().unwrap_or(0);
        let max_hold = hold_times.iter().max().copied().unwrap_or(0);
        println!("\nHolding time:");
        println!(
            "  Avg: {:.0}s  Min: {}s  Max: {}s",
            avg_hold, min_hold, max_hold
        );

        let entry_prices: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|trade| trade.entry_price.to_f64().unwrap_or(0.0))
            .collect();
        let avg_entry = entry_prices.iter().sum::<f64>() / entry_prices.len().max(1) as f64;
        println!("  Avg entry price: ${:.4}", avg_entry);

        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal, Decimal)> = HashMap::new();
        for trade in &self.closed_trades {
            let stats =
                symbol_stats
                    .entry(&trade.symbol)
                    .or_insert((0, 0, Decimal::ZERO, Decimal::ZERO));
            stats.0 += 1;
            if trade.won {
                stats.1 += 1;
            }
            stats.2 += trade.pnl;
            stats.3 += Decimal::from(trade.shares) * trade.entry_price;
        }

        let mut symbols: Vec<&&str> = symbol_stats.keys().collect();
        symbols.sort();

        println!("\nPer-symbol breakdown:");
        println!(
            "  {:<12} {:>6} {:>6} {:>8} {:>12} {:>12}",
            "Symbol", "Trades", "Wins", "WinRate", "PnL", "Volume"
        );
        println!("  {}", "-".repeat(62));
        for symbol in &symbols {
            let (trades, wins, pnl, vol) = symbol_stats[*symbol];
            let wr = if trades > 0 {
                wins as f64 / trades as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
                symbol, trades, wins, wr, pnl, vol
            );
        }
        let total_vol: Decimal = symbol_stats.values().map(|value| value.3).sum();
        let total_pnl: Decimal = symbol_stats.values().map(|value| value.2).sum();
        println!("  {}", "-".repeat(62));
        println!(
            "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
            "TOTAL",
            total,
            self.closed_trades.iter().filter(|trade| trade.won).count(),
            self.closed_trades.iter().filter(|trade| trade.won).count() as f64 / total as f64
                * 100.0,
            total_pnl,
            total_vol
        );
    }
}

impl fmt::Display for DirectionalBacktestEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let results = self.build_results();
        writeln!(f, "=== Directional Backtest Results ===")?;
        writeln!(
            f,
            "Period:        {} to {}",
            results.start_time.format("%Y-%m-%d %H:%M"),
            results.end_time.format("%Y-%m-%d %H:%M")
        )?;
        writeln!(f, "Total trades:  {}", results.total_trades)?;
        writeln!(
            f,
            "Win/Loss:      {} / {}",
            results.winning_trades, results.losing_trades
        )?;
        writeln!(f, "Win rate:      {:.1}%", results.win_rate * 100.0)?;
        writeln!(f, "Total PnL:     ${:.2}", results.total_pnl)?;
        writeln!(f, "Avg PnL/trade: ${:.4}", results.avg_pnl_per_trade)?;
        writeln!(f, "Sharpe ratio:  {:.2}", results.sharpe_ratio)?;
        writeln!(f, "Profit factor: {:.2}", results.profit_factor)?;
        writeln!(f, "Max drawdown:  {:.2}%", results.max_drawdown * dec!(100))?;
        writeln!(f, "Avg hold time: {:.0}s", results.avg_holding_time_secs)?;
        writeln!(f, "Largest win:   ${:.4}", results.largest_win)?;
        writeln!(f, "Largest loss:  ${:.4}", results.largest_loss)?;
        Ok(())
    }
}
