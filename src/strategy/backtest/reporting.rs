use rust_decimal_macros::dec;
use std::fmt;

use super::BacktestResults;

impl BacktestResults {
    pub fn report(&self) -> String {
        let mut report = String::new();

        report.push_str("╔══════════════════════════════════════════════════════════════╗\n");
        report.push_str("║              VOLATILITY ARBITRAGE BACKTEST REPORT            ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!(
            "║ Period: {} to {}\n",
            self.start_time.format("%Y-%m-%d"),
            self.end_time.format("%Y-%m-%d")
        ));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ PERFORMANCE SUMMARY                                          ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!(
            "║ Total Trades:      {:>10}                              ║\n",
            self.total_trades
        ));
        report.push_str(&format!(
            "║ Winning Trades:    {:>10}                              ║\n",
            self.winning_trades
        ));
        report.push_str(&format!(
            "║ Win Rate:          {:>10.2}%                             ║\n",
            self.win_rate * 100.0
        ));
        report.push_str(&format!(
            "║ Total PnL:         ${:>9.2}                             ║\n",
            self.total_pnl
        ));
        report.push_str(&format!(
            "║ Total Volume:      ${:>9.2}                             ║\n",
            self.total_volume
        ));
        report.push_str(&format!(
            "║ Avg PnL/Trade:     ${:>9.2}                             ║\n",
            self.avg_pnl_per_trade
        ));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ RISK METRICS                                                 ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!(
            "║ Max Drawdown:      {:>10.2}%                             ║\n",
            self.max_drawdown * dec!(100)
        ));
        report.push_str(&format!(
            "║ Sharpe Ratio:      {:>10.2}                              ║\n",
            self.sharpe_ratio
        ));
        report.push_str(&format!(
            "║ Profit Factor:     {:>10.2}                              ║\n",
            self.profit_factor
        ));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ WIN/LOSS ANALYSIS                                            ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        report.push_str(&format!(
            "║ Average Win:       ${:>9.2}                             ║\n",
            self.avg_win
        ));
        report.push_str(&format!(
            "║ Average Loss:      ${:>9.2}                             ║\n",
            self.avg_loss
        ));
        report.push_str(&format!(
            "║ Largest Win:       ${:>9.2}                             ║\n",
            self.largest_win
        ));
        report.push_str(&format!(
            "║ Largest Loss:      ${:>9.2}                             ║\n",
            self.largest_loss
        ));

        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");
        report.push_str("║ BY SYMBOL                                                    ║\n");
        report.push_str("╠══════════════════════════════════════════════════════════════╣\n");

        for (symbol, stats) in &self.trades_by_symbol {
            report.push_str(&format!(
                "║ {:8} | Trades: {:>4} | Win: {:>5.1}% | PnL: ${:>8.2}        ║\n",
                symbol,
                stats.total_trades,
                stats.win_rate * 100.0,
                stats.total_pnl
            ));
        }

        report.push_str("╚══════════════════════════════════════════════════════════════╝\n");
        report
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

impl fmt::Display for BacktestResults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report())
    }
}
