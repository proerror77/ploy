//! Transaction Manager for atomic state updates
//!
//! Provides transactional guarantees for critical operations that span multiple tables.
//! Ensures data consistency during state transitions, cycle updates, and order execution.

use crate::domain::{Order, Side, StrategyState};
use crate::error::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use sqlx::{Postgres, Row, Transaction};
use tracing::{debug, error, info, instrument, warn};

/// Transaction scope identifier for tracking and debugging
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionScope {
    /// Cycle state update with order
    CycleUpdate,
    /// State machine transition
    StateTransition,
    /// Risk limit update
    RiskUpdate,
    /// Position reconciliation
    PositionReconcile,
    /// Recovery from checkpoint
    Recovery,
}

impl std::fmt::Display for TransactionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CycleUpdate => write!(f, "cycle_update"),
            Self::StateTransition => write!(f, "state_transition"),
            Self::RiskUpdate => write!(f, "risk_update"),
            Self::PositionReconcile => write!(f, "position_reconcile"),
            Self::Recovery => write!(f, "recovery"),
        }
    }
}

/// Dead Letter Queue entry for failed operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DLQEntry {
    pub operation_type: String,
    pub payload: serde_json::Value,
    pub error_message: String,
    pub error_code: Option<String>,
}

/// Transaction Manager for atomic database operations
#[derive(Clone)]
pub struct TransactionManager {
    pool: PgPool,
}

