use crate::adapters::postgres::PostgresStore;
use crate::domain::order::OrderRequest;
use crate::error::{PloyError, Result};
use chrono::{Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tracing::{debug, info, warn};

/// Idempotency manager to prevent duplicate order submissions
///
/// This manager ensures that retrying an order submission (due to network timeouts,
/// etc.) will not result in duplicate orders being placed on the exchange.
///
/// # How it works
/// 1. Generate a unique idempotency key for each order request
/// 2. Before submission, check if this key was already processed
/// 3. If duplicate, return the cached result
/// 4. If new, proceed with submission and cache the result
///
/// # Example
/// ```rust,ignore
/// let idempotency = IdempotencyManager::new(store);
/// let key = IdempotencyManager::generate_key(&request);
///
/// match idempotency.check_or_create(&key, &request).await? {
///     IdempotencyResult::Duplicate { order_id, .. } => {
///         // Return cached result
///     }
///     IdempotencyResult::New => {
///         // Proceed with order submission
///     }
/// }
/// ```
pub struct IdempotencyManager {
    store: PostgresStore,
    account_id: String,
    ttl_seconds: i64,
}

impl IdempotencyManager {
    /// Create a new idempotency manager
    ///
    /// # Arguments
    /// * `store` - Database store for persistence
    pub fn new(store: PostgresStore) -> Self {
        Self::new_with_account(store, "default")
    }

    /// Create a new idempotency manager scoped to a specific DB account.
    pub fn new_with_account(store: PostgresStore, account_id: impl Into<String>) -> Self {
        Self {
            store,
            account_id: account_id.into(),
            ttl_seconds: 3600, // 1 hour TTL
        }
    }

    /// Generate an idempotency key for an order request.
    ///
    /// Uses an explicit idempotency key when provided, otherwise falls back
    /// to client_order_id. This allows legitimate repeated orders while still
    /// protecting retries of the same client order.
    pub fn generate_key(request: &OrderRequest) -> String {
        let candidate = request
            .idempotency_key
            .as_deref()
            .unwrap_or(&request.client_order_id)
            .trim();

        if candidate.is_empty() {
            Self::hash_request(request)
        } else {
            candidate.to_string()
        }
    }

    /// Hash the order request for duplicate detection
    ///
    /// This creates a deterministic hash of the order parameters to detect
    /// if the same order is being submitted multiple times.
    fn hash_request(request: &OrderRequest) -> String {
        let mut hasher = Sha256::new();
        hasher.update(request.token_id.as_bytes());
        hasher.update(&request.shares.to_le_bytes());
        hasher.update(request.limit_price.to_string().as_bytes());
        hasher.update(request.market_side.to_string().as_bytes());
        hasher.update(request.order_side.to_string().as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Check if request was already processed, or create new idempotency record
    ///
    /// # Arguments
    /// * `key` - Idempotency key
    /// * `request` - Order request to check
    ///
    /// # Returns
    /// * `IdempotencyResult::New` - This is a new request, proceed with submission
    /// * `IdempotencyResult::Duplicate` - This request was already processed
    pub async fn check_or_create(
        &self,
        key: &str,
        request: &OrderRequest,
    ) -> Result<IdempotencyResult> {
        let hash = Self::hash_request(request);
        let expires_at = Utc::now() + Duration::seconds(self.ttl_seconds);

        // Try to insert idempotency record atomically
        let result = sqlx::query(
            r#"
            INSERT INTO order_idempotency
            (account_id, idempotency_key, request_hash, status, expires_at)
            VALUES ($1, $2, $3, 'pending', $4)
            ON CONFLICT (account_id, idempotency_key) DO NOTHING
            RETURNING idempotency_key
            "#,
        )
        .bind(&self.account_id)
        .bind(key)
        .bind(&hash)
        .bind(expires_at)
        .fetch_optional(self.store.pool())
        .await?;

        if result.is_some() {
            // Successfully inserted - this is a new request
            debug!("New idempotency key: {}", key);
            Ok(IdempotencyResult::New)
        } else {
            // Key already exists - fetch the existing result
            let existing = self.fetch_record(key).await?;

            warn!(
                "Duplicate order detected with idempotency key: {} (status: {})",
                key, existing.status
            );

            Ok(IdempotencyResult::Duplicate {
                order_id: existing.order_id,
                status: existing.status,
                response_data: existing.response_data,
                error_message: existing.error_message,
            })
        }
    }

    /// Fetch the current idempotency record for a key.
    pub async fn fetch_record(&self, key: &str) -> Result<IdempotencyRecord> {
        let row = sqlx::query(
            r#"
            SELECT order_id, status, response_data, error_message
            FROM order_idempotency
            WHERE account_id = $1 AND idempotency_key = $2
            "#,
        )
        .bind(&self.account_id)
        .bind(key)
        .fetch_one(self.store.pool())
        .await?;

        Ok(IdempotencyRecord {
            order_id: row.try_get("order_id").ok(),
            status: row.get("status"),
            response_data: row.try_get("response_data").ok(),
            error_message: row.try_get("error_message").ok(),
        })
    }

    /// Mark idempotency key as completed with successful result
    ///
    /// # Arguments
    /// * `key` - Idempotency key
    /// * `order_id` - Exchange order ID
    /// * `response` - Serializable response data
    pub async fn mark_completed<T: Serialize>(
        &self,
        key: &str,
        order_id: &str,
        response: &T,
    ) -> Result<()> {
        let response_json = serde_json::to_value(response)
            .map_err(|e| PloyError::Internal(format!("Failed to serialize response: {}", e)))?;

        sqlx::query(
            r#"
            UPDATE order_idempotency
            SET order_id = $2,
                status = 'completed',
                response_data = $3
            WHERE account_id = $1 AND idempotency_key = $4
            "#,
        )
        .bind(&self.account_id)
        .bind(order_id)
        .bind(&response_json)
        .bind(key)
        .execute(self.store.pool())
        .await?;

        debug!(
            "Marked idempotency key {} as completed (order: {})",
            key, order_id
        );
        Ok(())
    }

    /// Mark idempotency key as failed
    ///
    /// # Arguments
    /// * `key` - Idempotency key
    /// * `error_message` - Error description
    pub async fn mark_failed(&self, key: &str, error_message: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE order_idempotency
            SET status = 'failed',
                error_message = $2
            WHERE account_id = $1 AND idempotency_key = $3
            "#,
        )
        .bind(&self.account_id)
        .bind(error_message)
        .bind(key)
        .execute(self.store.pool())
        .await?;

        warn!(
            "Marked idempotency key {} as failed: {}",
            key, error_message
        );
        Ok(())
    }

    /// Cleanup expired idempotency keys
    ///
    /// This should be called periodically (e.g., every hour) to remove old records.
    ///
    /// # Returns
    /// Number of records deleted
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let result = sqlx::query("SELECT cleanup_expired_idempotency_keys()")
            .fetch_one(self.store.pool())
            .await?;

        let deleted: i32 = result.try_get(0).unwrap_or(0);

        if deleted > 0 {
            info!("Cleaned up {} expired idempotency keys", deleted);
        }

        Ok(deleted as u64)
    }
}

/// Result of idempotency check
#[derive(Debug)]
pub enum IdempotencyResult {
    /// This is a new request - proceed with submission
    New,

    /// This request was already processed
    Duplicate {
        /// Exchange order ID (if submission succeeded)
        order_id: Option<String>,
        /// Status of the previous attempt
        status: String,
        /// Cached response data (if available)
        response_data: Option<serde_json::Value>,
        /// Error message (if previous attempt failed)
        error_message: Option<String>,
    },
}

/// Current idempotency record state.
#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    pub order_id: Option<String>,
    pub status: String,
    pub response_data: Option<serde_json::Value>,
    pub error_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::market::Side;
    use crate::domain::order::{OrderSide, OrderType, TimeInForce};
    use rust_decimal_macros::dec;

    fn test_request(client_order_id: &str, idempotency_key: Option<&str>) -> OrderRequest {
        OrderRequest {
            client_order_id: client_order_id.to_string(),
            idempotency_key: idempotency_key.map(|key| key.to_string()),
            token_id: "test_token".to_string(),
            market_side: Side::Up,
            order_side: OrderSide::Buy,
            shares: 100,
            limit_price: dec!(0.50),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
        }
    }

    #[test]
    fn test_generate_key() {
        let request = test_request("client-123", None);
        let key = IdempotencyManager::generate_key(&request);
        assert_eq!(key, "client-123");
    }

    #[test]
    fn test_generate_key_override() {
        let request = test_request("client-123", Some("idem-override"));
        let key = IdempotencyManager::generate_key(&request);
        assert_eq!(key, "idem-override");
    }

    #[test]
    fn test_hash_request() {
        let request1 = test_request("client-1", None);
        let request2 = test_request("client-2", None);

        let hash1 = IdempotencyManager::hash_request(&request1);
        let hash2 = IdempotencyManager::hash_request(&request2);

        // Same request should produce same hash
        assert_eq!(hash1, hash2);

        // Different request should produce different hash
        let mut request3 = test_request("client-3", None);
        request3.shares = 200;
        let hash3 = IdempotencyManager::hash_request(&request3);
        assert_ne!(hash1, hash3);
    }
}
