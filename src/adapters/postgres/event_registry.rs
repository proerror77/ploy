use sqlx::Row;

use super::PostgresStore;
use crate::error::{PloyError, Result};
use crate::strategy::registry::{EventFilter, EventStatus, EventUpsertRequest, RegisteredEvent};

impl PostgresStore {
    /// Insert or update an event in the registry.
    /// Deduplicates on (title, source); uses COALESCE to preserve existing data.
    pub async fn upsert_event(&self, req: &EventUpsertRequest) -> Result<i32> {
        let status = req.status.as_deref().unwrap_or("discovered");
        let metadata = req
            .metadata
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));

        let row = sqlx::query(
            r#"
            INSERT INTO event_registry (
                title, source, event_id, slug, domain, strategy_hint,
                status, confidence, settlement_rule, end_time,
                market_slug, condition_id, token_ids, outcome_prices,
                metadata, last_scanned_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, NOW())
            ON CONFLICT (title, source) DO UPDATE SET
                event_id       = COALESCE(EXCLUDED.event_id, event_registry.event_id),
                slug           = COALESCE(EXCLUDED.slug, event_registry.slug),
                domain         = EXCLUDED.domain,
                strategy_hint  = COALESCE(EXCLUDED.strategy_hint, event_registry.strategy_hint),
                confidence     = COALESCE(EXCLUDED.confidence, event_registry.confidence),
                settlement_rule= COALESCE(EXCLUDED.settlement_rule, event_registry.settlement_rule),
                end_time       = COALESCE(EXCLUDED.end_time, event_registry.end_time),
                market_slug    = COALESCE(EXCLUDED.market_slug, event_registry.market_slug),
                condition_id   = COALESCE(EXCLUDED.condition_id, event_registry.condition_id),
                token_ids      = COALESCE(EXCLUDED.token_ids, event_registry.token_ids),
                outcome_prices = COALESCE(EXCLUDED.outcome_prices, event_registry.outcome_prices),
                metadata       = event_registry.metadata || EXCLUDED.metadata,
                last_scanned_at= NOW(),
                updated_at     = NOW()
            RETURNING id
            "#,
        )
        .bind(&req.title)
        .bind(&req.source)
        .bind(&req.event_id)
        .bind(&req.slug)
        .bind(&req.domain)
        .bind(&req.strategy_hint)
        .bind(status)
        .bind(req.confidence)
        .bind(&req.settlement_rule)
        .bind(req.end_time)
        .bind(&req.market_slug)
        .bind(&req.condition_id)
        .bind(&req.token_ids)
        .bind(&req.outcome_prices)
        .bind(&metadata)
        .fetch_one(self.pool())
        .await?;

        Ok(row.get("id"))
    }

    /// List events matching the given filter criteria.
    pub async fn list_events(&self, filter: &EventFilter) -> Result<Vec<RegisteredEvent>> {
        let limit = filter.limit.unwrap_or(100);

        let mut conditions = Vec::new();
        let mut idx = 1u32;

        if filter.status.is_some() {
            conditions.push(format!("status = ${idx}"));
            idx += 1;
        }
        if filter.domain.is_some() {
            conditions.push(format!("domain = ${idx}"));
            idx += 1;
        }
        if filter.strategy_hint.is_some() {
            conditions.push(format!("strategy_hint = ${idx}"));
            idx += 1;
        }
        if filter.source.is_some() {
            conditions.push(format!("source = ${idx}"));
            idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT id, event_id, title, slug, source, domain, strategy_hint,
                   status, confidence, settlement_rule, end_time,
                   market_slug, condition_id, token_ids, outcome_prices,
                   metadata, last_scanned_at, created_at, updated_at
            FROM event_registry
            {where_clause}
            ORDER BY updated_at DESC
            LIMIT ${idx}
            "#,
        );

        let mut query = sqlx::query(&sql);

        if let Some(ref s) = filter.status {
            query = query.bind(s);
        }
        if let Some(ref d) = filter.domain {
            query = query.bind(d);
        }
        if let Some(ref sh) = filter.strategy_hint {
            query = query.bind(sh);
        }
        if let Some(ref src) = filter.source {
            query = query.bind(src);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(self.pool()).await?;

        Ok(rows.iter().map(map_registered_event).collect())
    }

    /// Transition an event to a new status (validates the state machine).
    pub async fn update_event_status(&self, id: i32, new_status: EventStatus) -> Result<()> {
        let row = sqlx::query("SELECT status FROM event_registry WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;

        let row =
            row.ok_or_else(|| PloyError::Validation(format!("event_registry id={id} not found")))?;

        let current_str: String = row.get("status");
        let current = EventStatus::from_str(&current_str)
            .ok_or_else(|| PloyError::Validation(format!("unknown status in DB: {current_str}")))?;

        if !current.can_transition_to(new_status) {
            return Err(PloyError::InvalidStateTransition {
                from: current_str,
                to: new_status.to_string(),
            });
        }

        sqlx::query("UPDATE event_registry SET status = $1, updated_at = NOW() WHERE id = $2")
            .bind(new_status.as_str())
            .bind(id)
            .execute(self.pool())
            .await?;

        Ok(())
    }

    /// Get events with status=monitoring for a given strategy.
    pub async fn get_monitoring_events(&self, strategy_hint: &str) -> Result<Vec<RegisteredEvent>> {
        let filter = EventFilter {
            status: Some("monitoring".to_string()),
            strategy_hint: Some(strategy_hint.to_string()),
            ..Default::default()
        };
        self.list_events(&filter).await
    }

    /// Expire events whose end_time has passed (from non-terminal states).
    pub async fn expire_stale_events(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE event_registry
            SET status = 'expired', updated_at = NOW()
            WHERE end_time < NOW()
              AND status NOT IN ('settled', 'expired')
            "#,
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }
}

fn map_registered_event(row: &sqlx::postgres::PgRow) -> RegisteredEvent {
    RegisteredEvent {
        id: row.get("id"),
        event_id: row.get("event_id"),
        title: row.get("title"),
        slug: row.get("slug"),
        source: row.get("source"),
        domain: row.get("domain"),
        strategy_hint: row.get("strategy_hint"),
        status: row.get("status"),
        confidence: row.get("confidence"),
        settlement_rule: row.get("settlement_rule"),
        end_time: row.get("end_time"),
        market_slug: row.get("market_slug"),
        condition_id: row.get("condition_id"),
        token_ids: row.get("token_ids"),
        outcome_prices: row.get("outcome_prices"),
        metadata: row.get("metadata"),
        last_scanned_at: row.get("last_scanned_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
