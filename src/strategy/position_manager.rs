//! Position Manager
//!
//! Manages position lifecycle with database persistence:
//! - Open/close positions
//! - Track position state
//! - Calculate PnL with complete trading costs
//! - Reconcile with exchange
//!
//! # CRITICAL FIX
//! Previously, PnL calculations only considered price differences without
//! deducting any trading costs (fees, gas, slippage). This led to inflated
//! PnL figures and unrealistic backtesting results.
//!
//! Now uses TradingCostCalculator for accurate net PnL calculation.

use crate::adapters::PostgresStore;
use crate::domain::Side;
use crate::error::{PloyError, Result};
use crate::strategy::trading_costs::{TradingCostCalculator, OrderType};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Position status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT")]
pub enum PositionStatus {
    /// Position is open
    #[sqlx(rename = "OPEN")]
    Open,
    /// Position is closed
    #[sqlx(rename = "CLOSED")]
    Closed,
}

impl std::fmt::Display for PositionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PositionStatus::Open => write!(f, "OPEN"),
            PositionStatus::Closed => write!(f, "CLOSED"),
        }
    }
}

/// Position record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: i32,
    pub event_id: String,
    pub symbol: String,
    pub token_id: String,
    pub market_side: Side,
    pub shares: i64,
    pub avg_entry_price: Decimal,
    pub amount_usd: Decimal,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub status: PositionStatus,
    pub pnl: Option<Decimal>,
    pub exit_price: Option<Decimal>,
    pub strategy_id: Option<String>,
}

/// Position summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSummary {
    pub total_open: i32,
    pub total_closed: i32,
    pub total_pnl: Decimal,
    pub avg_pnl: Decimal,
    pub win_rate: Decimal,
}

/// Position manager for persistent position tracking
pub struct PositionManager {
    store: Arc<PostgresStore>,
    cost_calculator: TradingCostCalculator,
}

