use std::collections::HashMap;

use rust_decimal::prelude::*;
use rust_decimal::Decimal;

use crate::strategy::staggered_arb_backtest::StaggeredArbClosedTrade;

pub(super) fn print_staggered_summary(closed_trades: &[StaggeredArbClosedTrade]) {
    if closed_trades.is_empty() {
        println!("\n=== Staggered Arb Summary ===");
        println!("No trades executed.");
        return;
    }

    let total = closed_trades.len();
    let merges: Vec<&StaggeredArbClosedTrade> = closed_trades
        .iter()
        .filter(|t| t.exit_reason == "merge")
        .collect();
    let settlements: Vec<&StaggeredArbClosedTrade> = closed_trades
        .iter()
        .filter(|t| t.exit_reason == "settlement")
        .collect();
    let aborts: Vec<&StaggeredArbClosedTrade> = closed_trades
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

    let avg_hold = closed_trades
        .iter()
        .map(|t| t.holding_secs as f64)
        .sum::<f64>()
        / total as f64;

    println!("\n=== Staggered Arb Summary ===");
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
    for t in closed_trades {
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
    for t in closed_trades {
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
}
