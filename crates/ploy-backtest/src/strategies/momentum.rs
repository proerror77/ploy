use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::BacktestResults;

/// Closed trade record for the momentum backtest strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumClosedTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
}

/// Calculate the strategy-level Sharpe ratio from momentum trades.
pub fn calculate_sharpe(trades: &[MomentumClosedTrade]) -> f64 {
    if trades.len() < 2 {
        return 0.0;
    }

    let pnls: Vec<f64> = trades
        .iter()
        .map(|trade| trade.pnl.to_f64().unwrap_or(0.0))
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

/// Build the generic `BacktestResults` view from momentum-specific closed trades.
pub fn build_results(
    trades: &[MomentumClosedTrade],
    equity_curve: &[(DateTime<Utc>, Decimal)],
    max_drawdown: Decimal,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
) -> BacktestResults {
    let total = trades.len() as u64;
    let winning = trades.iter().filter(|trade| trade.won).count() as u64;
    let losing = total - winning;
    let total_pnl: Decimal = trades.iter().map(|trade| trade.pnl).sum();

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

    let wins: Vec<Decimal> = trades
        .iter()
        .filter(|trade| trade.won)
        .map(|trade| trade.pnl)
        .collect();
    let losses: Vec<Decimal> = trades
        .iter()
        .filter(|trade| !trade.won)
        .map(|trade| trade.pnl)
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
        trades
            .iter()
            .map(|trade| trade.holding_secs as f64)
            .sum::<f64>()
            / total as f64
    } else {
        0.0
    };

    let total_volume: Decimal = trades
        .iter()
        .map(|trade| Decimal::from(trade.shares) * trade.entry_price)
        .sum();

    BacktestResults {
        start_time: data_range_start.unwrap_or_else(Utc::now),
        end_time: data_range_end.unwrap_or_else(Utc::now),
        total_trades: total,
        winning_trades: winning,
        losing_trades: losing,
        win_rate,
        total_pnl,
        total_volume,
        avg_pnl_per_trade: avg_pnl,
        max_drawdown,
        sharpe_ratio: calculate_sharpe(trades),
        profit_factor,
        avg_win,
        avg_loss,
        largest_win,
        largest_loss,
        avg_holding_time_secs: avg_holding,
        trades_by_symbol: HashMap::new(),
        trades: Vec::new(),
        equity_curve: equity_curve.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    fn sample_trade(
        entry_time: i64,
        exit_time: i64,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
        won: bool,
    ) -> MomentumClosedTrade {
        MomentumClosedTrade {
            symbol: "BTCUSDT".into(),
            direction: "Up".into(),
            entry_time: Utc.timestamp_opt(entry_time, 0).unwrap(),
            exit_time: Utc.timestamp_opt(exit_time, 0).unwrap(),
            entry_price,
            exit_price: if won { Decimal::ONE } else { Decimal::ZERO },
            shares,
            pnl,
            won,
            holding_secs: exit_time - entry_time,
        }
    }

    #[test]
    fn sharpe_is_zero_for_empty_trades() {
        assert_eq!(calculate_sharpe(&[]), 0.0);
    }

    #[test]
    fn build_results_preserves_trade_aggregates() {
        let trades = vec![
            sample_trade(
                1_700_000_000,
                1_700_000_060,
                dec!(0.40),
                10,
                dec!(6.0),
                true,
            ),
            sample_trade(
                1_700_000_120,
                1_700_000_180,
                dec!(0.30),
                20,
                dec!(-2.0),
                false,
            ),
        ];
        let equity_curve = vec![
            (Utc.timestamp_opt(1_700_000_000, 0).unwrap(), dec!(100)),
            (Utc.timestamp_opt(1_700_000_180, 0).unwrap(), dec!(104)),
        ];

        let results = build_results(
            &trades,
            &equity_curve,
            dec!(0.08),
            Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            Some(Utc.timestamp_opt(1_700_000_180, 0).unwrap()),
        );

        assert_eq!(results.total_trades, 2);
        assert_eq!(results.winning_trades, 1);
        assert_eq!(results.losing_trades, 1);
        assert!((results.win_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(results.total_pnl, dec!(4.0));
        assert_eq!(results.total_volume, dec!(10.0));
        assert_eq!(results.avg_pnl_per_trade, dec!(2.0));
        assert_eq!(results.max_drawdown, dec!(0.08));
        assert_eq!(results.avg_win, dec!(6.0));
        assert_eq!(results.avg_loss, dec!(-2.0));
        assert_eq!(results.largest_win, dec!(6.0));
        assert_eq!(results.largest_loss, dec!(-2.0));
        assert_eq!(results.avg_holding_time_secs, 60.0);
        assert_eq!(results.equity_curve, equity_curve);
        assert!((results.sharpe_ratio - calculate_sharpe(&trades)).abs() < f64::EPSILON);
    }
}
