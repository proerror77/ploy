use anyhow::{Context, Result};
use super::super::settlement_ops::is_market_resolved;

use crate::adapters::{PolymarketClient, PostgresStore};
use chrono::{DateTime, Utc};

fn parse_optional_bound(value: Option<&str>, flag: &str) -> Result<Option<DateTime<Utc>>> {
    let parsed = value
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        })
        .transpose()
        .with_context(|| format!("Invalid {} date (use ISO 8601 format)", flag))?
        .map(|dt| dt.with_timezone(&Utc));

    Ok(parsed)
}

fn validate_window(from_dt: &Option<DateTime<Utc>>, to_dt: &Option<DateTime<Utc>>) -> Result<()> {
    if let (Some(from_dt), Some(to_dt)) = (from_dt.as_ref(), to_dt.as_ref()) {
        if to_dt <= from_dt {
            anyhow::bail!("--to must be after --from");
        }
    }

    Ok(())
}

fn parse_symbol_filter(symbols: &str) -> (Vec<String>, Option<Vec<String>>) {
    let symbol_list: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let symbols_param = if symbol_list.is_empty() {
        None
    } else {
        Some(symbol_list.clone())
    };

    (symbol_list, symbols_param)
}

fn resolve_database_url(database_url: Option<String>) -> String {
    database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    })
}

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

    crate::platform::persistence_schema::ensure_clob_quote_ticks_table(pool)
        .await
        .context("Failed to ensure clob_quote_ticks table")?;
    crate::platform::persistence_schema::ensure_clob_orderbook_snapshots_table(pool)
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

