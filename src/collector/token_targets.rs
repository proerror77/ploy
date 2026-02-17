use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct CollectorTokenTarget {
    pub token_id: String,
    pub domain: String,
    pub target_date: Option<NaiveDate>,
    pub expires_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
}

impl CollectorTokenTarget {
    pub fn new(token_id: impl Into<String>, domain: impl Into<String>) -> Self {
        Self {
            token_id: token_id.into(),
            domain: domain.into(),
            target_date: None,
            expires_at: None,
            metadata: serde_json::json!({}),
        }
    }

    pub fn with_target_date(mut self, target_date: Option<NaiveDate>) -> Self {
        self.target_date = target_date;
        self
    }

    pub fn with_expires_at(mut self, expires_at: Option<DateTime<Utc>>) -> Self {
        self.expires_at = expires_at;
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Ensure the token-target table exists.
///
/// Note: `platform start` doesn't run sqlx migrations, so we keep this runtime DDL.
pub async fn ensure_collector_token_targets_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS collector_token_targets (
            token_id TEXT PRIMARY KEY,
            domain TEXT NOT NULL,
            target_date DATE,
            expires_at TIMESTAMPTZ,
            metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Indexes/triggers are best-effort: on some installs the DB is owned by `postgres`
    // while the app connects as a less-privileged role. We still want inserts to work.
    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_collector_token_targets_domain ON collector_token_targets(domain)",
    )
    .execute(pool)
    .await;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_collector_token_targets_target_date ON collector_token_targets(target_date) WHERE target_date IS NOT NULL",
    )
    .execute(pool)
    .await;

    let _ = sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_collector_token_targets_expires ON collector_token_targets(expires_at) WHERE expires_at IS NOT NULL",
    )
    .execute(pool)
    .await;

    // updated_at trigger (best-effort, may be missing on very old DBs)
    let _ = sqlx::query(
        r#"
        DO $$
        BEGIN
            IF to_regclass('public.collector_token_targets') IS NULL THEN
                RETURN;
            END IF;

            BEGIN
                DROP TRIGGER IF EXISTS update_collector_token_targets_updated_at ON collector_token_targets;
                CREATE TRIGGER update_collector_token_targets_updated_at
                BEFORE UPDATE ON collector_token_targets
                FOR EACH ROW
                EXECUTE FUNCTION update_updated_at_column();
            EXCEPTION WHEN undefined_function THEN
                NULL;
            END;
        END $$;
        "#,
    )
    .execute(pool)
    .await;

    Ok(())
}

pub async fn upsert_collector_token_targets(
    pool: &PgPool,
    targets: &[CollectorTokenTarget],
) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    for t in targets {
        sqlx::query(
            r#"
            INSERT INTO collector_token_targets
                (token_id, domain, target_date, expires_at, metadata)
            VALUES
                ($1, $2, $3, $4, $5)
            ON CONFLICT (token_id) DO UPDATE SET
                domain = EXCLUDED.domain,
                target_date = EXCLUDED.target_date,
                expires_at = EXCLUDED.expires_at,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&t.token_id)
        .bind(&t.domain)
        .bind(t.target_date)
        .bind(t.expires_at)
        .bind(sqlx::types::Json(&t.metadata))
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}
