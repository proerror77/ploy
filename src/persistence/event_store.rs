//! Event Store for Event Sourcing
//!
//! Provides event sourcing infrastructure for audit trail and state replay.
//! Events are immutable records of state changes that can be replayed to
//! reconstruct system state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use sqlx::Row;
use tracing::debug;

/// Metadata for stored events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Correlation ID for tracking related events
    pub correlation_id: Option<String>,
    /// Causation ID (event that caused this event)
    pub causation_id: Option<String>,
    /// User or system that triggered the event
    pub triggered_by: Option<String>,
    /// Additional custom metadata
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for EventMetadata {
    fn default() -> Self {
        Self {
            correlation_id: None,
            causation_id: None,
            triggered_by: None,
            extra: serde_json::Map::new(),
        }
    }
}

impl EventMetadata {
    /// Create new metadata with correlation ID
    pub fn with_correlation(correlation_id: &str) -> Self {
        Self {
            correlation_id: Some(correlation_id.to_string()),
            ..Default::default()
        }
    }

    /// Add causation ID
    pub fn with_causation(mut self, causation_id: &str) -> Self {
        self.causation_id = Some(causation_id.to_string());
        self
    }

    /// Add triggered by
    pub fn with_triggered_by(mut self, triggered_by: &str) -> Self {
        self.triggered_by = Some(triggered_by.to_string());
        self
    }
}

/// A stored event in the event store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: i64,
    pub aggregate_id: String,
    pub aggregate_type: String,
    pub event_type: String,
    pub event_version: i32,
    pub payload: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// Event store for persisting domain events
pub struct EventStore {
    pool: PgPool,
}

impl EventStore {
    /// Create a new event store
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append an event to the store
    pub async fn append(
        &self,
        aggregate_id: &str,
        aggregate_type: &str,
        event_type: &str,
        event_version: i32,
        payload: serde_json::Value,
        metadata: Option<EventMetadata>,
    ) -> crate::error::Result<i64> {
        let metadata_json = metadata.map(|m| serde_json::to_value(m).ok()).flatten();

        let row = sqlx::query(
            r#"
            INSERT INTO strategy_events (
                aggregate_id, aggregate_type, event_type, event_version, payload, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
        )
        .bind(aggregate_id)
        .bind(aggregate_type)
        .bind(event_type)
        .bind(event_version)
        .bind(&payload)
        .bind(&metadata_json)
        .fetch_one(&self.pool)
        .await?;

        let id: i64 = row.get("id");

        debug!(
            "Appended event {} to {}/{} (type: {}, version: {})",
            id, aggregate_type, aggregate_id, event_type, event_version
        );

        Ok(id)
    }

    /// Get all events for an aggregate
    pub async fn get_events(
        &self,
        aggregate_id: &str,
        aggregate_type: &str,
    ) -> crate::error::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, aggregate_id, aggregate_type, event_type, event_version,
                   payload, metadata, created_at
            FROM strategy_events
            WHERE aggregate_id = $1 AND aggregate_type = $2
            ORDER BY event_version ASC, created_at ASC
            "#,
        )
        .bind(aggregate_id)
        .bind(aggregate_type)
        .fetch_all(&self.pool)
        .await?;

