//! Nonce Management System
//!
//! Provides persistent, atomic nonce generation for exchange API calls.
//! Prevents nonce collisions after system restarts.

use crate::adapters::postgres::PostgresStore;
use crate::error::{PloyError, Result};
use sqlx::Row;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Nonce manager for exchange API calls
///
/// Ensures monotonically increasing nonces that persist across restarts.
/// Uses database-backed atomic increments to prevent collisions.
///
/// # Example
/// ```rust,ignore
/// let nonce_manager = NonceManager::new(store);
/// nonce_manager.recover().await?;
///
/// let nonce = nonce_manager.get_next().await?;
/// // Use nonce in API call
/// ```
pub struct NonceManager {
    store: Arc<PostgresStore>,
    cache: Arc<RwLock<Option<i64>>>,
}

impl NonceManager {
    /// Create a new nonce manager
    ///
    /// # Arguments
    /// * `store` - Database store for persistence
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the next nonce atomically
    ///
    /// This method:
    /// 1. Atomically increments the nonce in the database
    /// 2. Updates the local cache
    /// 3. Returns the new nonce
    ///
    /// # Returns
    /// The next nonce value to use
    ///
    /// # Errors
    /// Returns error if database operation fails
    pub async fn get_next(&self) -> Result<i64> {
        // Get next nonce from database (atomic increment)
        let nonce = sqlx::query_scalar::<_, i64>("SELECT get_next_nonce()")
            .fetch_one(self.store.pool())
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to get next nonce: {}", e)))?;

        // Update cache
        *self.cache.write().await = Some(nonce);

        debug!("Generated nonce: {}", nonce);
        Ok(nonce)
    }

    /// Recover nonce state from database
    ///
    /// This should be called on startup to restore the nonce state
    /// after a system restart or crash.
    ///
    /// # Returns
    /// The current nonce value from the database
    pub async fn recover(&self) -> Result<i64> {
        let current =
            sqlx::query_scalar::<_, i64>("SELECT current_nonce FROM nonce_state WHERE id = 1")
                .fetch_one(self.store.pool())
                .await
                .map_err(|e| {
                    PloyError::Internal(format!("Failed to recover nonce state: {}", e))
                })?;

        *self.cache.write().await = Some(current);
        info!("Recovered nonce state: {}", current);

        Ok(current)
    }

    /// Get current nonce without incrementing
    ///
    /// Returns the cached nonce if available, otherwise fetches from database.
    /// This is useful for monitoring and debugging.
    ///
    /// # Returns
    /// The current nonce value
    pub async fn get_current(&self) -> Result<i64> {
        // Try cache first
        if let Some(nonce) = *self.cache.read().await {
            return Ok(nonce);
        }

        // Fetch from database
        let current =
            sqlx::query_scalar::<_, i64>("SELECT current_nonce FROM nonce_state WHERE id = 1")
                .fetch_one(self.store.pool())
                .await
                .map_err(|e| PloyError::Internal(format!("Failed to get current nonce: {}", e)))?;

        // Update cache
        *self.cache.write().await = Some(current);

        Ok(current)
    }

    /// Reset nonce to a specific value
    ///
    /// ⚠️ WARNING: This should only be used in emergency situations
    /// or during testing. Resetting the nonce can cause API errors
    /// if not done carefully.
    ///
    /// # Arguments
    /// * `new_nonce` - The new nonce value to set
    ///
    /// # Safety
    /// The new nonce must be greater than any previously used nonce
    /// to avoid collisions with the exchange.
    pub async fn reset(&self, new_nonce: i64) -> Result<()> {
        warn!("⚠️ Resetting nonce to: {}", new_nonce);

        sqlx::query(
            r#"
            UPDATE nonce_state
            SET current_nonce = $1,
                last_updated = NOW()
            WHERE id = 1
            "#,
        )
        .bind(new_nonce)
        .execute(self.store.pool())
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to reset nonce: {}", e)))?;

        // Update cache
        *self.cache.write().await = Some(new_nonce);

        info!("Nonce reset to: {}", new_nonce);
        Ok(())
    }