impl PositionManager {
    /// Create a new position manager with default cost calculator
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self {
            store,
            cost_calculator: TradingCostCalculator::new(),
        }
    }

    /// Create a new position manager with custom cost calculator
    pub fn with_cost_calculator(
        store: Arc<PostgresStore>,
        cost_calculator: TradingCostCalculator,
    ) -> Self {
        Self {
            store,
            cost_calculator,
        }
    }

    /// Open a new position
    ///
    /// # Arguments
    /// * `event_id` - Event identifier
    /// * `symbol` - Trading symbol (e.g., "BTC")
    /// * `token_id` - Token identifier
    /// * `market_side` - Market side (UP/DOWN)
    /// * `shares` - Number of shares
    /// * `entry_price` - Entry price
    /// * `strategy_id` - Optional strategy identifier
    ///
    /// # Returns
    /// Position ID
    pub async fn open_position(
        &self,
        event_id: &str,
        symbol: &str,
        token_id: &str,
        market_side: Side,
        shares: i64,
        entry_price: Decimal,
        strategy_id: Option<&str>,
    ) -> Result<i32> {
        let amount_usd = Decimal::from(shares) * entry_price;

        let side_str = match market_side {
            Side::Up => "UP",
            Side::Down => "DOWN",
        };

        let position_id: i32 = sqlx::query_scalar(
            r#"
            INSERT INTO positions (
                event_id, symbol, token_id, market_side,
                shares, avg_entry_price, amount_usd, strategy_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (event_id, token_id) DO UPDATE
            SET shares = positions.shares + EXCLUDED.shares,
                avg_entry_price = (
                    (positions.avg_entry_price * positions.shares + EXCLUDED.avg_entry_price * EXCLUDED.shares) /
                    (positions.shares + EXCLUDED.shares)
                ),
                amount_usd = positions.amount_usd + EXCLUDED.amount_usd
            RETURNING id
            "#,
        )
        .bind(event_id)
        .bind(symbol)
        .bind(token_id)
        .bind(side_str)
        .bind(shares)
        .bind(entry_price)
        .bind(amount_usd)
        .bind(strategy_id)
        .fetch_one(self.store.pool())
        .await?;

        info!(
            "Opened position #{}: {} {} shares @ {} (${:.2})",
            position_id, symbol, shares, entry_price, amount_usd
        );

        Ok(position_id)
    }

    /// Close a position
    ///
    /// # Arguments
    /// * `position_id` - Position ID to close
    /// * `exit_price` - Exit price
    /// * `exit_order_type` - Order type for exit (maker/taker)
    /// * `market_depth_ratio` - Order size relative to market depth (for slippage estimation)
    ///
    /// # Returns
    /// Net realized PnL after all trading costs
    ///
    /// # CRITICAL FIX
    /// Now calculates complete PnL including:
    /// - Entry fees (maker/taker)
    /// - Exit fees (maker/taker)
    /// - Gas costs (entry + exit)
    /// - Slippage costs (entry + exit)
    pub async fn close_position(
        &self,
        position_id: i32,
        exit_price: Decimal,
        exit_order_type: OrderType,
        market_depth_ratio: Decimal,
    ) -> Result<Decimal> {
        // Get position details
        let position = self.get_position(position_id).await?;

        if position.status == PositionStatus::Closed {
            return Err(PloyError::Internal(format!(
                "Position {} is already closed",
                position_id
            )));
        }

        // Calculate gross PnL (price difference only)
        let gross_pnl = (exit_price - position.avg_entry_price) * Decimal::from(position.shares);

        // Calculate notional values
        let entry_notional = position.amount_usd;
        let exit_notional = exit_price * Decimal::from(position.shares);

        // Assume entry was taker order (conservative assumption)
        // In production, this should be tracked in the position record
        let entry_order_type = OrderType::Taker;

        // Calculate net PnL with all trading costs
        let net_pnl = self.cost_calculator.calculate_net_pnl(
            gross_pnl,
            entry_notional,
            exit_notional,
            entry_order_type,
            exit_order_type,
            market_depth_ratio,
        );

        // Get cost breakdown for logging
        let costs = self.cost_calculator.calculate_full_costs(
            entry_notional,
            exit_notional,
            entry_order_type,
            exit_order_type,
            market_depth_ratio,
        );

        // Update position
        sqlx::query(
            r#"
            UPDATE positions
            SET status = 'CLOSED',
                closed_at = NOW(),
                exit_price = $1,
                pnl = $2
            WHERE id = $3
            "#,
        )
        .bind(exit_price)
        .bind(net_pnl)
        .bind(position_id)
        .execute(self.store.pool())
        .await?;

        info!(
            "Closed position #{}: {} @ {} | Gross PnL: ${:.2} | Costs: ${:.2} (fees: ${:.2}, gas: ${:.2}, slippage: ${:.2}) | Net PnL: ${:.2}",
            position_id,
            position.symbol,
            exit_price,
            gross_pnl,
            costs.total_cost,
            costs.entry_fee + costs.exit_fee,
            costs.gas_costs,
            costs.slippage_cost,
            net_pnl
        );

        // Warn if costs are significant relative to gross PnL
        if gross_pnl > Decimal::ZERO && costs.total_cost > gross_pnl * dec!(0.5) {
            warn!(
                "Position #{}: Trading costs (${:.2}) consumed >50% of gross PnL (${:.2})",
                position_id, costs.total_cost, gross_pnl
            );
        }

        Ok(net_pnl)
    }

    /// Get a position by ID
    pub async fn get_position(&self, position_id: i32) -> Result<Position> {
        let row = sqlx::query_as::<_, (
            i32, String, String, String, String,
            i64, Decimal, Decimal,
            DateTime<Utc>, Option<DateTime<Utc>>, String, Option<Decimal>, Option<Decimal>, Option<String>
        )>(
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
        .await
        ?;

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

    /// Get all open positions
    pub async fn get_open_positions(&self) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, (
            i32, String, String, String, String,
            i64, Decimal, Decimal,
            DateTime<Utc>, Option<DateTime<Utc>>, String, Option<Decimal>, Option<Decimal>, Option<String>
        )>(
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
        .await
        ?;

        let mut positions = Vec::new();
        for row in rows {
            let market_side = match row.4.as_str() {
                "UP" => Side::Up,
                "DOWN" => Side::Down,
                _ => continue,
            };

            let status = match row.10.as_str() {
                "OPEN" => PositionStatus::Open,
                "CLOSED" => PositionStatus::Closed,
                _ => continue,
            };

            positions.push(Position {
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
            });
        }

        debug!("Found {} open positions", positions.len());
        Ok(positions)
    }

    /// Get open positions for a specific symbol
    pub async fn get_open_positions_by_symbol(&self, symbol: &str) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, (
            i32, String, String, String, String,
            i64, Decimal, Decimal,
            DateTime<Utc>, Option<DateTime<Utc>>, String, Option<Decimal>, Option<Decimal>, Option<String>
        )>(
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
        .await
        ?;

        let mut positions = Vec::new();
        for row in rows {
            let market_side = match row.4.as_str() {
                "UP" => Side::Up,
                "DOWN" => Side::Down,
                _ => continue,
            };

            let status = match row.10.as_str() {
                "OPEN" => PositionStatus::Open,
                "CLOSED" => PositionStatus::Closed,
                _ => continue,
            };

            positions.push(Position {
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
            });
        }

        debug!("Found {} open positions for {}", positions.len(), symbol);
        Ok(positions)
    }

    /// Get position summary statistics
    pub async fn get_summary(&self) -> Result<PositionSummary> {
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
        .await
        ?;

        Ok(PositionSummary {
            total_open: row.0,
            total_closed: row.1,
            total_pnl: row.2,
            avg_pnl: row.3,
            win_rate: row.4,
        })
    }

    /// Count open positions for a symbol
    pub async fn count_open_positions_by_symbol(&self, symbol: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM positions
            WHERE status = 'OPEN' AND symbol = $1
            "#,
        )
        .bind(symbol)
        .fetch_one(self.store.pool())
        .await
        ?;

        Ok(count)
    }

    /// Get total open position count
    pub async fn count_open_positions(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM positions
            WHERE status = 'OPEN'
            "#,
        )
        .fetch_one(self.store.pool())
        .await
        ?;

        Ok(count)
    }

    /// Get position by token ID
    pub async fn get_position_by_token(&self, token_id: &str) -> Result<Option<Position>> {
        let row = sqlx::query_as::<_, (
            i32, String, String, String, String,
            i64, Decimal, Decimal,
            DateTime<Utc>, Option<DateTime<Utc>>, String, Option<Decimal>, Option<Decimal>, Option<String>
        )>(
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
        .await
        ?;

        if let Some(row) = row {
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

            Ok(Some(Position {
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
            }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // Note: These tests require a running PostgreSQL database with migrations applied
    // Run with: cargo test --features test-db

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_position_lifecycle() {
        // This is a placeholder for integration tests
        // Actual tests would require database setup
    }
}
