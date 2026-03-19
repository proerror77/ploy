use super::{parse_optional_bound, parse_symbol_filter, resolve_database_url, validate_window};
use anyhow::{Context, Result};

use crate::adapters::PostgresStore;

/// Backfill PM replay tables from sync_records:
/// - clob_quote_ticks
/// - clob_orderbook_snapshots (synthetic depth from prices)
/// - pm_market_metadata
pub(crate) async fn backfill_pm_replay_tables(
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    synthetic_depth: u64,
    database_url: Option<String>,
) -> Result<()> {
    let from_dt = parse_optional_bound(from.as_deref(), "--from")?;
    let to_dt = parse_optional_bound(to.as_deref(), "--to")?;
    validate_window(&from_dt, &to_dt)?;

    let (symbol_list, symbols_param) = parse_symbol_filter(symbols);

    let db_url = resolve_database_url(database_url);
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    crate::persistence::ensure_clob_quote_ticks_table(pool)
        .await
        .context("Failed to ensure clob_quote_ticks table")?;
    crate::persistence::ensure_clob_orderbook_snapshots_table(pool)
        .await
        .context("Failed to ensure clob_orderbook_snapshots table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_market_metadata (
            market_slug TEXT PRIMARY KEY,
            price_to_beat NUMERIC(20,8) NOT NULL,
            start_time TIMESTAMPTZ,
            end_time TIMESTAMPTZ,
            horizon TEXT,
            symbol TEXT,
            raw_market JSONB,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure pm_market_metadata table")?;

    println!("\nBackfilling PM replay tables from sync_records...");
    println!(
        "  symbols: {}",
        if symbol_list.is_empty() {
            "(all)".to_string()
        } else {
            symbol_list.join(",")
        }
    );
    println!(
        "  from: {}",
        from_dt
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  to:   {}",
        to_dt
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );

    let quote_up = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_yes_token_id)
                sr.timestamp AS received_at,
                sr.pm_yes_token_id AS token_id,
                sr.pm_yes_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND sr.pm_yes_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_yes_token_id
        )
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
        )
        SELECT
            src.token_id,
            'UP',
            GREATEST(src.best_ask - 0.01, 0.0001)::NUMERIC(10,6),
            src.best_ask::NUMERIC(10,6),
            NULL,
            NULL,
            'sync_backfill',
            src.received_at,
            'crypto'
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_quote_ticks q
            WHERE q.token_id = src.token_id
              AND q.side = 'UP'
              AND q.received_at = src.received_at
              AND q.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill clob_quote_ticks UP rows")?
    .rows_affected();

    let quote_down = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_no_token_id)
                sr.timestamp AS received_at,
                sr.pm_no_token_id AS token_id,
                sr.pm_no_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND sr.pm_no_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_no_token_id
        )
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask, bid_size, ask_size, source, received_at, domain
        )
        SELECT
            src.token_id,
            'DOWN',
            GREATEST(src.best_ask - 0.01, 0.0001)::NUMERIC(10,6),
            src.best_ask::NUMERIC(10,6),
            NULL,
            NULL,
            'sync_backfill',
            src.received_at,
            'crypto'
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_quote_ticks q
            WHERE q.token_id = src.token_id
              AND q.side = 'DOWN'
              AND q.received_at = src.received_at
              AND q.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill clob_quote_ticks DOWN rows")?
    .rows_affected();

    let md_rows = sqlx::query(
        r#"
        WITH agg AS (
            SELECT
                sr.pm_market_slug AS market_slug,
                (array_agg(sr.symbol ORDER BY sr.timestamp ASC))[1] AS symbol,
                MIN(sr.timestamp) AS start_time,
                MAX(sr.timestamp) AS observed_end_time,
                (array_agg(sr.bn_mid_price ORDER BY sr.timestamp ASC))[1] AS price_to_beat
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            GROUP BY sr.pm_market_slug
        )
        INSERT INTO pm_market_metadata (
            market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at
        )
        SELECT
            market_slug,
            COALESCE(price_to_beat, 0),
            start_time,
            CASE
                WHEN market_slug LIKE '%-5m-%' THEN start_time + INTERVAL '5 minutes'
                WHEN market_slug LIKE '%-15m-%' THEN start_time + INTERVAL '15 minutes'
                WHEN market_slug LIKE '%-60m-%' THEN start_time + INTERVAL '60 minutes'
                ELSE observed_end_time
            END AS end_time,
            CASE
                WHEN market_slug LIKE '%-5m-%' THEN '5m'
                WHEN market_slug LIKE '%-15m-%' THEN '15m'
                WHEN market_slug LIKE '%-60m-%' THEN '60m'
                ELSE NULL
            END AS horizon,
            symbol,
            jsonb_build_object(
                'source', 'sync_backfill',
                'derived_from', 'sync_records',
                'market_slug', market_slug,
                'symbol', symbol
            ),
            NOW()
        FROM agg
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = CASE
                WHEN EXCLUDED.price_to_beat > 0 THEN EXCLUDED.price_to_beat
                ELSE pm_market_metadata.price_to_beat
            END,
            start_time = COALESCE(pm_market_metadata.start_time, EXCLUDED.start_time),
            end_time = COALESCE(pm_market_metadata.end_time, EXCLUDED.end_time),
            horizon = COALESCE(pm_market_metadata.horizon, EXCLUDED.horizon),
            symbol = COALESCE(pm_market_metadata.symbol, EXCLUDED.symbol),
            raw_market = COALESCE(pm_market_metadata.raw_market, EXCLUDED.raw_market),
            updated_at = NOW()
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .execute(pool)
    .await
    .context("Failed to backfill pm_market_metadata")?
    .rows_affected();

    let ob_up = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_yes_token_id)
                sr.timestamp AS received_at,
                sr.pm_market_slug AS market_slug,
                sr.pm_yes_token_id AS token_id,
                sr.pm_yes_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND sr.pm_yes_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_yes_token_id
        )
        INSERT INTO clob_orderbook_snapshots (
            domain, token_id, market, bids, asks, book_timestamp, hash, source, context, received_at
        )
        SELECT
            'crypto',
            src.token_id,
            src.market_slug,
            jsonb_build_array(
                jsonb_build_object(
                    'price', GREATEST((1 - src.best_ask), 0.0001)::text,
                    'size', $4::text
                )
            ),
            jsonb_build_array(
                jsonb_build_object(
                    'price', src.best_ask::text,
                    'size', $4::text
                )
            ),
            src.received_at,
            NULL,
            'sync_backfill',
            jsonb_build_object('synthetic', true, 'side', 'UP', 'source', 'sync_records'),
            src.received_at
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_orderbook_snapshots s
            WHERE s.token_id = src.token_id
              AND s.received_at = src.received_at
              AND s.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(synthetic_depth as i64)
    .execute(pool)
    .await
    .context("Failed to backfill clob_orderbook_snapshots UP rows")?
    .rows_affected();

    let ob_down = sqlx::query(
        r#"
        WITH src AS (
            SELECT DISTINCT ON (sr.timestamp, sr.pm_no_token_id)
                sr.timestamp AS received_at,
                sr.pm_market_slug AS market_slug,
                sr.pm_no_token_id AS token_id,
                sr.pm_no_price AS best_ask
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND sr.pm_no_price IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            ORDER BY sr.timestamp, sr.pm_no_token_id
        )
        INSERT INTO clob_orderbook_snapshots (
            domain, token_id, market, bids, asks, book_timestamp, hash, source, context, received_at
        )
        SELECT
            'crypto',
            src.token_id,
            src.market_slug,
            jsonb_build_array(
                jsonb_build_object(
                    'price', GREATEST((1 - src.best_ask), 0.0001)::text,
                    'size', $4::text
                )
            ),
            jsonb_build_array(
                jsonb_build_object(
                    'price', src.best_ask::text,
                    'size', $4::text
                )
            ),
            src.received_at,
            NULL,
            'sync_backfill',
            jsonb_build_object('synthetic', true, 'side', 'DOWN', 'source', 'sync_records'),
            src.received_at
        FROM src
        WHERE NOT EXISTS (
            SELECT 1
            FROM clob_orderbook_snapshots s
            WHERE s.token_id = src.token_id
              AND s.received_at = src.received_at
              AND s.source = 'sync_backfill'
        )
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(synthetic_depth as i64)
    .execute(pool)
    .await
    .context("Failed to backfill clob_orderbook_snapshots DOWN rows")?
    .rows_affected();

    let quote_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM clob_quote_ticks
        WHERE source = 'sync_backfill'
          AND ($1::timestamptz IS NULL OR received_at >= $1)
          AND ($2::timestamptz IS NULL OR received_at <= $2)
        "#,
    )
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let ob_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM clob_orderbook_snapshots
        WHERE source = 'sync_backfill'
          AND ($1::timestamptz IS NULL OR received_at >= $1)
          AND ($2::timestamptz IS NULL OR received_at <= $2)
        "#,
    )
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let md_total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::bigint
        FROM pm_market_metadata
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2)
          AND ($3::timestamptz IS NULL OR start_time <= $3)
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    println!("\nBackfill complete:");
    println!(
        "  clob_quote_ticks inserted: {} (UP {}) + (DOWN {})",
        quote_up + quote_down,
        quote_up,
        quote_down
    );
    println!(
        "  clob_orderbook_snapshots inserted: {} (UP {}) + (DOWN {})",
        ob_up + ob_down,
        ob_up,
        ob_down
    );
    println!("  pm_market_metadata upsert affected rows: {}", md_rows);
    println!("\nCurrent totals in selected window:");
    println!("  clob_quote_ticks (sync_backfill): {}", quote_total);
    println!("  clob_orderbook_snapshots (sync_backfill): {}", ob_total);
    println!("  pm_market_metadata: {}", md_total);
    println!();

    Ok(())
}
