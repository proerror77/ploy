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
use crate::strategy::trading_costs::{OrderType, TradingCostCalculator};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use std::sync::Arc;
use tracing::{info, warn};

mod queries;

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

        // Use explicit transaction with SELECT ... FOR UPDATE to prevent
        // stale avg_entry_price reads under concurrent inserts.
        let mut tx = self.store.pool().begin().await?;

        // Try to lock the existing position row
        let existing: Option<(i32, i64, Decimal, Decimal)> = sqlx::query_as(
            r#"
            SELECT id, shares, avg_entry_price, amount_usd
            FROM positions
            WHERE event_id = $1 AND token_id = $2
            FOR UPDATE
            "#,
        )
        .bind(event_id)
        .bind(token_id)
        .fetch_optional(&mut *tx)
        .await?;

        let position_id: i32 = if let Some((id, old_shares, old_avg, old_amount)) = existing {
            // Update with locked (consistent) values
            let new_shares = old_shares + shares;
            let new_avg = if new_shares > 0 {
                (old_avg * Decimal::from(old_shares) + entry_price * Decimal::from(shares))
                    / Decimal::from(new_shares)
            } else {
                entry_price
            };
            let new_amount = old_amount + amount_usd;

            sqlx::query_scalar(
                r#"
                UPDATE positions
                SET shares = $1, avg_entry_price = $2, amount_usd = $3
                WHERE id = $4
                RETURNING id
                "#,
            )
            .bind(new_shares)
            .bind(new_avg)
            .bind(new_amount)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?
        } else {
            // Insert fresh position
            sqlx::query_scalar(
                r#"
                INSERT INTO positions (
                    event_id, symbol, token_id, market_side,
                    shares, avg_entry_price, amount_usd, strategy_id
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
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
            .fetch_one(&mut *tx)
            .await?
        };

        tx.commit().await?;

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
        self.fetch_position(position_id).await
    }

    /// Get all open positions
    pub async fn get_open_positions(&self) -> Result<Vec<Position>> {
        self.fetch_open_positions().await
    }

    /// Get open positions for a specific symbol
    pub async fn get_open_positions_by_symbol(&self, symbol: &str) -> Result<Vec<Position>> {
        self.fetch_open_positions_by_symbol(symbol).await
    }

    /// Get position summary statistics
    pub async fn get_summary(&self) -> Result<PositionSummary> {
        self.fetch_summary().await
    }

    /// Count open positions for a symbol
    pub async fn count_open_positions_by_symbol(&self, symbol: &str) -> Result<i64> {
        self.count_open_positions_for_symbol(symbol).await
    }

    /// Get total open position count
    pub async fn count_open_positions(&self) -> Result<i64> {
        self.count_all_open_positions().await
    }

    /// Get position by token ID
    pub async fn get_position_by_token(&self, token_id: &str) -> Result<Option<Position>> {
        self.fetch_open_position_by_token(token_id).await
    }
}

#[cfg(test)]
mod tests {
    // Note: These tests require a running PostgreSQL database with migrations applied
    // Run with: cargo test --features test-db

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_position_lifecycle() {
        // This is a placeholder for integration tests
        // Actual tests would require database setup
    }
}
