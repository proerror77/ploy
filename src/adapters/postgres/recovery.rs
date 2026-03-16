use super::PostgresStore;
use crate::domain::{Side, StrategyState};
use crate::error::Result;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::Row;
use tracing::info;

impl PostgresStore {
    /// Get all incomplete cycles (for crash recovery)
    /// Returns cycles that are in LEG1_PENDING, LEG1_FILLED, or LEG2_PENDING states
    pub async fn get_incomplete_cycles(&self) -> Result<Vec<IncompleteCycle>> {
        let rows = sqlx::query(
            r#"
            SELECT c.id, c.round_id, c.state, c.leg1_side, c.leg1_entry_price, c.leg1_shares,
                   c.leg1_filled_at, c.created_at,
                   r.slug, r.up_token_id, r.down_token_id, r.end_time
            FROM cycles c
            JOIN rounds r ON c.round_id = r.id
            WHERE c.state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
            ORDER BY c.created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let cycles = rows
            .into_iter()
            .map(|r| IncompleteCycle {
                cycle_id: r.get("id"),
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
                created_at: r.get("created_at"),
                round_slug: r.get("slug"),
                up_token_id: r.get("up_token_id"),
                down_token_id: r.get("down_token_id"),
                round_end_time: r.get("end_time"),
            })
            .collect();

        Ok(cycles)
    }

    /// Get orphaned orders (submitted but not filled/cancelled for too long)
    pub async fn get_orphaned_orders(&self, age_minutes: i32) -> Result<Vec<OrphanedOrder>> {
        let rows = sqlx::query(
            r#"
            SELECT o.id, o.client_order_id, o.exchange_order_id, o.token_id,
                   o.shares, o.limit_price, o.status, o.submitted_at, o.leg,
                   c.id as cycle_id, c.state as cycle_state
            FROM orders o
            LEFT JOIN cycles c ON o.cycle_id = c.id
            WHERE o.status IN ('Submitted', 'Pending', 'PartiallyFilled')
              AND o.submitted_at < NOW() - INTERVAL '1 minute' * $1
            ORDER BY o.submitted_at ASC
            "#,
        )
        .bind(age_minutes)
        .fetch_all(&self.pool)
        .await?;

        let orders = rows
            .into_iter()
            .map(|r| OrphanedOrder {
                order_id: r.get("id"),
                client_order_id: r.get("client_order_id"),
                exchange_order_id: r.get("exchange_order_id"),
                token_id: r.get("token_id"),
                shares: r.get::<i32, _>("shares") as u64,
                limit_price: r.get("limit_price"),
                status: r.get("status"),
                submitted_at: r.get("submitted_at"),
                leg: r.get::<i32, _>("leg") as u8,
                cycle_id: r.get("cycle_id"),
                cycle_state: r.get("cycle_state"),
            })
            .collect();

        Ok(orders)
    }

    /// Mark an order as cancelled (for orphan cleanup)
    pub async fn mark_order_cancelled(&self, client_order_id: &str, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE orders SET
                status = 'Cancelled',
                error = $1,
                cancelled_at = NOW(),
                updated_at = NOW()
            WHERE client_order_id = $2
            "#,
        )
        .bind(reason)
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check if trading was halted
    pub async fn is_trading_halted(&self, date: NaiveDate) -> Result<bool> {
        let row = sqlx::query("SELECT halted FROM daily_metrics WHERE date = $1")
            .bind(date)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| r.get::<bool, _>("halted")).unwrap_or(false))
    }

    /// Get recovery summary for startup logging
    pub async fn get_recovery_summary(&self) -> Result<RecoverySummary> {
        let incomplete_cycles = self.get_incomplete_cycles().await?;
        let orphaned_orders = self.get_orphaned_orders(5).await?;

        let persisted_state = self.get_strategy_state().await.ok();

        Ok(RecoverySummary {
            incomplete_cycle_count: incomplete_cycles.len(),
            orphaned_order_count: orphaned_orders.len(),
            last_state: persisted_state.as_ref().map(|s| s.current_state.clone()),
            last_cycle_id: persisted_state.and_then(|s| s.current_cycle_id),
            incomplete_cycles,
            orphaned_orders,
        })
    }
}

/// Incomplete cycle for crash recovery
#[derive(Debug, Clone)]
pub struct IncompleteCycle {
    pub cycle_id: i32,
    pub round_id: i32,
    pub state: StrategyState,
    pub leg1_side: Option<Side>,
    pub leg1_entry_price: Option<Decimal>,
    pub leg1_shares: Option<u64>,
    pub leg1_filled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub round_slug: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub round_end_time: DateTime<Utc>,
}