impl TransactionManager {
    /// Create a new transaction manager
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Begin a new transaction with scope tracking
    #[instrument(skip(self))]
    pub async fn begin(&self, scope: TransactionScope) -> Result<ManagedTransaction<'_>> {
        let tx = self.pool.begin().await?;
        debug!("Started transaction for scope: {}", scope);
        Ok(ManagedTransaction {
            tx: Some(tx),
            scope,
            committed: false,
        })
    }

    /// Execute a transactional operation with automatic rollback on error
    pub async fn execute<'a, F, T, Fut>(&'a self, scope: TransactionScope, op: F) -> Result<T>
    where
        F: FnOnce(ManagedTransaction<'a>) -> Fut,
        Fut: std::future::Future<Output = Result<(T, ManagedTransaction<'a>)>>,
    {
        let tx = self.begin(scope).await?;
        match op(tx).await {
            Ok((result, mut tx)) => {
                tx.commit().await?;
                Ok(result)
            }
            Err(e) => {
                error!("Transaction {} failed: {}", scope, e);
                Err(e)
            }
        }
    }

    // ==================== Atomic Operations ====================

    /// Atomically update cycle state and insert order
    #[instrument(skip(self))]
    pub async fn update_cycle_with_order(
        &self,
        cycle_id: i32,
        leg: i32,
        side: Side,
        price: Decimal,
        shares: u64,
        order: &Order,
    ) -> Result<()> {
        let mut tx = self.begin(TransactionScope::CycleUpdate).await?;

        // Update cycle
        if leg == 1 {
            sqlx::query(
                r#"
                UPDATE cycles SET
                    leg1_side = $2,
                    leg1_price = $3,
                    leg1_shares = $4,
                    state = 'LEG1_FILLED',
                    updated_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(cycle_id)
            .bind(side.as_str())
            .bind(price)
            .bind(shares as i64)
            .execute(tx.executor())
            .await?;
        } else {
            sqlx::query(
                r#"
                UPDATE cycles SET
                    leg2_price = $2,
                    leg2_shares = $3,
                    state = 'CYCLE_COMPLETE',
                    net_pnl = $4,
                    updated_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(cycle_id)
            .bind(price)
            .bind(shares as i64)
            .bind(Decimal::ZERO) // PnL calculated elsewhere
            .execute(tx.executor())
            .await?;
        }

        // Insert order
        sqlx::query(
            r#"
            INSERT INTO orders (
                cycle_id, order_id, token_id, side, status,
                requested_shares, filled_shares, limit_price,
                avg_fill_price, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), NOW())
            "#,
        )
        .bind(cycle_id)
        .bind(&order.exchange_order_id)
        .bind(&order.token_id)
        .bind(order.market_side.as_str())
        .bind(format!("{:?}", order.status))
        .bind(order.shares as i64)
        .bind(order.filled_shares as i64)
        .bind(order.limit_price)
        .bind(order.avg_fill_price)
        .execute(tx.executor())
        .await?;

        tx.commit().await?;
        info!(
            "Atomically updated cycle {} leg {} with order {:?}",
            cycle_id, leg, order.exchange_order_id
        );
        Ok(())
    }

    /// Atomically transition state and record event
    #[instrument(skip(self))]
    pub async fn transition_state(
        &self,
        from_state: StrategyState,
        to_state: StrategyState,
        component: &str,
        reason: &str,
    ) -> Result<()> {
        let mut tx = self.begin(TransactionScope::StateTransition).await?;

        // Record the state transition event
        sqlx::query(
            r#"
            INSERT INTO system_events (event_type, component, severity, message, metadata)
            VALUES ('state_transition', $1, 'info', $2, $3)
            "#,
        )
        .bind(component)
        .bind(format!("{} -> {}: {}", from_state, to_state, reason))
        .bind(serde_json::json!({
            "from_state": from_state.to_string(),
            "to_state": to_state.to_string(),
            "reason": reason
        }))
        .execute(tx.executor())
        .await?;

        tx.commit().await?;
        debug!(
            "Recorded state transition: {} -> {} ({})",
            from_state, to_state, reason
        );
        Ok(())
    }

    /// Record heartbeat for a component
    #[instrument(skip(self))]
    pub async fn record_heartbeat(
        &self,
        component: &str,
        status: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO component_heartbeats (component_name, status, metadata, last_heartbeat)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (component_name) DO UPDATE SET
                status = EXCLUDED.status,
                metadata = EXCLUDED.metadata,
                last_heartbeat = NOW()
            "#,
        )
        .bind(component)
        .bind(status)
        .bind(metadata)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get components that haven't sent heartbeat within timeout
    pub async fn get_stale_components(&self, timeout_secs: i64) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT component_name
            FROM component_heartbeats
            WHERE last_heartbeat < NOW() - INTERVAL '1 second' * $1
            AND status != 'stopped'
            "#,
        )
        .bind(timeout_secs)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("component_name"))
            .collect())
    }

    /// Add entry to dead letter queue
    #[instrument(skip(self, entry))]
    pub async fn add_to_dlq(&self, entry: DLQEntry) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO dead_letter_queue (operation_type, payload, error_message, error_code)
            VALUES ($1, $2, $3, $4)
            RETURNING id
            "#,
        )
        .bind(&entry.operation_type)
        .bind(&entry.payload)
        .bind(&entry.error_message)
        .bind(&entry.error_code)
        .fetch_one(&self.pool)
        .await?;

        let id: i64 = row.get("id");
        warn!(
            "Added operation {} to DLQ with id {}: {}",
            entry.operation_type, id, entry.error_message
        );
        Ok(id)
    }

    /// Get pending DLQ entries for retry
    pub async fn get_pending_dlq(&self, limit: i64) -> Result<Vec<(i64, DLQEntry)>> {
        let rows = sqlx::query(
            r#"
            SELECT id, operation_type, payload, error_message, error_code
            FROM dead_letter_queue
            WHERE status = 'pending' AND retry_count < max_retries
            ORDER BY created_at ASC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<i64, _>("id"),
                    DLQEntry {
                        operation_type: r.get("operation_type"),
                        payload: r.get("payload"),
                        error_message: r.get("error_message"),
                        error_code: r.get("error_code"),
                    },
                )
            })
            .collect())
    }

    /// Mark DLQ entry as resolved
    pub async fn resolve_dlq(&self, id: i64, resolved_by: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE dead_letter_queue SET
                status = 'resolved',
                resolved_at = NOW(),
                resolved_by = $2
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(resolved_by)
        .execute(&self.pool)
        .await?;

        info!("Resolved DLQ entry {} by {}", id, resolved_by);
        Ok(())
    }

    /// Mark a DLQ entry as permanently failed in a single atomic operation
    pub async fn mark_dlq_permanent_failure(&self, id: i64, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE dead_letter_queue SET
                status = 'failed',
                retry_count = max_retries,
                last_retry_at = NOW(),
                resolved_by = $2
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(reason)
        .execute(&self.pool)
        .await?;

        info!("DLQ entry {} marked as permanently failed: {}", id, reason);
        Ok(())
    }

    /// Increment retry count for DLQ entry
    pub async fn increment_dlq_retry(&self, id: i64) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE dead_letter_queue SET
                retry_count = retry_count + 1,
                last_retry_at = NOW(),
                status = CASE
                    WHEN retry_count + 1 >= max_retries THEN 'failed'
                    ELSE 'pending'
                END
            WHERE id = $1
            "#,
        )
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Save state snapshot for recovery
    #[instrument(skip(self, state_data))]
    pub async fn save_snapshot(
        &self,
        snapshot_type: &str,
        component: &str,
        state_data: serde_json::Value,
        version: i32,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO state_snapshots (snapshot_type, component, state_data, version)
            VALUES ($1, $2, $3, $4)
            RETURNING id
            "#,
        )
        .bind(snapshot_type)
        .bind(component)
        .bind(state_data)
        .bind(version)
        .fetch_one(&self.pool)
        .await?;

        let id: i64 = row.get("id");
        debug!("Saved snapshot {} for {}/{}", id, snapshot_type, component);
        Ok(id)
    }

    /// Get latest valid snapshot for a component
    pub async fn get_latest_snapshot(
        &self,
        snapshot_type: &str,
        component: &str,
    ) -> Result<Option<(i64, serde_json::Value, i32)>> {
        let row = sqlx::query(
            r#"
            SELECT id, state_data, version
            FROM state_snapshots
            WHERE snapshot_type = $1 AND component = $2 AND is_valid = true
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(snapshot_type)
        .bind(component)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            (
                r.get::<i64, _>("id"),
                r.get::<serde_json::Value, _>("state_data"),
                r.get::<i32, _>("version"),
            )
        }))
    }

    /// Record system event
    #[instrument(skip(self, metadata))]
    pub async fn record_event(
        &self,
        event_type: &str,
        component: &str,
        severity: &str,
        message: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO system_events (event_type, component, severity, message, metadata)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(event_type)
        .bind(component)
        .bind(severity)
        .bind(message)
        .bind(metadata)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get recent events by severity
    pub async fn get_recent_events(
        &self,
        severity: Option<&str>,
        limit: i64,
    ) -> Result<Vec<(String, String, String, String, DateTime<Utc>)>> {
        let rows = if let Some(sev) = severity {
            sqlx::query(
                r#"
                SELECT event_type, component, severity, message, created_at
                FROM system_events
                WHERE severity = $1
                ORDER BY created_at DESC
                LIMIT $2
                "#,
            )
            .bind(sev)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT event_type, component, severity, message, created_at
                FROM system_events
                ORDER BY created_at DESC
                LIMIT $1
                "#,
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("event_type"),
                    r.get::<String, _>("component"),
                    r.get::<String, _>("severity"),
                    r.get::<String, _>("message"),
                    r.get::<DateTime<Utc>, _>("created_at"),
                )
            })
            .collect())
    }
}

