use crate::domain::Side;
use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use sqlx::types::chrono::{DateTime, Utc};
use tracing::debug;

use super::{Position, PositionManager, PositionStatus, PositionSummary};

type PositionRow = (
    i32,
    String,
    String,
    String,
    String,
    i64,
    Decimal,
    Decimal,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    String,
    Option<Decimal>,
    Option<Decimal>,
    Option<String>,
);

impl PositionManager {
    pub(super) async fn fetch_position(&self, position_id: i32) -> Result<Position> {
        let row = sqlx::query_as::<_, PositionRow>(
            r#"
            SELECT id, event_id, symbol, token_id, market_side,
                   shares, avg_entry_price, amount_usd,
                   opened_at, closed_at, status, pnl, exit_price, strategy_id
            FROM positions
            WHERE id = $1
            "#,
        )
        .bind(position_id)
        .fetch_one(self.store.pool())
        .await?;

        decode_position(row)
    }

    pub(super) async fn fetch_open_positions(&self) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, PositionRow>(
            r#"
            SELECT id, event_id, symbol, token_id, market_side,
                   shares, avg_entry_price, amount_usd,
                   opened_at, closed_at, status, pnl, exit_price, strategy_id
            FROM positions
            WHERE status = 'OPEN'
            ORDER BY opened_at DESC
            "#,
        )
        .fetch_all(self.store.pool())
        .await?;

        let positions = rows
            .into_iter()
            .filter_map(|row| decode_position(row).ok())
            .collect::<Vec<_>>();

        debug!("Found {} open positions", positions.len());
        Ok(positions)
    }

    pub(super) async fn fetch_open_positions_by_symbol(&self, symbol: &str) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, PositionRow>(
            r#"
            SELECT id, event_id, symbol, token_id, market_side,
                   shares, avg_entry_price, amount_usd,
                   opened_at, closed_at, status, pnl, exit_price, strategy_id
            FROM positions
            WHERE status = 'OPEN' AND symbol = $1
            ORDER BY opened_at DESC
            "#,
        )
        .bind(symbol)
        .fetch_all(self.store.pool())
        .await?;

        let positions = rows
            .into_iter()
            .filter_map(|row| decode_position(row).ok())
            .collect::<Vec<_>>();

        debug!("Found {} open positions for {}", positions.len(), symbol);
        Ok(positions)
    }

    pub(super) async fn fetch_summary(&self) -> Result<PositionSummary> {
        let row = sqlx::query_as::<_, (i32, i32, Decimal, Decimal, Decimal)>(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'OPEN')::INT as total_open,
                COUNT(*) FILTER (WHERE status = 'CLOSED')::INT as total_closed,
                COALESCE(SUM(pnl) FILTER (WHERE status = 'CLOSED'), 0) as total_pnl,
                COALESCE(AVG(pnl) FILTER (WHERE status = 'CLOSED'), 0) as avg_pnl,
                CASE
                    WHEN COUNT(*) FILTER (WHERE status = 'CLOSED') > 0 THEN
                        COUNT(*) FILTER (WHERE status = 'CLOSED' AND pnl > 0)::DECIMAL /
                        COUNT(*) FILTER (WHERE status = 'CLOSED')::DECIMAL
                    ELSE 0
                END as win_rate
            FROM positions
            "#,
        )
        .fetch_one(self.store.pool())
        .await?;

        Ok(PositionSummary {
            total_open: row.0,
            total_closed: row.1,
            total_pnl: row.2,
            avg_pnl: row.3,
            win_rate: row.4,
        })
    }

    pub(super) async fn count_open_positions_for_symbol(&self, symbol: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM positions
            WHERE status = 'OPEN' AND symbol = $1
            "#,
        )
        .bind(symbol)
        .fetch_one(self.store.pool())
        .await?;

        Ok(count)
    }

    pub(super) async fn count_all_open_positions(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM positions
            WHERE status = 'OPEN'
            "#,
        )
        .fetch_one(self.store.pool())
        .await?;

        Ok(count)
    }

    pub(super) async fn fetch_open_position_by_token(&self, token_id: &str) -> Result<Option<Position>> {
        let row = sqlx::query_as::<_, PositionRow>(
            r#"
            SELECT id, event_id, symbol, token_id, market_side,
                   shares, avg_entry_price, amount_usd,
                   opened_at, closed_at, status, pnl, exit_price, strategy_id
            FROM positions
            WHERE token_id = $1 AND status = 'OPEN'
            ORDER BY opened_at DESC
            LIMIT 1
            "#,
        )
        .bind(token_id)
        .fetch_optional(self.store.pool())
        .await?;

        row.map(decode_position).transpose()
    }
}

fn decode_position(row: PositionRow) -> Result<Position> {
    let market_side = match row.4.as_str() {
        "UP" => Side::Up,
        "DOWN" => Side::Down,
        _ => {
            return Err(PloyError::Internal(format!(
                "Invalid market side: {}",
                row.4
            )))
        }
    };

    let status = match row.10.as_str() {
        "OPEN" => PositionStatus::Open,
        "CLOSED" => PositionStatus::Closed,
        _ => {
            return Err(PloyError::Internal(format!(
                "Invalid position status: {}",
                row.10
            )))
        }
    };

    Ok(Position {
        id: row.0,
        event_id: row.1,
        symbol: row.2,
        token_id: row.3,
        market_side,
        shares: row.5,
        avg_entry_price: row.6,
        amount_usd: row.7,
        opened_at: row.8,
        closed_at: row.9,
        status,
        pnl: row.11,
        exit_price: row.12,
        strategy_id: row.13,
    })
}
