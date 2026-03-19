use super::*;

impl StaggeredArbBacktestEngine {
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
        let total_losses_abs: Decimal = losses.iter().map(|l| l.abs()).sum();
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
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        let sharpe = self.calculate_sharpe();

        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|t| Decimal::from(t.shares) * t.leg1_price)
            .sum();

        let start_time = self.data_range_start.unwrap_or(Utc::now());
        let end_time = self.data_range_end.unwrap_or(Utc::now());

        BacktestResults {
            start_time,
            end_time,
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: sharpe,
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
            .map(|t| t.pnl.to_f64().unwrap_or(0.0))
            .collect();
        let n = pnls.len() as f64;
        let mean = pnls.iter().sum::<f64>() / n;
        let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std_dev = variance.sqrt();
        if std_dev < 1e-10 {
            return 0.0;
        }
        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std_dev) * trades_per_year.sqrt()
    }

    /// Print staggered-arb-specific summary stats.
    pub fn print_summary(&self, title: &str) {
        if self.closed_trades.is_empty() {
            println!("\n=== {title} Summary ===");
            println!("No trades executed.");
            return;
        }

        let total = self.closed_trades.len();
        let merges: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "merge")
            .collect();
        let settlements: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .collect();
        let aborts: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason != "merge" && t.exit_reason != "settlement")
            .collect();

        let merge_count = merges.len();
        let leg2_fill_rate = merge_count as f64 / total as f64 * 100.0;

        let avg_compression = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| t.final_sum.map(|fs| t.initial_sum - fs))
                .map(|d| d.to_f64().unwrap_or(0.0))
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        let avg_wait = if !merges.is_empty() {
            merges
                .iter()
                .filter_map(|t| {
                    t.leg2_time
                        .map(|l2| (l2 - t.leg1_time).num_seconds() as f64)
                })
                .sum::<f64>()
                / merges.len() as f64
        } else {
            0.0
        };

        let mut abort_reasons: HashMap<&str, usize> = HashMap::new();
        for t in &aborts {
            *abort_reasons.entry(&t.exit_reason).or_default() += 1;
        }

        let merge_pnl: Decimal = merges.iter().map(|t| t.pnl).sum();
        let single_pnl: Decimal = settlements.iter().map(|t| t.pnl).sum();
        let abort_pnl: Decimal = aborts.iter().map(|t| t.pnl).sum();

        let direction_correct = merges.iter().filter(|t| t.won).count();
        let direction_accuracy = if !merges.is_empty() {
            direction_correct as f64 / merges.len() as f64 * 100.0
        } else {
            0.0
        };

        let avg_hold = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        println!("\n=== {title} Summary ===");
        println!("Total attempts:     {}", total);
        println!(
            "Leg2 fill rate:     {:.1}% ({}/{})",
            leg2_fill_rate, merge_count, total
        );
        println!("Settlements:        {} (single-leg)", settlements.len());
        println!("Aborts:             {}", aborts.len());
        println!();
        println!("Avg spread compression: {:.4}", avg_compression);
        println!("Avg Leg2 wait time:     {:.1}s", avg_wait);
        println!("Avg holding time:       {:.1}s", avg_hold);
        println!();
        println!("PnL breakdown:");
        println!("  Merges:      ${:.2} ({} trades)", merge_pnl, merge_count);
        println!(
            "  Settlements: ${:.2} ({} trades)",
            single_pnl,
            settlements.len()
        );
        println!("  Aborts:      ${:.2} ({} trades)", abort_pnl, aborts.len());
        println!();
        if !abort_reasons.is_empty() {
            println!("Abort reasons:");
            for (reason, count) in &abort_reasons {
                println!("  {:<16} {}", reason, count);
            }
            println!();
        }
        println!(
            "Direction accuracy: {:.1}% (merge wins)",
            direction_accuracy
        );
        println!(
            "Capital turnover:   {} merges, avg hold {:.0}s",
            merge_count, avg_hold
        );

        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry = symbol_stats
                .entry(&t.symbol)
                .or_insert((0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            entry.2 += t.pnl;
        }
        if symbol_stats.len() > 1 {
            println!("\nPer-symbol:");
            println!(
                "  {:<12} {:>6} {:>6} {:>8} {:>10}",
                "Symbol", "Trades", "Wins", "WinRate", "PnL"
            );
            let mut syms: Vec<&&str> = symbol_stats.keys().collect();
            syms.sort();
            for sym in syms {
                let (t, w, p) = symbol_stats[sym];
                let wr = if t > 0 {
                    w as f64 / t as f64 * 100.0
                } else {
                    0.0
                };
                println!("  {:<12} {:>6} {:>6} {:>7.1}% ${:>9.2}", sym, t, w, wr, p);
            }
        }

        let mut window_stats: HashMap<&str, (usize, usize, usize, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let label = match t.window_duration_secs {
                0..=330 => "5m",
                331..=930 => "15m",
                _ => "other",
            };
            let entry = window_stats
                .entry(label)
                .or_insert((0, 0, 0, Decimal::ZERO));
            entry.0 += 1;
            if t.won {
                entry.1 += 1;
            }
            if t.exit_reason == "merge" {
                entry.2 += 1;
            }
            entry.3 += t.pnl;
        }
        println!("\nPer-window breakdown:");
        println!(
            "  {:<8} {:>6} {:>6} {:>8} {:>8} {:>10}",
            "Window", "Trades", "Wins", "WinRate", "Merges", "PnL"
        );
        let mut labels: Vec<&&str> = window_stats.keys().collect();
        labels.sort();
        for label in labels {
            let (t, w, m, p) = window_stats[label];
            let wr = if t > 0 {
                w as f64 / t as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<8} {:>6} {:>6} {:>7.1}% {:>8} ${:>9.2}",
                label, t, w, wr, m, p
            );
        }

        let trades_with_greeks: Vec<&StaggeredArbClosedTrade> = self
            .closed_trades
            .iter()
            .filter(|t| t.entry_delta.is_some())
            .collect();
        if !trades_with_greeks.is_empty() {
            let n = trades_with_greeks.len() as f64;
            let avg_delta = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_delta)
                .sum::<f64>()
                / n;
            let avg_gamma = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_gamma)
                .sum::<f64>()
                / n;
            let avg_theta = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_theta)
                .sum::<f64>()
                / n;
            let avg_fv = trades_with_greeks
                .iter()
                .filter_map(|t| t.entry_fair_value)
                .sum::<f64>()
                / n;

            let winning_greeks: Vec<&StaggeredArbClosedTrade> = trades_with_greeks
                .iter()
                .filter(|t| t.won)
                .copied()
                .collect();
            let losing_greeks: Vec<&StaggeredArbClosedTrade> = trades_with_greeks
                .iter()
                .filter(|t| !t.won)
                .copied()
                .collect();

            println!("\nGreeks at entry (avg):");
            println!("  Delta:      {:.6}", avg_delta);
            println!("  Gamma:      {:.6}", avg_gamma);
            println!("  Theta:      {:.6}/s", avg_theta);
            println!("  Fair Value: {:.4}", avg_fv);

            if !winning_greeks.is_empty() && !losing_greeks.is_empty() {
                let win_gamma = winning_greeks
                    .iter()
                    .filter_map(|t| t.entry_gamma)
                    .map(|g| g.abs())
                    .sum::<f64>()
                    / winning_greeks.len() as f64;
                let lose_gamma = losing_greeks
                    .iter()
                    .filter_map(|t| t.entry_gamma)
                    .map(|g| g.abs())
                    .sum::<f64>()
                    / losing_greeks.len() as f64;
                println!(
                    "  Win |gamma|:  {:.6}  vs  Lose |gamma|: {:.6}",
                    win_gamma, lose_gamma
                );
            }
        }
    }

    pub fn print_staggered_summary(&self) {
        self.print_summary("Staggered Arb");
    }
}