    /// Initialize nonce state if not exists
    ///
    /// This is called automatically during system startup.
    /// Uses current timestamp in milliseconds as the initial nonce.
    pub async fn initialize(&self) -> Result<()> {
        let initial_nonce = chrono::Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO nonce_state (id, current_nonce)
            VALUES (1, $1)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(initial_nonce)
        .execute(self.store.pool())
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to initialize nonce: {}", e)))?;

        info!("Nonce state initialized with: {}", initial_nonce);
        Ok(())
    }

    /// Get nonce statistics
    ///
    /// Returns information about nonce usage for monitoring.
    pub async fn get_stats(&self) -> Result<NonceStats> {
        let row = sqlx::query(
            r#"
            SELECT
                current_nonce,
                last_updated,
                EXTRACT(EPOCH FROM (NOW() - last_updated)) as seconds_since_update
            FROM nonce_state
            WHERE id = 1
            "#,
        )
        .fetch_one(self.store.pool())
        .await
        .map_err(|e| PloyError::Internal(format!("Failed to get nonce stats: {}", e)))?;

        let current_nonce: i64 = row.try_get("current_nonce")?;
        let last_updated: chrono::DateTime<chrono::Utc> = row.try_get("last_updated")?;
        let seconds_since_update: f64 = row.try_get("seconds_since_update")?;

        Ok(NonceStats {
            current_nonce,
            last_updated,
            seconds_since_update,
        })
    }
}

/// Nonce statistics for monitoring
#[derive(Debug, Clone)]
pub struct NonceStats {
    /// Current nonce value
    pub current_nonce: i64,
    /// Last time nonce was updated
    pub last_updated: chrono::DateTime<chrono::Utc>,
    /// Seconds since last update
    pub seconds_since_update: f64,
}

impl std::fmt::Display for NonceStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Nonce: {} (updated {:.1}s ago at {})",
            self.current_nonce,
            self.seconds_since_update,
            self.last_updated.format("%Y-%m-%d %H:%M:%S UTC")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a database connection
    // Run with: cargo test --features test-integration

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_nonce_increment() {
        // Setup test database
        let store = Arc::new(
            PostgresStore::new("postgresql://localhost/ploy_test", 5)
                .await
                .unwrap(),
        );
        let manager = NonceManager::new(store);

        // Initialize
        manager.initialize().await.unwrap();

        // Get first nonce
        let nonce1 = manager.get_next().await.unwrap();

        // Get second nonce
        let nonce2 = manager.get_next().await.unwrap();

        // Should increment
        assert_eq!(nonce2, nonce1 + 1);
    }

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_nonce_recovery() {
        let store = Arc::new(
            PostgresStore::new("postgresql://localhost/ploy_test", 5)
                .await
                .unwrap(),
        );

        // First manager
        let manager1 = NonceManager::new(store.clone());
        manager1.initialize().await.unwrap();
        let nonce1 = manager1.get_next().await.unwrap();

        // Simulate restart - create new manager
        let manager2 = NonceManager::new(store);
        let recovered = manager2.recover().await.unwrap();

        // Should recover the same nonce
        assert_eq!(recovered, nonce1);

        // Next nonce should be incremented
        let nonce2 = manager2.get_next().await.unwrap();
        assert_eq!(nonce2, nonce1 + 1);
    }

    #[tokio::test]
    #[ignore] // Requires database
    async fn test_concurrent_nonce_generation() {
        use tokio::task::JoinSet;

        let store = Arc::new(
            PostgresStore::new("postgresql://localhost/ploy_test", 10)
                .await
                .unwrap(),
        );
        let manager = Arc::new(NonceManager::new(store));
        manager.initialize().await.unwrap();

        // Generate 100 nonces concurrently
        let mut tasks = JoinSet::new();
        for _ in 0..100 {
            let manager = manager.clone();
            tasks.spawn(async move { manager.get_next().await.unwrap() });
        }

        // Collect all nonces
        let mut nonces = Vec::new();
        while let Some(result) = tasks.join_next().await {
            nonces.push(result.unwrap());
        }

        // Sort nonces
        nonces.sort();

        // Check for duplicates
        for i in 1..nonces.len() {
            assert_ne!(nonces[i], nonces[i - 1], "Found duplicate nonce!");
        }

        // Check for gaps (should be consecutive)
        for i in 1..nonces.len() {
            let diff = nonces[i] - nonces[i - 1];
            assert_eq!(diff, 1, "Found gap in nonce sequence!");
        }
    }
}
