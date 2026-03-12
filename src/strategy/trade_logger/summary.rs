use super::{BucketStats, SymbolStats, TradingStats};
use rust_decimal_macros::dec;

pub(super) fn format_stats(stats: &TradingStats) -> String {
    let mut output = String::new();

    push_overview(&mut output, stats);
    push_symbol_table(&mut output, stats);

    if !stats.by_time_bucket.is_empty() {
        push_time_bucket_table(&mut output, stats);
    }

    if !stats.by_strategy_mode.is_empty() {
        push_strategy_mode_table(&mut output, stats);
    }

    output
}

fn push_overview(output: &mut String, stats: &TradingStats) {
    output.push_str("\n╔══════════════════════════════════════════════════════════════╗\n");
    output.push_str("║                    TRADING STATISTICS                        ║\n");
    output.push_str("╚══════════════════════════════════════════════════════════════╝\n\n");

    output.push_str(&format!("  Total Trades:  {}\n", stats.total_trades));
    output.push_str(&format!(
        "  Wins:          {} ({:.1}%)\n",
        stats.wins,
        stats.win_rate() * dec!(100)
    ));
    output.push_str(&format!("  Losses:        {}\n", stats.losses));
    output.push_str(&format!("  Open:          {}\n", stats.open));
    output.push_str(&format!("  Total Cost:    ${:.2}\n", stats.total_cost));
    output.push_str(&format!("  Total Payout:  ${:.2}\n", stats.total_payout));
    output.push_str(&format!("  Total PnL:     ${:.2}\n", stats.total_pnl));
    output.push_str(&format!(
        "  ROI:           {:.1}%\n",
        stats.roi() * dec!(100)
    ));
}

fn push_symbol_table(output: &mut String, stats: &TradingStats) {
    output.push_str("\n  ── Per Symbol ──────────────────────────────────────────────\n\n");
    output.push_str("  Symbol     Trades  Win%    PnL       ROI\n");
    output.push_str("  ────────   ──────  ──────  ────────  ────────\n");

    let mut symbols: Vec<_> = stats.by_symbol.values().collect();
    symbols.sort_by(|a, b| b.total_pnl.cmp(&a.total_pnl));

    for symbol in symbols {
        push_symbol_row(output, symbol);
    }
}

fn push_symbol_row(output: &mut String, symbol: &SymbolStats) {
    output.push_str(&format!(
        "  {:<10} {:>4}    {:>5.1}%  ${:>7.2}  {:>6.1}%\n",
        symbol.symbol,
        symbol.total_trades,
        symbol.win_rate() * dec!(100),
        symbol.total_pnl,
        symbol.roi() * dec!(100)
    ));
}

fn push_time_bucket_table(output: &mut String, stats: &TradingStats) {
    output.push_str("\n  ── By Entry Time (minutes elapsed) ─────────────────────────\n\n");
    output.push_str("  Bucket   Trades  Win%    PnL       EV/trade  ROI\n");
    output.push_str("  ───────  ──────  ──────  ────────  ────────  ────────\n");

    for bucket in ["0-2", "2-5", "5-10", "10-15"] {
        if let Some(bucket_stats) = stats.by_time_bucket.get(bucket) {
            push_bucket_row(output, bucket, bucket_stats);
        }
    }
}

fn push_strategy_mode_table(output: &mut String, stats: &TradingStats) {
    output.push_str("\n  ── By Strategy Mode ────────────────────────────────────────\n\n");
    output.push_str("  Mode              Trades  Win%    PnL       EV/trade  ROI\n");
    output.push_str("  ───────────────── ──────  ──────  ────────  ────────  ────────\n");

    for mode in ["early_mispricing", "late_reversal"] {
        if let Some(mode_stats) = stats.by_strategy_mode.get(mode) {
            push_mode_row(output, mode, mode_stats);
        }
    }
}

fn push_mode_row(output: &mut String, mode: &str, stats: &BucketStats) {
    output.push_str(&format!(
        "  {:<17} {:>4}    {:>5.1}%  ${:>7.2}  ${:>6.2}   {:>6.1}%\n",
        mode,
        stats.trades,
        stats.win_rate() * dec!(100),
        stats.pnl,
        stats.ev_per_trade(),
        stats.roi() * dec!(100)
    ));
}

fn push_bucket_row(output: &mut String, label: &str, stats: &BucketStats) {
    output.push_str(&format!(
        "  {:<7}  {:>4}    {:>5.1}%  ${:>7.2}  ${:>6.2}   {:>6.1}%\n",
        label,
        stats.trades,
        stats.win_rate() * dec!(100),
        stats.pnl,
        stats.ev_per_trade(),
        stats.roi() * dec!(100)
    ));
}
