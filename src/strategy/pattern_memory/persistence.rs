//! PostgreSQL persistence for PatternMemory samples.
//!
//! Supports:
//! - `ensure_table()`: CREATE TABLE IF NOT EXISTS
//! - `save_samples()`: INSERT new samples (dedup on timestamp via ON CONFLICT)
//! - `load_samples()`: SELECT all samples for a (symbol, pattern_len) pair
//!
//! On startup, all samples are loaded in a single query.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::{debug, info, warn};

use super::engine::PatternSample;

/// Ensure the pattern_memory_samples table exists.
pub async fn ensure_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pattern_memory_samples (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            pattern_len SMALLINT NOT NULL,
            pattern DOUBLE PRECISION[] NOT NULL,
            next_return DOUBLE PRECISION NOT NULL,
            sample_ts TIMESTAMPTZ NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE(symbol, pattern_len, sample_ts)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_samples_symbol_len ON pattern_memory_samples(symbol, pattern_len, sample_ts DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Save new samples to the database (individual inserts, skip duplicates via ON CONFLICT).
///
/// Returns the number of rows actually inserted.
pub async fn save_samples<const N: usize>(
    pool: &PgPool,
    symbol: &str,
    samples: &[PatternSample<N>],
) -> Result<u64, sqlx::Error> {
    if samples.is_empty() {
        return Ok(0);
    }

    let pattern_len = N as i16;
    let mut inserted = 0u64;

    for sample in samples {
        let pattern_vec: Vec<f64> = sample.pattern.to_vec();

        let result = sqlx::query(
            r#"
            INSERT INTO pattern_memory_samples (symbol, pattern_len, pattern, next_return, sample_ts)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (symbol, pattern_len, sample_ts) DO NOTHING
            "#,
        )
        .bind(symbol)
        .bind(pattern_len)
        .bind(&pattern_vec)
        .bind(sample.next_return)
        .bind(sample.timestamp)
        .execute(pool)
        .await;

        match result {
            Ok(r) => inserted += r.rows_affected(),
            Err(e) => {
                warn!(symbol, error = %e, "failed to save pattern_memory sample");
            }
        }
    }

    if inserted > 0 {
        debug!(symbol, inserted, "saved pattern_memory samples to DB");
    }

    Ok(inserted)
}

/// Load all samples for a given symbol and pattern length from the database.
///
/// Returns samples ordered by timestamp ascending (oldest first).
pub async fn load_samples<const N: usize>(
    pool: &PgPool,
    symbol: &str,
    max_samples: usize,
) -> Result<Vec<PatternSample<N>>, sqlx::Error> {
    let pattern_len = N as i16;

    let rows = sqlx::query_as::<_, (Vec<f64>, f64, DateTime<Utc>)>(
        r#"
        SELECT pattern, next_return, sample_ts
        FROM pattern_memory_samples
        WHERE symbol = $1 AND pattern_len = $2
        ORDER BY sample_ts DESC
        LIMIT $3
        "#,
    )
    .bind(symbol)
    .bind(pattern_len)
    .bind(max_samples as i64)
    .fetch_all(pool)
    .await?;

    let mut samples: Vec<PatternSample<N>> = Vec::with_capacity(rows.len());

    for (pattern_vec, next_return, ts) in rows {
        if pattern_vec.len() != N {
            warn!(
                symbol,
                expected = N,
                actual = pattern_vec.len(),
                "skipping pattern with wrong length"
            );
            continue;
        }

        let mut pattern = [0.0f64; N];
        pattern.copy_from_slice(&pattern_vec);

        samples.push(PatternSample {
            pattern,
            next_return,
            timestamp: ts,
        });
    }

    // Reverse to oldest-first order
    samples.reverse();

    info!(
        symbol,
        loaded = samples.len(),
        "loaded pattern_memory samples from DB"
    );

    Ok(samples)
}

/// Load samples from DB and populate a PatternMemory instance.
pub async fn load_into_memory<const N: usize>(
    pool: &PgPool,
    symbol: &str,
    memory: &mut super::engine::PatternMemory<N>,
    max_samples: usize,
) -> Result<usize, sqlx::Error> {
    let samples = load_samples::<N>(pool, symbol, max_samples).await?;
    let count = samples.len();

    for sample in samples {
        memory.push_sample(sample);
    }

    Ok(count)
}
