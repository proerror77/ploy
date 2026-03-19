use std::collections::HashMap;
use anyhow::Result;
use chrono::Utc;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::info;

use crate::strategy::backtest::BacktestResults;

use super::{MomentumBacktestConfig, MomentumBacktestEngine};

impl MomentumBacktestEngine {
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
            .map(|t| Decimal::from(t.shares) * t.entry_price)
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

    pub(super) fn calculate_sharpe(&self) -> f64 {
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
}

/// Save backtest results to `strategy_evaluations` + `backtest_runs` tables.
pub async fn save_backtest_results(
    pool: &PgPool,
    config: &MomentumBacktestConfig,
    results: &BacktestResults,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    let status = if results.sharpe_ratio > 1.0 {
        "PASS"
    } else if results.sharpe_ratio > 0.5 {
        "WARN"
    } else {
        "FAIL"
    };

    let eval_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO strategy_evaluations (
            evaluated_at, strategy_id, domain, stage, status,
            score, pnl_usd, win_rate, sharpe,
            max_drawdown_pct, evidence_kind
        )
        VALUES (NOW(), 'momentum', 'crypto', 'BACKTEST', $1,
                $2, $3, $4, $5, $6, 'backtest_run')
        RETURNING id
        "#,
    )
    .bind(status)
    .bind(results.sharpe_ratio)
    .bind(results.total_pnl)
    .bind(Decimal::from_f64(results.win_rate).unwrap_or(Decimal::ZERO))
    .bind(Decimal::from_f64(results.sharpe_ratio).unwrap_or(Decimal::ZERO))
    .bind(results.max_drawdown)
    .fetch_one(&mut *tx)
    .await?;

    let config_json = serde_json::to_value(config)?;
    let equity_json = serde_json::to_value(&results.equity_curve)?;

    sqlx::query(
        r#"
        INSERT INTO backtest_runs (
            evaluation_id, strategy_id, config_hash, config_json,
            started_at, completed_at,
            data_range_start, data_range_end,
            total_trades, winning_trades, losing_trades, win_rate,
            total_pnl, sharpe_ratio, max_drawdown_pct,
            profit_factor, avg_trade_pnl, avg_holding_secs,
            equity_curve
        )
        VALUES (
            $1, 'momentum', $2, $3,
            NOW(), NOW(),
            $4, $5,
            $6, $7, $8, $9,
            $10, $11, $12,
            $13, $14, $15,
            $16
        )
        "#,
    )
    .bind(eval_id)
    .bind(&config.config_hash())
    .bind(&config_json)
    .bind(results.start_time)
    .bind(results.end_time)
    .bind(results.total_trades as i32)
    .bind(results.winning_trades as i32)
    .bind(results.losing_trades as i32)
    .bind(Decimal::from_f64(results.win_rate).unwrap_or(Decimal::ZERO))
    .bind(results.total_pnl)
    .bind(Decimal::from_f64(results.sharpe_ratio).unwrap_or(Decimal::ZERO))
    .bind(results.max_drawdown)
    .bind(Decimal::from_f64(results.profit_factor).unwrap_or(Decimal::ZERO))
    .bind(results.avg_pnl_per_trade)
    .bind(results.avg_holding_time_secs as i64)
    .bind(&equity_json)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    info!(
        "Saved backtest: evaluation #{}, status={}, sharpe={:.2}, pnl=${:.2}",
        eval_id, status, results.sharpe_ratio, results.total_pnl
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use rust_decimal_macros::dec;
    use std::collections::VecDeque;

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    #[test]
    fn test_engine_empty_feed() {
        let config =
            MomentumBacktestConfig::default_with_symbols(vec!["BTCUSDT".into()], dec!(10000));
        let mut engine = MomentumBacktestEngine::new(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_sharpe_calculation() {
        let config =
            MomentumBacktestConfig::default_with_symbols(vec!["BTCUSDT".into()], dec!(10000));
        let engine = MomentumBacktestEngine::new(config);
        assert_eq!(engine.calculate_sharpe(), 0.0);
    }
}