/// Backfill official Polymarket token settlements into pm_token_settlements.
///
/// Source of token universe:
/// - Distinct pm_yes_token_id / pm_no_token_id from sync_records in selected window.
///
/// Source of official settlement state:
/// - Gamma market lookup by token_id via Polymarket API.
pub(crate) async fn backfill_pm_token_settlements(
    from: Option<String>,
    to: Option<String>,
    symbols: &str,
    limit: usize,
    database_url: Option<String>,
) -> Result<()> {
    use rust_decimal::Decimal;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

    let from_dt = parse_optional_bound(from.as_deref(), "--from")?;
    let to_dt = parse_optional_bound(to.as_deref(), "--to")?;
    validate_window(&from_dt, &to_dt)?;

    let (symbol_list, symbols_param) = parse_symbol_filter(symbols);

    let db_url = resolve_database_url(database_url);
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    crate::persistence::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    println!("\nBackfilling pm_token_settlements from Gamma...");
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
    println!("  token limit: {}", limit);

    let token_ids: Vec<String> = sqlx::query_scalar(
        r#"
        WITH tokens AS (
            SELECT sr.pm_yes_token_id AS token_id
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_yes_token_id IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
            UNION
            SELECT sr.pm_no_token_id AS token_id
            FROM sync_records sr
            WHERE sr.pm_market_slug IS NOT NULL
              AND sr.pm_no_token_id IS NOT NULL
              AND ($1::text[] IS NULL OR sr.symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR sr.timestamp >= $2)
              AND ($3::timestamptz IS NULL OR sr.timestamp <= $3)
        )
        SELECT token_id
        FROM tokens
        ORDER BY token_id
        LIMIT $4
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .context("Failed to query token_ids from sync_records")?;

    if token_ids.is_empty() {
        println!("  No token_ids found in sync_records for selected window.\n");
        return Ok(());
    }

    let token_slug_rows = sqlx::query(
        r#"
        SELECT DISTINCT pm_market_slug, pm_yes_token_id, pm_no_token_id
        FROM sync_records
        WHERE pm_market_slug IS NOT NULL
          AND ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR timestamp >= $2)
          AND ($3::timestamptz IS NULL OR timestamp <= $3)
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_all(pool)
    .await
    .context("Failed to query token↔slug map from sync_records")?;

    let mut token_to_slug: HashMap<String, String> = HashMap::new();
    for row in &token_slug_rows {
        let slug: Option<String> = row.try_get("pm_market_slug").ok();
        let yes: Option<String> = row.try_get("pm_yes_token_id").ok();
        let no: Option<String> = row.try_get("pm_no_token_id").ok();
        let Some(slug) = slug else { continue };
        if let Some(token_id) = yes {
            token_to_slug
                .entry(token_id)
                .or_insert_with(|| slug.clone());
        }
        if let Some(token_id) = no {
            token_to_slug.entry(token_id).or_insert_with(|| slug.clone());
        }
    }

    let md_rows = sqlx::query(
        r#"
        SELECT market_slug, end_time
        FROM pm_market_metadata
        WHERE end_time IS NOT NULL
          AND ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2)
          AND ($3::timestamptz IS NULL OR end_time <= $3 + INTERVAL '1 day')
        "#,
    )
    .bind(symbols_param.clone())
    .bind(from_dt)
    .bind(to_dt)
    .fetch_all(pool)
    .await
    .context("Failed to query pm_market_metadata end_time map")?;

    let mut slug_to_end: HashMap<String, DateTime<Utc>> = HashMap::new();
    for row in &md_rows {
        let slug: String = row.get("market_slug");
        let end_time: DateTime<Utc> = row.get("end_time");
        slug_to_end.entry(slug).or_insert(end_time);
    }

    let existing_rows = sqlx::query(
        r#"
        SELECT token_id, resolved
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(&token_ids)
    .fetch_all(pool)
    .await
    .context("Failed to query existing pm_token_settlements rows")?;

    let mut resolved_map: HashMap<String, bool> = HashMap::new();
    for row in existing_rows {
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        resolved_map.insert(token_id, resolved);
    }

    let mut to_refresh: Vec<String> = token_ids
        .iter()
        .filter(|token_id| !resolved_map.get(*token_id).copied().unwrap_or(false))
        .cloned()
        .collect();
    to_refresh.sort();
    to_refresh.dedup();

    if to_refresh.is_empty() {
        println!("  All candidate tokens already marked resolved.\n");
        return Ok(());
    }

    println!("  refreshing {} token(s) via Gamma...", to_refresh.len());

    let pm = PolymarketClient::new("https://clob.polymarket.com", true)
        .context("Failed to create Polymarket client")?;

    let mut seen_conditions: HashSet<String> = HashSet::new();
    let mut refreshed_markets = 0usize;
    let mut upserted_rows = 0usize;
    let mut resolved_rows = 0usize;

    for token_id in to_refresh {
        let market = match pm.get_gamma_market_by_token_id(&token_id).await {
            Ok(market) => market,
            Err(error) => {
                tracing::warn!(token_id = %token_id, error = %error, "Gamma fetch failed for token");
                continue;
            }
        };

        if let Some(ref condition_id) = market.condition_id {
            let condition_id = condition_id.to_string();
            if !seen_conditions.insert(condition_id) {
                continue;
            }
        }

        let clob_ids: Vec<String> = market
            .clob_token_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.to_string()).collect())
            .unwrap_or_default();
        let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
        let price_strs: Vec<String> = market
            .outcome_prices
            .as_ref()
            .map(|prices| prices.iter().map(|price| price.to_string()).collect())
            .unwrap_or_default();

        if clob_ids.is_empty() || price_strs.is_empty() {
            continue;
        }

        let mut prices: Vec<Decimal> = Vec::new();
        for price in &price_strs {
            if let Ok(price) = price.parse::<Decimal>() {
                prices.push(price);
            }
        }

        let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
        let condition_id = market.condition_id.map(|value| value.to_string());

        for (index, token_id) in clob_ids.iter().enumerate() {
            let outcome = outcomes.get(index).cloned();
            let settled_price = price_strs
                .get(index)
                .and_then(|price| price.parse::<Decimal>().ok());

            let slug_fallback = token_to_slug.get(token_id).cloned();
            let market_slug = market.slug.clone().or(slug_fallback);
            let resolved_at = if resolved {
                market_slug
                    .as_ref()
                    .and_then(|slug| slug_to_end.get(slug).cloned())
                    .or_else(|| Some(Utc::now()))
            } else {
                None
            };

            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            sqlx::query(
                r#"
                INSERT INTO pm_token_settlements (
                    token_id,
                    condition_id,
                    market_id,
                    market_slug,
                    outcome,
                    settled_price,
                    resolved,
                    resolved_at,
                    fetched_at,
                    raw_market
                )
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                ON CONFLICT (token_id) DO UPDATE SET
                    condition_id = EXCLUDED.condition_id,
                    market_id = EXCLUDED.market_id,
                    market_slug = EXCLUDED.market_slug,
                    outcome = EXCLUDED.outcome,
                    settled_price = EXCLUDED.settled_price,
                    resolved = EXCLUDED.resolved,
                    resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                    fetched_at = NOW(),
                    raw_market = EXCLUDED.raw_market
                "#,
            )
            .bind(token_id)
            .bind(condition_id.as_deref())
            .bind(&market.id)
            .bind(market_slug.as_deref())
            .bind(outcome.as_deref())
            .bind(settled_price)
            .bind(resolved)
            .bind(resolved_at)
            .bind(sqlx::types::Json(raw_market.clone()))
            .execute(pool)
            .await
            .context("Failed to upsert pm_token_settlements row")?;

            upserted_rows += 1;
            if resolved {
                resolved_rows += 1;
            }
        }

        refreshed_markets += 1;
    }

    let summary = sqlx::query(
        r#"
        SELECT
          COUNT(*)::bigint AS total_rows,
          COUNT(*) FILTER (WHERE resolved)::bigint AS resolved_rows,
          MIN(resolved_at) AS min_resolved_at,
          MAX(resolved_at) AS max_resolved_at
        FROM pm_token_settlements
        "#,
    )
    .fetch_one(pool)
    .await
    .context("Failed to query settlement summary")?;

    let total_rows: i64 = summary.get("total_rows");
    let total_resolved: i64 = summary.get("resolved_rows");
    let min_resolved: Option<DateTime<Utc>> = summary.try_get("min_resolved_at").ok();
    let max_resolved: Option<DateTime<Utc>> = summary.try_get("max_resolved_at").ok();

    println!("\nBackfill complete:");
    println!("  refreshed markets: {}", refreshed_markets);
    println!("  upserted token rows: {}", upserted_rows);
    println!("  upserted resolved rows: {}", resolved_rows);
    println!("  table total rows: {}", total_rows);
    println!("  table total resolved rows: {}", total_resolved);
    println!(
        "  resolved_at range: {} .. {}",
        min_resolved
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        max_resolved
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!();

    Ok(())
}
