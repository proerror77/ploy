use crate::adapters::postgres::PostgresStore;
use crate::error::Result;
use sqlx::Row;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Nonce manager for atomic nonce allocation and tracking
///
/// This manager ensures that nonces are allocated sequentially and atomically,
/// preventing collisions and enabling crash recovery.
///
/// # How it works
/// 1. Nonces are stored in the database with atomic increment
/// 2. Each allocation is logged for debugging and recovery
/// 3. Nonces can be marked as used or released (on failure)
/// 4. On restart, the system continues from the last used nonce
///
/// # Example
/// ```rust,ignore
/// let nonce_mgr = NonceManager::new(store, wallet_address);
///
/// // Allocate nonce
/// let nonce = nonce_mgr.allocate().await?;
///
/// // Use nonce in order
/// let order = sign_order(..., nonce);
///
/// // Mark as used on success
/// nonce_mgr.mark_used(nonce, &order_id).await?;
///
/// // Or release on failure
/// nonce_mgr.release(nonce, "Order rejected").await?;
/// ```
pub struct NonceManager {
    store: PostgresStore,
    wallet_address: String,
    /// In-memory cache for fast allocation (synced with DB)
    cached_nonce: Arc<Mutex<Option<u64>>>,
}

impl NonceManager {
    /// Create a new nonce manager
    ///
    /// # Arguments
    /// * `store` - Database store for persistence
    /// * `wallet_address` - Wallet address to track nonces for
    pub fn new(store: PostgresStore, wallet_address: String) -> Self {
        Self {
            store,
            wallet_address,
            cached_nonce: Arc::new(Mutex::new(None)),
        }
    }

    /// Allocate the next nonce atomically
    ///
    /// This method is thread-safe and guarantees unique nonce allocation
    /// even under concurrent access.
    ///
    /// # Returns
    /// The allocated nonce value
    pub async fn allocate(&self) -> Result<u64> {
        // Call database function for atomic allocation
        let nonce: i64 = sqlx::query_scalar("SELECT get_next_nonce($1)")
            .bind(&self.wallet_address)
            .fetch_one(self.store.pool())
            .await?;

        let nonce = nonce as u64;

        // Update cache
        *self.cached_nonce.lock().await = Some(nonce);

        debug!(
            "Allocated nonce {} for wallet {}",
            nonce, self.wallet_address
        );
        Ok(nonce)
    }

    /// Mark nonce as successfully used in an order
    ///
    /// # Arguments
    /// * `nonce` - The nonce that was used
    /// * `order_id` - The exchange order ID
    pub async fn mark_used(&self, nonce: u64, order_id: &str) -> Result<()> {
        sqlx::query("SELECT mark_nonce_used($1, $2, $3)")
            .bind(&self.wallet_address)
            .bind(nonce as i64)
            .bind(order_id)
            .execute(self.store.pool())
            .await?;

        debug!("Marked nonce {} as used (order: {})", nonce, order_id);
        Ok(())
    }

    /// Release nonce when order fails
    ///
    /// This allows the nonce to be reused if the order was never submitted
    /// to the exchange.
    ///
    /// # Arguments
    /// * `nonce` - The nonce to release
    /// * `error_message` - Reason for release
    pub async fn release(&self, nonce: u64, error_message: &str) -> Result<()> {
        sqlx::query("SELECT release_nonce($1, $2, $3)")
            .bind(&self.wallet_address)
            .bind(nonce as i64)
            .bind(error_message)
            .execute(self.store.pool())
            .await?;

        warn!("Released nonce {} due to: {}", nonce, error_message);
        Ok(())
    }

    /// Get current nonce (for recovery)
    ///
    /// Returns the last allocated nonce for this wallet.
    pub async fn get_current(&self) -> Result<u64> {
        let nonce: Option<i64> = sqlx::query_scalar("SELECT get_current_nonce($1)")
            .bind(&self.wallet_address)
            .fetch_one(self.store.pool())
            .await?;

        Ok(nonce.unwrap_or(0) as u64)
    }

    /// Get nonce usage statistics
    ///
    /// Returns statistics about nonce allocation and usage for monitoring.
    pub async fn get_stats(&self) -> Result<NonceStats> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*) as total_allocations,
                COUNT(*) FILTER (WHERE status = 'used') as used_count,
                COUNT(*) FILTER (WHERE status = 'released') as released_count,
                COUNT(*) FILTER (WHERE status = 'allocated') as pending_count,
                MAX(nonce) as highest_nonce
            FROM nonce_usage
            WHERE wallet_address = $1
            "#,
        )
        .bind(&self.wallet_address)
        .fetch_one(self.store.pool())
        .await?;

        Ok(NonceStats {
            total_allocations: row.get::<i64, _>("total_allocations") as u64,
            used_count: row.get::<i64, _>("used_count") as u64,
            released_count: row.get::<i64, _>("released_count") as u64,
            pending_count: row.get::<i64, _>("pending_count") as u64,
            highest_nonce: row.get::<Option<i64>, _>("highest_nonce").map(|n| n as u64),
        })
    }

    /// Cleanup old nonce usage records
    ///
    /// Removes nonce usage records older than the specified number of days.
    /// This helps keep the database size manageable.
    ///
    /// # Arguments
    /// * `days_to_keep` - Number of days of history to retain
    ///
    /// # Returns
    /// Number of records deleted
    pub async fn cleanup_old_records(&self, days_to_keep: i32) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM nonce_usage
            WHERE wallet_address = $1
              AND allocated_at < NOW() - ($2 || ' days')::INTERVAL
              AND status IN ('used', 'released')
            "#,
        )
        .bind(&self.wallet_address)
        .bind(days_to_keep)
        .execute(self.store.pool())
        .await?;

        let deleted = result.rows_affected();

        if deleted > 0 {
            info!(
                "Cleaned up {} old nonce records for wallet {}",
                deleted, self.wallet_address
            );
        }

        Ok(deleted)
    }
}

/// Nonce usage statistics
#[derive(Debug, Clone)]
pub struct NonceStats {
    pub total_allocations: u64,
    pub used_count: u64,
    pub released_count: u64,
    pub pending_count: u64,
    pub highest_nonce: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nonce_stats_creation() {
        let stats = NonceStats {
            total_allocations: 100,
            used_count: 95,
            released_count: 3,
            pending_count: 2,
            highest_nonce: Some(100),
        };

        assert_eq!(stats.total_allocations, 100);
        assert_eq!(stats.used_count, 95);
        assert_eq!(stats.released_count, 3);
        assert_eq!(stats.pending_count, 2);
        assert_eq!(stats.highest_nonce, Some(100));
    }
}
