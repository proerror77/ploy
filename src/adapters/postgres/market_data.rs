use super::PostgresStore;
use crate::domain::{Round, Side, Tick};
use crate::error::Result;
use sqlx::postgres::PgRow;
use sqlx::Row;
use tracing::{debug, instrument};

impl PostgresStore {
    // ==================== Rounds ====================

    /// Insert or update a round
    #[instrument(skip(self))]
    pub async fn upsert_round(&self, round: &Round) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO rounds (slug, up_token_id, down_token_id, start_time, end_time, outcome)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (slug) DO UPDATE SET
                up_token_id = EXCLUDED.up_token_id,
                down_token_id = EXCLUDED.down_token_id,
                start_time = EXCLUDED.start_time,
                end_time = EXCLUDED.end_time,
                outcome = EXCLUDED.outcome
            RETURNING id
            "#,
        )
        .bind(&round.slug)
        .bind(&round.up_token_id)
        .bind(&round.down_token_id)
        .bind(round.start_time)
        .bind(round.end_time)
        .bind(round.outcome.map(|side| side.as_str()))
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Get a round by slug
    pub async fn get_round_by_slug(&self, slug: &str) -> Result<Option<Round>> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, up_token_id, down_token_id, start_time, end_time, outcome
            FROM rounds WHERE slug = $1
            "#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.as_ref().map(round_from_row))
    }

    /// Get active round (current time between start and end)
    pub async fn get_active_round(&self) -> Result<Option<Round>> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, up_token_id, down_token_id, start_time, end_time, outcome
            FROM rounds
            WHERE start_time <= NOW() AND end_time > NOW()
            ORDER BY start_time DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.as_ref().map(round_from_row))
    }

    // ==================== Ticks ====================

    /// Insert a tick
    pub async fn insert_tick(&self, tick: &Tick) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO ticks (round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
            "#,
        )
        .bind(tick.round_id)
        .bind(tick.timestamp)
        .bind(tick.side.as_str())
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.bid_size)
        .bind(tick.ask_size)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Batch insert ticks
    pub async fn insert_ticks(&self, ticks: &[Tick]) -> Result<()> {
        if ticks.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;

        for tick in ticks {
            sqlx::query(
                r#"
                INSERT INTO ticks (round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(tick.round_id)
            .bind(tick.timestamp)
            .bind(tick.side.as_str())
            .bind(tick.best_bid)
            .bind(tick.best_ask)
            .bind(tick.bid_size)
            .bind(tick.ask_size)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        debug!("Inserted {} ticks", ticks.len());
        Ok(())
    }

    /// Get historical ticks for a round
    pub async fn get_ticks_for_round(&self, round_id: i32) -> Result<Vec<Tick>> {
        let rows = sqlx::query(
            r#"
            SELECT id, round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size
            FROM ticks
            WHERE round_id = $1
            ORDER BY timestamp ASC
            "#,
        )
        .bind(round_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(tick_from_row).collect())
    }

    /// Get all rounds with tick data
    pub async fn get_rounds_with_ticks(&self) -> Result<Vec<Round>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT r.id, r.slug, r.up_token_id, r.down_token_id,
                   r.start_time, r.end_time, r.outcome
            FROM rounds r
            INNER JOIN ticks t ON t.round_id = r.id
            ORDER BY r.start_time DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(round_from_row).collect())
    }

    /// Get tick count for a round
    pub async fn get_tick_count(&self, round_id: i32) -> Result<i64> {
        let row = sqlx::query(r#"SELECT COUNT(*) as count FROM ticks WHERE round_id = $1"#)
            .bind(round_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }
}

fn round_from_row(row: &PgRow) -> Round {
    Round {
        id: Some(row.get("id")),
        slug: row.get("slug"),
        up_token_id: row.get("up_token_id"),
        down_token_id: row.get("down_token_id"),
        start_time: row.get("start_time"),
        end_time: row.get("end_time"),
        outcome: row
            .get::<Option<String>, _>("outcome")
            .map(|side| parse_side_or_default(&side)),
    }
}

fn tick_from_row(row: &PgRow) -> Tick {
    Tick {
        id: Some(row.get("id")),
        round_id: row.get("round_id"),
        timestamp: row.get("timestamp"),
        side: parse_side_or_default(&row.get::<String, _>("side")),
        best_bid: row.get("best_bid"),
        best_ask: row.get("best_ask"),
        bid_size: row.get("bid_size"),
        ask_size: row.get("ask_size"),
    }
}

fn parse_side_or_default(value: &str) -> Side {
    match value.to_uppercase().as_str() {
        "UP" => Side::Up,
        "DOWN" => Side::Down,
        _ => Side::Up,
    }
}