        let events: Vec<StoredEvent> = rows
            .iter()
            .map(|row| StoredEvent {
                id: row.get("id"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_type: row.get("aggregate_type"),
                event_type: row.get("event_type"),
                event_version: row.get("event_version"),
                payload: row.get("payload"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(events)
    }

    /// Get events since a specific event ID
    pub async fn get_events_since(
        &self,
        aggregate_id: &str,
        aggregate_type: &str,
        since_id: i64,
    ) -> crate::error::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, aggregate_id, aggregate_type, event_type, event_version,
                   payload, metadata, created_at
            FROM strategy_events
            WHERE aggregate_id = $1 AND aggregate_type = $2 AND id > $3
            ORDER BY event_version ASC, created_at ASC
            "#,
        )
        .bind(aggregate_id)
        .bind(aggregate_type)
        .bind(since_id)
        .fetch_all(&self.pool)
        .await?;

        let events: Vec<StoredEvent> = rows
            .iter()
            .map(|row| StoredEvent {
                id: row.get("id"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_type: row.get("aggregate_type"),
                event_type: row.get("event_type"),
                event_version: row.get("event_version"),
                payload: row.get("payload"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(events)
    }

    /// Get events by type across all aggregates
    pub async fn get_events_by_type(
        &self,
        event_type: &str,
        limit: i64,
    ) -> crate::error::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, aggregate_id, aggregate_type, event_type, event_version,
                   payload, metadata, created_at
            FROM strategy_events
            WHERE event_type = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(event_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let events: Vec<StoredEvent> = rows
            .iter()
            .map(|row| StoredEvent {
                id: row.get("id"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_type: row.get("aggregate_type"),
                event_type: row.get("event_type"),
                event_version: row.get("event_version"),
                payload: row.get("payload"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(events)
    }

    /// Get the latest event version for an aggregate
    pub async fn get_latest_version(
        &self,
        aggregate_id: &str,
        aggregate_type: &str,
    ) -> crate::error::Result<Option<i32>> {
        let row = sqlx::query(
            r#"
            SELECT MAX(event_version) as max_version
            FROM strategy_events
            WHERE aggregate_id = $1 AND aggregate_type = $2
            "#,
        )
        .bind(aggregate_id)
        .bind(aggregate_type)
        .fetch_one(&self.pool)
        .await?;

        let version: Option<i32> = row.get("max_version");
        Ok(version)
    }

    /// Get events within a time range
    pub async fn get_events_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: i64,
    ) -> crate::error::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, aggregate_id, aggregate_type, event_type, event_version,
                   payload, metadata, created_at
            FROM strategy_events
            WHERE created_at >= $1 AND created_at <= $2
            ORDER BY created_at ASC
            LIMIT $3
            "#,
        )
        .bind(start)
        .bind(end)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let events: Vec<StoredEvent> = rows
            .iter()
            .map(|row| StoredEvent {
                id: row.get("id"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_type: row.get("aggregate_type"),
                event_type: row.get("event_type"),
                event_version: row.get("event_version"),
                payload: row.get("payload"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(events)
    }

    /// Count events for an aggregate
    pub async fn count_events(
        &self,
        aggregate_id: &str,
        aggregate_type: &str,
    ) -> crate::error::Result<i64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM strategy_events
            WHERE aggregate_id = $1 AND aggregate_type = $2
            "#,
        )
        .bind(aggregate_id)
        .bind(aggregate_type)
        .fetch_one(&self.pool)
        .await?;

        let count: i64 = row.get("count");
        Ok(count)
    }

    /// Get recent events with correlation ID
    pub async fn get_correlated_events(
        &self,
        correlation_id: &str,
    ) -> crate::error::Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, aggregate_id, aggregate_type, event_type, event_version,
                   payload, metadata, created_at
            FROM strategy_events
            WHERE metadata->>'correlation_id' = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(correlation_id)
        .fetch_all(&self.pool)
        .await?;

        let events: Vec<StoredEvent> = rows
            .iter()
            .map(|row| StoredEvent {
                id: row.get("id"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_type: row.get("aggregate_type"),
                event_type: row.get("event_type"),
                event_version: row.get("event_version"),
                payload: row.get("payload"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(events)
    }
}

/// Trait for aggregates that can be rebuilt from events
pub trait EventSourced: Sized {
    /// Apply an event to mutate state
    fn apply(&mut self, event: &StoredEvent);

    /// Rebuild aggregate from events
    fn replay(events: &[StoredEvent]) -> Option<Self>
    where
        Self: Default,
    {
        if events.is_empty() {
            return None;
        }

        let mut aggregate = Self::default();
        for event in events {
            aggregate.apply(event);
        }
        Some(aggregate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_metadata_builder() {
        let metadata = EventMetadata::with_correlation("corr-123")
            .with_causation("cause-456")
            .with_triggered_by("system");

        assert_eq!(metadata.correlation_id, Some("corr-123".to_string()));
        assert_eq!(metadata.causation_id, Some("cause-456".to_string()));
        assert_eq!(metadata.triggered_by, Some("system".to_string()));
    }

    #[test]
    fn test_stored_event_serialization() {
        let event = StoredEvent {
            id: 1,
            aggregate_id: "cycle-123".to_string(),
            aggregate_type: "TradeCycle".to_string(),
            event_type: "CycleStarted".to_string(),
            event_version: 1,
            payload: serde_json::json!({"symbol": "BTC-USD"}),
            metadata: Some(serde_json::json!({"correlation_id": "abc"})),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let parsed: StoredEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.aggregate_id, "cycle-123");
        assert_eq!(parsed.event_type, "CycleStarted");
    }
}