/// A managed transaction with automatic rollback on drop
pub struct ManagedTransaction<'a> {
    tx: Option<Transaction<'a, Postgres>>,
    scope: TransactionScope,
    committed: bool,
}

impl<'a> ManagedTransaction<'a> {
    /// Get mutable reference to the underlying transaction for executing queries
    /// Use as: `.execute(tx.executor()).await`
    pub fn executor(&mut self) -> &mut sqlx::PgConnection {
        let tx = self.tx.as_mut().expect("Transaction already consumed");
        // Dereference the Transaction to get the underlying connection
        &mut **tx
    }

    /// Commit the transaction
    pub async fn commit(&mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit().await?;
            self.committed = true;
            debug!("Committed transaction for scope: {}", self.scope);
        }
        Ok(())
    }

    /// Rollback the transaction explicitly
    pub async fn rollback(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
            warn!("Rolled back transaction for scope: {}", self.scope);
        }
        Ok(())
    }
}

impl<'a> Drop for ManagedTransaction<'a> {
    fn drop(&mut self) {
        if self.tx.is_some() && !self.committed {
            // Transaction will be rolled back automatically by sqlx
            warn!(
                "Transaction for scope {} was dropped without commit - rolling back",
                self.scope
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_scope_display() {
        assert_eq!(TransactionScope::CycleUpdate.to_string(), "cycle_update");
        assert_eq!(
            TransactionScope::StateTransition.to_string(),
            "state_transition"
        );
    }

    #[test]
    fn test_dlq_entry_serialization() {
        let entry = DLQEntry {
            operation_type: "order_submit".to_string(),
            payload: serde_json::json!({"order_id": "123"}),
            error_message: "Timeout".to_string(),
            error_code: Some("E001".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: DLQEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.operation_type, "order_submit");
    }
}
