mod daily_metrics;
mod event_registry;
mod market_data;
mod nba_team_stats;
mod recovery;
mod strategy_state;

use crate::domain::{Cycle, DumpSignal, Order, OrderStatus, Side, StrategyState};
use crate::error::{PloyError, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use tracing::info;

pub use daily_metrics::DailyMetrics;
pub use recovery::{IncompleteCycle, OrphanedOrder, RecoverySummary};
pub use strategy_state::PersistedState;

/// PostgreSQL storage adapter
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Create a new PostgreSQL store
    pub async fn new(database_url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;

        info!("Connected to PostgreSQL");
        Ok(Self { pool })
    }

    /// Create a PostgreSQL store from an existing connection pool (zero-cost reuse)
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run migrations
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        info!("Database migrations completed");
        Ok(())
    }

    /// Get the connection pool
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ==================== Cycles ====================

    /// Create a new cycle
    pub async fn create_cycle(&self, round_id: i32, state: StrategyState) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO cycles (round_id, state)
            VALUES ($1, $2)
            RETURNING id
            "#,
        )
        .bind(round_id)
        .bind(state.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Update cycle state with optimistic locking.
    /// Returns `true` if the update succeeded, `false` if version conflict.
    pub async fn update_cycle_state(
        &self,
        cycle_id: i32,
        state: StrategyState,
        expected_version: i32,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE cycles SET state = $1, version = version + 1 WHERE id = $2 AND version = $3",
        )
        .bind(state.as_str())
        .bind(cycle_id)
        .bind(expected_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Update cycle with Leg1 fill, using optimistic locking.
    /// Returns `true` if the update succeeded, `false` if version conflict.
    pub async fn update_cycle_leg1(
        &self,
        cycle_id: i32,
        side: Side,
        entry_price: Decimal,
        shares: u64,
        expected_version: i32,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE cycles SET
                state = 'LEG1_FILLED',
                leg1_side = $1,
                leg1_entry_price = $2,
                leg1_shares = $3,
                leg1_filled_at = NOW(),
                version = version + 1
            WHERE id = $4 AND version = $5
            "#,
        )
        .bind(side.as_str())
        .bind(entry_price)
        .bind(shares as i32)
        .bind(cycle_id)
        .bind(expected_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Update cycle with Leg2 fill and PnL, using optimistic locking.
    /// Returns `true` if the update succeeded, `false` if version conflict.
    pub async fn update_cycle_leg2(
        &self,
        cycle_id: i32,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
        expected_version: i32,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE cycles SET
                state = 'CYCLE_COMPLETE',
                leg2_entry_price = $1,
                leg2_shares = $2,
                leg2_filled_at = NOW(),
                pnl = $3,
                version = version + 1
            WHERE id = $4 AND version = $5
            "#,
        )
        .bind(entry_price)
        .bind(shares as i32)
        .bind(pnl)
        .bind(cycle_id)
        .bind(expected_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Abort a cycle
    pub async fn abort_cycle(&self, cycle_id: i32, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE cycles SET state = 'ABORT', abort_reason = $1 WHERE id = $2
            "#,
        )
        .bind(reason)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get cycle by ID
    pub async fn get_cycle(&self, cycle_id: i32) -> Result<Option<Cycle>> {
        let row = sqlx::query(
            r#"
            SELECT id, round_id, state, leg1_side, leg1_entry_price, leg1_shares, leg1_filled_at,
                   leg2_entry_price, leg2_shares, leg2_filled_at, pnl, version, created_at, updated_at
            FROM cycles WHERE id = $1
            "#,
        )
        .bind(cycle_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Cycle {
            id: Some(r.get("id")),
            round_id: r.get("round_id"),
            state: r
                .get::<String, _>("state")
                .as_str()
                .try_into()
                .unwrap_or(StrategyState::Idle),
            leg1_side: r
                .get::<Option<String>, _>("leg1_side")
                .and_then(|s| Side::try_from(s.as_str()).ok()),
            leg1_entry_price: r.get("leg1_entry_price"),
            leg1_shares: r.get::<Option<i32>, _>("leg1_shares").map(|s| s as u64),
            leg1_filled_at: r.get("leg1_filled_at"),
            leg2_entry_price: r.get("leg2_entry_price"),
            leg2_shares: r.get::<Option<i32>, _>("leg2_shares").map(|s| s as u64),
            leg2_filled_at: r.get("leg2_filled_at"),
            pnl: r.get("pnl"),
            version: r.get::<Option<i32>, _>("version").unwrap_or(0),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    // ==================== Orders ====================

    /// Insert a new order
    pub async fn insert_order(&self, order: &Order) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO orders (
                cycle_id, leg, client_order_id, exchange_order_id, market_side, order_side,
                token_id, shares, limit_price, avg_fill_price, filled_shares, status,
                submitted_at, filled_at, error, strategy_id, fee
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
            RETURNING id
            "#,
        )
        .bind(order.cycle_id)
        .bind(order.leg as i32)
        .bind(&order.client_order_id)
        .bind(&order.exchange_order_id)
        .bind(order.market_side.as_str())
        .bind(order.order_side.to_string())
        .bind(&order.token_id)
        .bind(order.shares as i32)
        .bind(order.limit_price)
        .bind(order.avg_fill_price)
        .bind(order.filled_shares as i32)
        .bind(format!("{:?}", order.status))
        .bind(order.submitted_at)
        .bind(order.filled_at)
        .bind(&order.error)
        .bind(&order.strategy_id)
        .bind(order.fee)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Update order status
    pub async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE orders SET
                status = $1,
                exchange_order_id = COALESCE($2, exchange_order_id),
                submitted_at = CASE
                    WHEN $1 = 'Submitted' AND submitted_at IS NULL THEN NOW()
                    ELSE submitted_at
                END,
                cancelled_at = CASE
                    WHEN $1 = 'Cancelled' AND cancelled_at IS NULL THEN NOW()
                    ELSE cancelled_at
                END,
                updated_at = NOW()
            WHERE client_order_id = $3
            "#,
        )
        .bind(format!("{:?}", status))
        .bind(exchange_order_id)
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(PloyError::Validation(format!(
                "order not found for status update: {}",
                client_order_id
            )));
        }
        Ok(())
    }

    /// Update order fill
    pub async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE orders SET
                filled_shares = $1,
                avg_fill_price = $2,
                status = $3,
                filled_at = CASE
                    WHEN $3 = 'Filled' AND filled_at IS NULL THEN NOW()
                    ELSE filled_at
                END,
                cancelled_at = CASE
                    WHEN $3 = 'Cancelled' AND cancelled_at IS NULL THEN NOW()
                    ELSE cancelled_at
                END,
                updated_at = NOW()
            WHERE client_order_id = $4
            "#,
        )
        .bind(filled_shares as i32)
        .bind(avg_fill_price)
        .bind(format!("{:?}", status))
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(PloyError::Validation(format!(
                "order not found for fill update: {}",
                client_order_id
            )));
        }
        Ok(())
    }

    /// Update order fee after fill
    pub async fn update_order_fee(&self, client_order_id: &str, fee: Decimal) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE orders SET fee = $1, updated_at = NOW()
            WHERE client_order_id = $2
            "#,
        )
        .bind(fee)
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Dump Signals ====================

    /// Insert dump signal
    pub async fn insert_dump_signal(&self, signal: &DumpSignal, round_id: i32) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO dump_signals (
                round_id, side, trigger_price, reference_price, drop_pct,
                spread_bps, was_valid, timestamp
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id
            "#,
        )
        .bind(round_id)
        .bind(signal.side.as_str())
        .bind(signal.trigger_price)
        .bind(signal.reference_price)
        .bind(signal.drop_pct)
        .bind(signal.spread_bps as i32)
        .bind(signal.is_valid(500)) // TODO: use config
        .bind(signal.timestamp)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }
}

impl PostgresStore {
    // ==================== NBA Comeback Stats ====================
}

// Implement Side::try_from for database strings
impl TryFrom<&str> for Side {
    type Error = String;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s.to_uppercase().as_str() {
            "UP" => Ok(Side::Up),
            "DOWN" => Ok(Side::Down),
            _ => Err(format!("Unknown side: {}", s)),
        }
    }
}
