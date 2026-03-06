use std::collections::HashMap;

use rust_decimal::prelude::*;
use rust_decimal::Decimal;

use crate::strategy::directional_backtest::DirectionalClosedTrade;

pub(super) fn print_directional_summary(closed_trades: &[DirectionalClosedTrade]) {
    let total = closed_trades.len();

    let settled = closed_trades
        .iter()
        .filter(|t| t.exit_reason == "settlement")
        .count();
    let settlement_rate = settled as f64 / total as f64 * 100.0;

    let mut exit_counts: HashMap<&str, usize> = HashMap::new();
    for t in closed_trades {
        *exit_counts.entry(&t.exit_reason).or_default() += 1;
    }

    let winner_p: Vec<f64> = closed_trades
        .iter()
        .filter(|t| t.won)
        .map(|t| t.entry_p_hat)
        .collect();
    let loser_p: Vec<f64> = closed_trades
        .iter()
        .filter(|t| !t.won)
        .map(|t| t.entry_p_hat)
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

    let ev_nets: Vec<f64> = closed_trades.iter().map(|t| t.entry_ev_net).collect();
    let avg_ev = ev_nets.iter().sum::<f64>() / total as f64;

    let up_trades = closed_trades.iter().filter(|t| t.direction == "UP").count();
    let down_trades = total - up_trades;
    let up_wins = closed_trades
        .iter()
        .filter(|t| t.direction == "UP" && t.won)
        .count();
    let down_wins = closed_trades
        .iter()
        .filter(|t| t.direction == "DOWN" && t.won)
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

    let sigmas: Vec<f64> = closed_trades.iter().map(|t| t.entry_sigma).collect();
    let avg_sigma = sigmas.iter().sum::<f64>() / sigmas.len().max(1) as f64;
    let min_sigma = sigmas.iter().cloned().fold(f64::MAX, f64::min);
    let max_sigma = sigmas.iter().cloned().fold(f64::MIN, f64::max);
    println!("\nVolatility:");
    println!("  Avg σ at entry: {:.5}", avg_sigma);
    println!("  Min σ: {:.5}  Max σ: {:.5}", min_sigma, max_sigma);

    let hold_times: Vec<i64> = closed_trades.iter().map(|t| t.holding_secs).collect();
    let avg_hold = hold_times.iter().sum::<i64>() as f64 / hold_times.len().max(1) as f64;
    let min_hold = hold_times.iter().min().copied().unwrap_or(0);
    let max_hold = hold_times.iter().max().copied().unwrap_or(0);
    println!("\nHolding time:");
    println!(
        "  Avg: {:.0}s  Min: {}s  Max: {}s",
        avg_hold, min_hold, max_hold
    );

    let entry_prices: Vec<f64> = closed_trades
        .iter()
        .map(|t| t.entry_price.to_f64().unwrap_or(0.0))
        .collect();
    let avg_entry = entry_prices.iter().sum::<f64>() / entry_prices.len().max(1) as f64;
    println!("  Avg entry price: ${:.4}", avg_entry);

    let mut symbol_stats: HashMap<&str, (usize, usize, Decimal, Decimal)> = HashMap::new();
    for t in closed_trades {
        let entry = symbol_stats
            .entry(&t.symbol)
            .or_insert((0, 0, Decimal::ZERO, Decimal::ZERO));
        entry.0 += 1;
        if t.won {
            entry.1 += 1;
        }
        entry.2 += t.pnl;
        entry.3 += Decimal::from(t.shares) * t.entry_price;
    }

    let mut symbols: Vec<&&str> = symbol_stats.keys().collect();
    symbols.sort();

    println!("\nPer-symbol breakdown:");
    println!(
        "  {:<12} {:>6} {:>6} {:>8} {:>12} {:>12}",
        "Symbol", "Trades", "Wins", "WinRate", "PnL", "Volume"
    );
    println!("  {}", "-".repeat(62));
    for sym in &symbols {
        let (trades, wins, pnl, vol) = symbol_stats[*sym];
        let wr = if trades > 0 {
            wins as f64 / trades as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
            sym, trades, wins, wr, pnl, vol
        );
    }
    let total_vol: Decimal = symbol_stats.values().map(|v| v.3).sum();
    let total_pnl: Decimal = symbol_stats.values().map(|v| v.2).sum();
    println!("  {}", "-".repeat(62));
    println!(
        "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
        "TOTAL",
        total,
        closed_trades.iter().filter(|t| t.won).count(),
        closed_trades.iter().filter(|t| t.won).count() as f64 / total as f64 * 100.0,
        total_pnl,
        total_vol
    );
}