impl IncompleteCycle {
    /// Check if the round has already ended
    pub fn is_round_expired(&self) -> bool {
        Utc::now() > self.round_end_time
    }

    /// Get time remaining until round ends
    pub fn time_remaining(&self) -> chrono::Duration {
        self.round_end_time - Utc::now()
    }
}

/// Orphaned order for cleanup
#[derive(Debug, Clone)]
pub struct OrphanedOrder {
    pub order_id: i32,
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    pub token_id: String,
    pub shares: u64,
    pub limit_price: Option<Decimal>,
    pub status: String,
    pub submitted_at: Option<DateTime<Utc>>,
    pub leg: u8,
    pub cycle_id: Option<i32>,
    pub cycle_state: Option<String>,
}

impl OrphanedOrder {
    /// Check if this order can be cancelled on the exchange
    pub fn can_cancel_on_exchange(&self) -> bool {
        self.exchange_order_id.is_some() && self.status != "Cancelled" && self.status != "Filled"
    }
}

/// Recovery summary for startup
#[derive(Debug, Clone)]
pub struct RecoverySummary {
    pub incomplete_cycle_count: usize,
    pub orphaned_order_count: usize,
    pub last_state: Option<StrategyState>,
    pub last_cycle_id: Option<i32>,
    pub incomplete_cycles: Vec<IncompleteCycle>,
    pub orphaned_orders: Vec<OrphanedOrder>,
}

impl RecoverySummary {
    /// Check if recovery is needed
    pub fn needs_recovery(&self) -> bool {
        self.incomplete_cycle_count > 0 || self.orphaned_order_count > 0
    }

    /// Log recovery summary
    pub fn log_summary(&self) {
        if !self.needs_recovery() {
            info!("No crash recovery needed - clean startup");
            return;
        }

        info!(
            "Crash recovery summary: {} incomplete cycles, {} orphaned orders",
            self.incomplete_cycle_count, self.orphaned_order_count
        );

        for cycle in &self.incomplete_cycles {
            let expired = if cycle.is_round_expired() {
                " [EXPIRED]"
            } else {
                ""
            };
            info!(
                "  - Cycle {} in state {} (round: {}){}",
                cycle.cycle_id, cycle.state, cycle.round_slug, expired
            );
        }

        for order in &self.orphaned_orders {
            info!(
                "  - Order {} ({}) status={} token={}",
                order.client_order_id,
                if order.leg == 1 { "Leg1" } else { "Leg2" },
                order.status,
                &order.token_id[..8]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IncompleteCycle, OrphanedOrder, RecoverySummary};
    use crate::domain::StrategyState;
    use chrono::{Duration, Utc};

    #[test]
    fn incomplete_cycle_reports_expiration_and_remaining_time() {
        let cycle = IncompleteCycle {
            cycle_id: 1,
            round_id: 7,
            state: StrategyState::Leg1Pending,
            leg1_side: None,
            leg1_entry_price: None,
            leg1_shares: None,
            leg1_filled_at: None,
            created_at: Utc::now(),
            round_slug: "btc-5m".to_string(),
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
            round_end_time: Utc::now() + Duration::minutes(5),
        };

        assert!(!cycle.is_round_expired());
        assert!(cycle.time_remaining() > Duration::zero());
    }

    #[test]
    fn orphaned_order_cancel_gate_requires_exchange_id_and_active_status() {
        let order = OrphanedOrder {
            order_id: 9,
            client_order_id: "cid".to_string(),
            exchange_order_id: Some("exch".to_string()),
            token_id: "tok".to_string(),
            shares: 10,
            limit_price: None,
            status: "Submitted".to_string(),
            submitted_at: None,
            leg: 1,
            cycle_id: None,
            cycle_state: None,
        };

        assert!(order.can_cancel_on_exchange());
        assert!(
            !OrphanedOrder {
                status: "Cancelled".to_string(),
                ..order.clone()
            }
            .can_cancel_on_exchange()
        );
    }

    #[test]
    fn recovery_summary_flags_needed_recovery_when_any_counts_exist() {
        let summary = RecoverySummary {
            incomplete_cycle_count: 1,
            orphaned_order_count: 0,
            last_state: None,
            last_cycle_id: None,
            incomplete_cycles: Vec::new(),
            orphaned_orders: Vec::new(),
        };

        assert!(summary.needs_recovery());
        assert!(
            !RecoverySummary {
                incomplete_cycle_count: 0,
                orphaned_order_count: 0,
                ..summary
            }
            .needs_recovery()
        );
    }
}
