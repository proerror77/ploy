use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::BacktestResults;

/// A closed staggered-arb trade for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbClosedTrade {
    pub symbol: String,
    pub leg1_direction: String,
    pub leg1_price: Decimal,
    pub leg1_time: DateTime<Utc>,
    pub leg2_price: Option<Decimal>,
    pub leg2_time: Option<DateTime<Utc>>,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    pub initial_sum: Decimal,
    pub final_sum: Option<Decimal>,
    pub entry_p_hat: f64,
    pub entry_sigma: f64,
    pub best_sum_seen: Decimal,
    pub s0: Decimal,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    pub window_duration_secs: i64,
}

/// Calculate the strategy-level Sharpe ratio from staggered-arb trades.
pub fn calculate_sharpe(trades: &[StaggeredArbClosedTrade]) -> f64 {
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

/// Build the generic `BacktestResults` view from staggered-arb closed trades.
pub fn build_results(
    trades: &[StaggeredArbClosedTrade],
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
        .map(|trade| Decimal::from(trade.shares) * trade.leg1_price)
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
        leg1_price: Decimal,
        shares: u64,
        pnl: Decimal,
        won: bool,
        exit_reason: &str,
    ) -> StaggeredArbClosedTrade {
        StaggeredArbClosedTrade {
            symbol: "BTCUSDT".into(),
            leg1_direction: "UP".into(),
            leg1_price,
            leg1_time: Utc.timestamp_opt(entry_time, 0).unwrap(),
            leg2_price: Some(dec!(0.42)),
            leg2_time: Some(Utc.timestamp_opt(exit_time, 0).unwrap()),
            shares,
            pnl,
            won,
            holding_secs: exit_time - entry_time,
            exit_reason: exit_reason.into(),
            initial_sum: dec!(0.92),
            final_sum: Some(dec!(0.98)),
            entry_p_hat: 0.61,
            entry_sigma: 0.005,
            best_sum_seen: dec!(0.95),
            s0: dec!(100),
            window_duration_secs: 300,
        }
    }

    #[test]
    fn sharpe_is_zero_for_insufficient_trades() {
        let trades = vec![sample_trade(
            1_700_000_000,
            1_700_000_060,
            dec!(0.48),
            10,
            dec!(1.0),
            true,
            "merge",
        )];

        assert_eq!(calculate_sharpe(&trades), 0.0);
    }

    #[test]
    fn build_results_preserves_staggered_trade_aggregates() {
        let trades = vec![
            sample_trade(
                1_700_000_000,
                1_700_000_060,
                dec!(0.48),
                10,
                dec!(1.5),
                true,
                "merge",
            ),
            sample_trade(
                1_700_000_120,
                1_700_000_300,
                dec!(0.32),
                20,
                dec!(-0.5),
                false,
                "abort_timeout",
            ),
        ];
        let equity_curve = vec![
            (Utc.timestamp_opt(1_700_000_000, 0).unwrap(), dec!(100)),
            (Utc.timestamp_opt(1_700_000_300, 0).unwrap(), dec!(101)),
        ];

        let results = build_results(
            &trades,
            &equity_curve,
            dec!(0.04),
            Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            Some(Utc.timestamp_opt(1_700_000_300, 0).unwrap()),
        );

        assert_eq!(results.total_trades, 2);
        assert_eq!(results.winning_trades, 1);
        assert_eq!(results.losing_trades, 1);
        assert!((results.win_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(results.total_pnl, dec!(1.0));
        assert_eq!(results.total_volume, dec!(11.20));
        assert_eq!(results.avg_pnl_per_trade, dec!(0.5));
        assert_eq!(results.max_drawdown, dec!(0.04));
        assert_eq!(results.avg_win, dec!(1.5));
        assert_eq!(results.avg_loss, dec!(-0.5));
        assert_eq!(results.largest_win, dec!(1.5));
        assert_eq!(results.largest_loss, dec!(-0.5));
        assert_eq!(results.avg_holding_time_secs, 120.0);
        assert_eq!(results.equity_curve, equity_curve);
        assert!((results.sharpe_ratio - calculate_sharpe(&trades)).abs() < f64::EPSILON);
    }
}
