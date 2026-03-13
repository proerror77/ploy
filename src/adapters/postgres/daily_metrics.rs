use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::Row;

use crate::error::Result;

use super::PostgresStore;

/// Daily metrics structure
#[derive(Debug, Clone)]
pub struct DailyMetrics {
    pub date: NaiveDate,
    pub total_cycles: i32,
    pub completed_cycles: i32,
    pub aborted_cycles: i32,
    pub leg2_completions: i32,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub consecutive_failures: i32,
    pub halted: bool,
    pub halt_reason: Option<String>,
}

impl PostgresStore {
    /// Get or create today's metrics
    pub async fn get_or_create_daily_metrics(&self, date: NaiveDate) -> Result<DailyMetrics> {
        self.ensure_daily_metrics_row(date).await?;

        let row = sqlx::query(
            r#"
            SELECT date, total_cycles, completed_cycles, aborted_cycles, leg2_completions,
                   total_pnl, max_drawdown, consecutive_failures, halted, halt_reason
            FROM daily_metrics WHERE date = $1
            "#,
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await?;

        Ok(DailyMetrics {
            date: row.get("date"),
            total_cycles: row.get("total_cycles"),
            completed_cycles: row.get("completed_cycles"),
            aborted_cycles: row.get("aborted_cycles"),
            leg2_completions: row.get("leg2_completions"),
            total_pnl: row.get("total_pnl"),
            max_drawdown: row.get("max_drawdown"),
            consecutive_failures: row.get("consecutive_failures"),
            halted: row.get("halted"),
            halt_reason: row.get("halt_reason"),
        })
    }

    /// Increment cycle count
    pub async fn increment_cycle_count(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query("UPDATE daily_metrics SET total_cycles = total_cycles + 1 WHERE date = $1")
            .bind(date)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record cycle completion
    pub async fn record_cycle_completion(&self, date: NaiveDate, pnl: Decimal) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                completed_cycles = completed_cycles + 1,
                leg2_completions = leg2_completions + 1,
                total_pnl = total_pnl + $1,
                consecutive_failures = 0
            WHERE date = $2
            "#,
        )
        .bind(pnl)
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record cycle abort
    pub async fn record_cycle_abort(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                aborted_cycles = aborted_cycles + 1,
                consecutive_failures = consecutive_failures + 1
            WHERE date = $1
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record cycle abort without counting as a failure.
    ///
    /// Useful for expected/neutral aborts (e.g. IOC order got 0 fill) where we should track
    /// the abort rate but not trip consecutive-failure logic.
    pub async fn record_cycle_abort_neutral(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                aborted_cycles = aborted_cycles + 1
            WHERE date = $1
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Halt trading
    pub async fn halt_trading(&self, date: NaiveDate, reason: &str) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query("UPDATE daily_metrics SET halted = TRUE, halt_reason = $1 WHERE date = $2")
            .bind(reason)
            .bind(date)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn ensure_daily_metrics_row(&self, date: NaiveDate) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO daily_metrics (date)
            VALUES ($1)
            ON CONFLICT (date) DO NOTHING
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
