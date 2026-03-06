use anyhow::Result;
use ploy_backtest::BacktestResults;
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::info;

use super::MomentumBacktestConfig;

/// Save backtest results to `strategy_evaluations` + `backtest_runs` tables.
pub async fn save_backtest_results(
    pool: &PgPool,
    config: &MomentumBacktestConfig,
    results: &BacktestResults,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let status = evaluation_status(results.sharpe_ratio);

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

fn evaluation_status(sharpe_ratio: f64) -> &'static str {
    if sharpe_ratio > 1.0 {
        "PASS"
    } else if sharpe_ratio > 0.5 {
        "WARN"
    } else {
        "FAIL"
    }
}

#[cfg(test)]
mod tests {
    use super::evaluation_status;

    #[test]
    fn uses_fail_warn_pass_status_bands() {
        assert_eq!(evaluation_status(0.5), "FAIL");
        assert_eq!(evaluation_status(0.51), "WARN");
        assert_eq!(evaluation_status(1.0), "WARN");
        assert_eq!(evaluation_status(1.01), "PASS");
    }
}
