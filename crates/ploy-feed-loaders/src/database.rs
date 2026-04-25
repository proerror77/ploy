//! Database-backed historical feed loader.
//!
//! Loads market data from PostgreSQL into a `Vec<MarketUpdate>` that
//! can be wrapped in [`HistoricalFeed`] for backtesting.
//!
//! Kept outside `ploy-strategy-bundles` so strategy logic can compile without
//! SQLx when only synthetic, recorded, or live feeds are needed.
//!
//! # Tables Queried
//!
//! | Table | Data |
//! |-------|------|
//! | `sync_records` or `binance_price_ticks` | CEX spot prices |
//! | `binance_lob_ticks` | L2 orderbook (OBI, spread) |
//! | `clob_quote_ticks` | Polymarket quotes |
//! | `pm_market_metadata` | Event windows (UP/DOWN tokens) |
//! | `pm_token_settlements` | Settlement outcomes |

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use ploy_market_contracts::{
    HistoricalLoadOptions, MarketUpdate, l2_updates_from_depth_totals, market_update_sort_ts,
    normalize_token_id,
};

#[cfg(test)]
use serde_json::Value;

/// How far before `from` to load spot prices for EWMA volatility warm-up.
const WARMUP_MINUTES: i64 = 30;

/// Historical research backtests only trust canonical historical PM quote captures.
///
/// Older `ploy_runner_live` rows were synthetic midpoint quotes; newer rows carry
/// filtered top-of-book sizes from REST `/book`. The existing
/// `polymarket_ws_collector` history before the next validated cutover is too
/// polluted with placeholder `0.01/0.99` books to mix into research results.
/// Dry-run parity should use recorded replay mode instead of the historical DB path.
const TRUSTED_PM_RESEARCH_QUOTE_SOURCES: &[&str] = &[
    "polymarket_ws",
    "polymarket_ws_collector",
    "ploy_runner_live",
];

/// One persisted reference-price tick from `reference_price_ticks`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReferencePriceTick {
    pub symbol: String,
    pub source: String,
    pub asset_class: String,
    pub price: Decimal,
    pub full_accuracy_value: Option<String>,
    pub price_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub is_carried_forward: bool,
}

/// One persisted sports-state event from `sports_state_events`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SportsStateEventRow {
    pub game_id: String,
    pub league: String,
    pub slug: String,
    pub home_team: String,
    pub away_team: String,
    pub status: String,
    pub period: Option<String>,
    pub score: Option<String>,
    pub elapsed: Option<String>,
    pub live: bool,
    pub ended: bool,
    pub finished_at: Option<DateTime<Utc>>,
    pub event_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub source: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EventMetadataRow {
    market_slug: String,
    symbol: Option<String>,
    end_time: Option<DateTime<Utc>>,
    start_time: Option<DateTime<Utc>>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    price_to_beat: Option<Decimal>,
}

/// Load all historical market updates for given symbols and time range.
///
/// Returns updates sorted by timestamp, ready for `HistoricalFeed::new()`.
///
/// Spot prices are loaded from `from - WARMUP_MINUTES` to give the EWMA
/// volatility estimator enough history before the first event arrives.
/// All other data (quotes, events, L2) uses the exact `from..to` range.
pub async fn load_from_database(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    load_from_database_with_options(pool, symbols, from, to, &HistoricalLoadOptions::default())
        .await
}

/// Load all historical market updates for given symbols and time range plus
/// any explicitly requested additive sources.
pub async fn load_from_database_with_options(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    let mut updates: Vec<MarketUpdate> = Vec::new();

    // 1. Spot prices — load earlier for EWMA warm-up
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);
    load_spot_prices(
        pool,
        symbols,
        spot_from,
        to,
        options.spot_sample_secs,
        &mut updates,
    )
    .await?;

    // 1b. Aggregated trade flow — additive signal stream for roadmap work.
    load_agg_trades(pool, symbols, from, to, &mut updates).await?;

    // 2. Event windows FIRST — must be registered before quotes are processed.
    //    Quotes for a token can arrive before the event's official start_time
    //    (Polymarket tokens are continuous; ~90% of quotes precede start_time).
    //    Loading events before quotes ensures token_symbol mappings exist.
    load_events(
        pool,
        symbols,
        from,
        to,
        options.require_official_settlement,
        &mut updates,
    )
    .await?;

    // 3. Polymarket quotes — prefer persisted top-of-book rows when they carry
    // executable size. Older `ploy_runner_live` quote rows are price-only; those
    // are useful for rough direction studies but must not drive executable PnL.
    let token_map = load_token_mappings(pool, symbols, from, to).await?;
    let quote_count_before = updates.len();
    let quote_stats = load_pm_quotes(pool, &token_map, from, to, &mut updates).await?;

    // If clob_quote_ticks has no executable size, replace those price-only
    // rows with quotes extracted from full order-book snapshots. This preserves
    // point-in-time prices while grounding research labels in actual CLOB depth.
    if quote_stats.rows == 0 || quote_stats.sized_rows == 0 {
        if quote_stats.rows == 0 {
            info!("No real quotes in clob_quote_ticks, falling back to orderbook snapshots");
        } else {
            info!(
                rows = quote_stats.rows,
                "clob_quote_ticks rows are price-only, replacing with orderbook snapshot depth"
            );
        }
        updates.truncate(quote_count_before);
        let snapshot_stats = load_pm_quotes_from_snapshots(
            pool,
            &token_map,
            from,
            to,
            options.lob_sample_secs,
            &mut updates,
        )
        .await?;
        if snapshot_stats.rows == 0 && quote_stats.rows > 0 {
            info!("No usable orderbook snapshots found, restoring price-only quote rows");
            load_pm_quotes(pool, &token_map, from, to, &mut updates).await?;
        }
    }

    // 4. L2 orderbook from binance_lob_ticks.
    //
    // Some research consumers load richer LOB snapshots separately and do not
    // need these generic updates. Keep the default enabled for strategy replay,
    // but allow research jobs to avoid scanning the large LOB table twice.
    if options.include_l2 {
        load_l2_data(
            pool,
            symbols,
            from,
            to,
            options.lob_sample_secs,
            &mut updates,
        )
        .await?;
    }

    if options.include_reference_prices {
        let reference_symbols = options.normalized_reference_symbols();
        if !reference_symbols.is_empty() {
            updates.extend(load_reference_price_updates(pool, &reference_symbols, from, to).await?);
        }
    }

    if options.include_sports_state {
        updates.extend(load_sports_state_events(pool, from, to).await?);
    }

    // Sort by timestamp
    updates.sort_by_key(market_update_sort_ts);

    info!(
        count = updates.len(),
        symbols = ?symbols,
        reference_symbols = ?options.normalized_reference_symbols(),
        include_reference_prices = options.include_reference_prices,
        include_sports_state = options.include_sports_state,
        from = %from,
        to = %to,
        "Loaded historical data from database",
    );

    Ok(updates)
}

// ── Spot Prices ──────────────────────────────────────────

async fn load_spot_prices(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    let sample_secs = sample_secs.max(1) as i64;
    // Try sync_records first (has bn_mid_price)
    let rows: Vec<(DateTime<Utc>, String, Decimal)> = sqlx::query_as(
        r#"
        SELECT timestamp, symbol, bn_mid_price
        FROM (
            SELECT DISTINCT ON (symbol, bucket)
                   timestamp, symbol, bn_mid_price, bucket
            FROM (
                SELECT
                    timestamp,
                    symbol,
                    bn_mid_price,
                    to_timestamp(
                        floor(EXTRACT(EPOCH FROM timestamp) / $4::double precision) * $4
                    ) AS bucket
                FROM sync_records
                WHERE symbol = ANY($1)
                  AND timestamp >= $2
                  AND timestamp <= $3
            ) ticks
            ORDER BY symbol, bucket, timestamp DESC
        ) sampled
        ORDER BY timestamp
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if !rows.is_empty() {
        info!(
            count = rows.len(),
            sample_secs,
            "Loaded spot prices from sync_records"
        );
        for (ts, symbol, price) in rows {
            updates.push(MarketUpdate::SpotPrice {
                symbol: Arc::from(symbol),
                price,
                ts,
            });
        }
        return Ok(());
    }

    // Fallback: binance_price_ticks
    let rows: Vec<(DateTime<Utc>, String, Decimal)> = sqlx::query_as(
        r#"
        SELECT trade_time, symbol, price
        FROM (
            SELECT DISTINCT ON (symbol, bucket)
                   trade_time, symbol, price, bucket
            FROM (
                SELECT
                    trade_time,
                    symbol,
                    price,
                    to_timestamp(
                        floor(EXTRACT(EPOCH FROM trade_time) / $4::double precision) * $4
                    ) AS bucket
                FROM binance_price_ticks
                WHERE symbol = ANY($1)
                  AND trade_time >= $2
                  AND trade_time <= $3
            ) ticks
            ORDER BY symbol, bucket, trade_time DESC
        ) sampled
        ORDER BY trade_time
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        sample_secs,
        "Loaded spot prices from binance_price_ticks"
    );
    for (ts, symbol, price) in rows {
        updates.push(MarketUpdate::SpotPrice {
            symbol: Arc::from(symbol),
            price,
            ts,
        });
    }

    Ok(())
}

async fn load_agg_trades(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    // Downsample to one row per 5-second bucket per symbol to avoid OOM.
    // AggTrade volume can reach millions of rows per hour; the strategy only
    // needs the signed trade imbalance signal, not tick-level granularity.
    let rows: Vec<(DateTime<Utc>, String, i64, Decimal, Decimal, bool)> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (symbol, date_trunc('seconds', trade_time) - INTERVAL '1 second' * (EXTRACT(EPOCH FROM trade_time)::bigint % 5))
               trade_time, symbol, agg_trade_id, price, quantity, is_buyer_maker
        FROM binance_agg_trade_ticks
        WHERE symbol = ANY($1)
          AND trade_time >= $2
          AND trade_time <= $3
        ORDER BY symbol,
                 date_trunc('seconds', trade_time) - INTERVAL '1 second' * (EXTRACT(EPOCH FROM trade_time)::bigint % 5),
                 trade_time
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        "Loaded agg trades from binance_agg_trade_ticks (5s downsampled)"
    );
    for (ts, symbol, agg_trade_id, price, quantity, is_buyer_maker) in rows {
        updates.push(MarketUpdate::AggTrade {
            symbol: Arc::from(symbol),
            agg_trade_id: agg_trade_id as u64,
            price,
            quantity,
            is_buyer_maker,
            ts,
        });
    }

    Ok(())
}

/// Load reference-price ticks from the additive `reference_price_ticks` table.
pub async fn load_reference_price_ticks(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<ReferencePriceTick>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            symbol,
            source,
            asset_class,
            price,
            full_accuracy_value,
            price_time,
            received_at,
            is_carried_forward
        FROM reference_price_ticks
        WHERE symbol = ANY($1)
          AND price_time >= $2
          AND price_time <= $3
        ORDER BY price_time
        "#,
    )
    .bind(
        &symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .collect::<Vec<_>>(),
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

/// Load reference-price ticks as canonical `MarketUpdate` values.
pub async fn load_reference_price_updates(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    let rows = load_reference_price_ticks(pool, symbols, from, to).await?;

    info!(
        count = rows.len(),
        symbols = ?symbols,
        "Loaded reference prices from reference_price_ticks"
    );

    Ok(rows
        .into_iter()
        .map(|row| MarketUpdate::ReferencePrice {
            symbol: Arc::from(row.symbol),
            source: Arc::from(row.source),
            asset_class: Arc::from(row.asset_class),
            price: row.price,
            full_accuracy_value: row.full_accuracy_value.map(Arc::from),
            is_carried_forward: row.is_carried_forward,
            ts: row.price_time,
        })
        .collect())
}

/// Load normalized sports-state events from `sports_state_events`.
pub async fn load_sports_state_events(
    pool: &PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    let rows: Vec<SportsStateEventRow> = sqlx::query_as(
        r#"
        SELECT
            game_id,
            league,
            slug,
            home_team,
            away_team,
            status,
            period,
            score,
            elapsed,
            live,
            ended,
            finished_at,
            event_time,
            received_at,
            source
        FROM sports_state_events
        WHERE event_time >= $1
          AND event_time <= $2
        ORDER BY event_time, received_at, id
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        "Loaded sports state from sports_state_events"
    );

    Ok(rows
        .into_iter()
        .map(|row| MarketUpdate::SportsState {
            game_id: Arc::from(row.game_id),
            league: Arc::from(row.league),
            slug: Arc::from(row.slug),
            home_team: Arc::from(row.home_team),
            away_team: Arc::from(row.away_team),
            status: Arc::from(row.status),
            period: row.period.map(Arc::from),
            score: row.score.map(Arc::from),
            elapsed: row.elapsed.map(Arc::from),
            live: row.live,
            ended: row.ended,
            finished_at: row.finished_at,
            ts: row.event_time,
        })
        .collect())
}

// ── Token Mappings ───────────────────────────────────────

/// Map of token_id → (symbol, is_up_token)
type TokenMap = std::collections::HashMap<String, (String, bool)>;

#[derive(Debug, Default, Clone, Copy)]
struct PmQuoteLoadStats {
    rows: usize,
    sized_rows: usize,
}

async fn load_token_mappings(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<TokenMap, sqlx::Error> {
    let mut map = TokenMap::new();

    // Extract token IDs from JSONB structure: raw_market->'markets'->0->>'clobTokenIds'
    // clobTokenIds is stored as a JSON string, so we use ->> then cast to jsonb
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            token_id,
            symbol
        FROM pm_market_metadata
        CROSS JOIN LATERAL (
            SELECT jsonb_array_elements_text(
                (raw_market->'markets'->0->>'clobTokenIds')::jsonb
            ) as token_id
        ) t
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND end_time >= $2
          AND start_time <= $3
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (_slug, token_id, symbol) in rows {
        let token_id = normalize_token_id(&token_id);
        let sym = symbol.unwrap_or_default();
        // Heuristic: UP tokens tend to come first in the array
        let is_up = !map.contains_key(&token_id);
        map.insert(token_id, (sym, is_up));
    }

    info!(tokens = map.len(), "Loaded token mappings");
    Ok(map)
}

// ── PM Quotes ────────────────────────────────────────────

async fn load_pm_quotes(
    pool: &PgPool,
    token_map: &TokenMap,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<PmQuoteLoadStats, sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(PmQuoteLoadStats::default());
    }
    let trusted_sources: Vec<&str> = TRUSTED_PM_RESEARCH_QUOTE_SOURCES.to_vec();

    // Polymarket UP/DOWN token structure:
    // - UP token:   best_bid is the real price, best_ask may be NULL
    // - DOWN token: best_ask is the real price, best_bid may be NULL
    // Accept rows where EITHER bid OR ask is in the real price range (0.01, 0.99).
    // Within each second, prefer book rows with executable size over later price-only
    // best_bid_ask/price_change rows; otherwise LOB-aware replay sees valid prices
    // but no executable top-of-book liquidity.
    let rows: Vec<(
        DateTime<Utc>,
        String,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
    )> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at, token_id, best_bid, best_ask, bid_size, ask_size
        FROM clob_quote_ticks
        WHERE received_at >= $1
          AND received_at <= $2
          AND token_id = ANY($3)
          AND source = ANY($4)
          AND (
            (best_bid  IS NOT NULL AND best_bid  > 0.01 AND best_bid  < 0.99)
            OR
            (best_ask  IS NOT NULL AND best_ask  > 0.01 AND best_ask  < 0.99)
          )
        ORDER BY date_trunc('second', received_at), token_id,
                 (ask_size IS NOT NULL AND ask_size > 0)::int DESC,
                 (bid_size IS NOT NULL AND bid_size > 0)::int DESC,
                 received_at DESC
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .bind(&trusted_sources)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let sized_rows = rows
        .iter()
        .filter(|(_, _, _, _, bid_size, ask_size)| {
            bid_size.is_some_and(|size| size > Decimal::ZERO)
                || ask_size.is_some_and(|size| size > Decimal::ZERO)
        })
        .count();

    info!(
        count = rows.len(),
        sized_rows,
        sources = ?TRUSTED_PM_RESEARCH_QUOTE_SOURCES,
        "Loaded PM quotes from clob_quote_ticks (trusted sources, filtered: bid/ask in 0.02-0.98)"
    );
    let row_count = rows.len();
    for (ts, token_id, bid, ask, bid_size, ask_size) in rows {
        updates.push(MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid,
            ask,
            bid_size,
            ask_size,
            ts,
        });
    }

    Ok(PmQuoteLoadStats {
        rows: row_count,
        sized_rows,
    })
}

/// Fallback quote loader from `clob_orderbook_snapshots`.
///
/// Polymarket CLOB structure for UP/DOWN binary markets:
/// - UP token:   bids array has full depth (0.01→0.99), asks is empty []
/// - DOWN token: asks array has full depth (0.99→0.01), bids is empty []
///
/// So best_bid = MAX(bids prices), best_ask = MIN(asks prices).
/// For UP tokens: best_bid is the real market price.
/// For DOWN tokens: best_ask is the real market price (= 1 - UP price).
async fn load_pm_quotes_from_snapshots(
    pool: &PgPool,
    token_map: &TokenMap,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<PmQuoteLoadStats, sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(PmQuoteLoadStats::default());
    }
    let sample_secs = sample_secs.max(1) as i64;

    // Extract executable top-of-book directly from JSONB depth. This intentionally
    // avoids synthetic opposite-side prices: if a snapshot has only bids or only
    // asks, only that executable side receives size. Downsample in SQL before
    // expanding JSONB so full-day factor reviews do not materialize every CLOB
    // snapshot into memory.
    let rows: Vec<(
        DateTime<Utc>,
        String,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
    )> = sqlx::query_as(
        r#"
        WITH sampled AS (
            SELECT DISTINCT ON (snapshot.token_id, snapshot.bucket)
                   snapshot.received_at,
                   snapshot.token_id,
                   snapshot.bids,
                   snapshot.asks
            FROM (
                SELECT
                    received_at,
                    token_id,
                    bids,
                    asks,
                    to_timestamp(
                        floor(EXTRACT(EPOCH FROM received_at) / $4::double precision) * $4
                    ) AS bucket
                FROM clob_orderbook_snapshots
                WHERE received_at >= $1
                  AND received_at <= $2
                  AND token_id = ANY($3)
            ) snapshot
            ORDER BY snapshot.token_id, snapshot.bucket, snapshot.received_at DESC
        ),
        extracted AS (
            SELECT snapshot.received_at,
                   snapshot.token_id,
                   best_bid.price AS best_bid,
                   best_ask.price AS best_ask,
                   best_bid.size AS bid_size,
                   best_ask.size AS ask_size
            FROM sampled snapshot
            LEFT JOIN LATERAL (
                SELECT (elem->>'price')::numeric AS price,
                       (elem->>'size')::numeric AS size
                FROM jsonb_array_elements(COALESCE(snapshot.bids, '[]'::jsonb)) AS elem
                WHERE (elem->>'price')::numeric > 0.01
                  AND (elem->>'price')::numeric < 0.99
                  AND (elem->>'size')::numeric > 0
                ORDER BY price DESC
                LIMIT 1
            ) best_bid ON true
            LEFT JOIN LATERAL (
                SELECT (elem->>'price')::numeric AS price,
                       (elem->>'size')::numeric AS size
                FROM jsonb_array_elements(COALESCE(snapshot.asks, '[]'::jsonb)) AS elem
                WHERE (elem->>'price')::numeric > 0.01
                  AND (elem->>'price')::numeric < 0.99
                  AND (elem->>'size')::numeric > 0
                ORDER BY price ASC
                LIMIT 1
            ) best_ask ON true
        )
        SELECT received_at, token_id, best_bid, best_ask, bid_size, ask_size
        FROM extracted
        WHERE best_bid IS NOT NULL OR best_ask IS NOT NULL
        ORDER BY received_at
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let sized_rows = rows
        .iter()
        .filter(|(_, _, _, _, bid_size, ask_size)| {
            bid_size.is_some_and(|size| size > Decimal::ZERO)
                || ask_size.is_some_and(|size| size > Decimal::ZERO)
        })
        .count();

    info!(
        count = rows.len(),
        sized_rows,
        sample_secs,
        "Loaded PM quotes from clob_orderbook_snapshots (real bid/ask and size extracted from JSONB depth)"
    );
    let row_count = rows.len();
    for (ts, token_id, bid, ask, bid_size, ask_size) in rows {
        updates.push(MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid,
            ask,
            bid_size,
            ask_size,
            ts,
        });
    }

    Ok(PmQuoteLoadStats {
        rows: row_count,
        sized_rows,
    })
}

// ── L2 Data ──────────────────────────────────────────────

async fn load_l2_data(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    let sample_secs = sample_secs.max(1) as i64;
    let rows: Vec<(DateTime<Utc>, String, Decimal, i32, Decimal, Decimal)> = match sqlx::query_as(
        r#"
        SELECT DISTINCT ON (symbol, date_trunc('second', event_time) - INTERVAL '1 second' * (EXTRACT(EPOCH FROM event_time)::bigint % $4))
               event_time, symbol,
               COALESCE(obi_5, 0.0) as obi,
               COALESCE(spread_bps, 0)::int as spread_bps,
               COALESCE(bid_volume_5, 0) as bid_volume_5,
               COALESCE(ask_volume_5, 0) as ask_volume_5
        FROM binance_lob_ticks
        WHERE symbol = ANY($1)
          AND event_time >= $2
          AND event_time <= $3
        ORDER BY symbol,
                 date_trunc('second', event_time) - INTERVAL '1 second' * (EXTRACT(EPOCH FROM event_time)::bigint % $4),
                 event_time DESC
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "Failed to load L2 data from binance_lob_ticks");
            Vec::new()
        }
    };

    info!(count = rows.len(), "Loaded L2 data from binance_lob_ticks");
    for (ts, symbol, obi, spread_bps, bid_volume_5, ask_volume_5) in rows {
        updates.extend(l2_updates_from_depth_totals(
            &symbol,
            obi.to_f64().unwrap_or_default(),
            spread_bps as u32,
            bid_volume_5,
            ask_volume_5,
            ts,
        ));
    }

    Ok(())
}

// near_depth / sum_depth_in_range / parse_depth_level / json_f64 are only
// used in tests via l2_updates_from_book.
#[cfg(test)]
const NEAR_DEPTH_PCT_RANGE: f64 = 0.001;

#[cfg(test)]
fn l2_updates_from_book(
    symbol: &str,
    obi: f64,
    spread_bps: u32,
    mid_price: Decimal,
    bids: Option<&Value>,
    asks: Option<&Value>,
    ts: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let sym: Arc<str> = Arc::from(symbol);
    let mut updates = vec![MarketUpdate::L2 {
        symbol: Arc::clone(&sym),
        obi,
        spread_bps,
        ts,
    }];

    if bids.is_none() && asks.is_none() {
        return updates;
    }

    let Some(mid_price) = mid_price.to_f64() else {
        return updates;
    };
    if !mid_price.is_finite() || mid_price <= 0.0 {
        return updates;
    }

    let empty = Value::Null;
    let (bid_depth_near, ask_depth_near) = near_depth(
        bids.unwrap_or(&empty),
        asks.unwrap_or(&empty),
        mid_price,
        NEAR_DEPTH_PCT_RANGE,
    );

    updates.push(MarketUpdate::L2Depth {
        symbol: sym,
        obi,
        spread_bps,
        bid_depth_near,
        ask_depth_near,
        ts,
    });

    updates
}

#[cfg(test)]
fn near_depth(bids: &Value, asks: &Value, mid_price: f64, pct_range: f64) -> (f64, f64) {
    if !mid_price.is_finite() || mid_price <= 0.0 || !pct_range.is_finite() || pct_range < 0.0 {
        return (0.0, 0.0);
    }

    let bid_min = mid_price * (1.0 - pct_range);
    let ask_max = mid_price * (1.0 + pct_range);

    (
        sum_depth_in_range(bids, bid_min, mid_price),
        sum_depth_in_range(asks, mid_price, ask_max),
    )
}

#[cfg(test)]
fn sum_depth_in_range(levels: &Value, min_price: f64, max_price: f64) -> f64 {
    levels
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(parse_depth_level)
                .filter(|(price, _)| *price >= min_price && *price <= max_price)
                .map(|(_, size)| size)
                .sum()
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

#[cfg(test)]
fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

// ── Events ───────────────────────────────────────────────

async fn load_events(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    require_official_settlement: bool,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    // Extract token IDs from JSONB: raw_market->'markets'->0->>'clobTokenIds'
    // clobTokenIds is stored as a JSON string, so we use ->> then cast to jsonb
    // First token is UP, second is DOWN
    let rows: Vec<EventMetadataRow> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            symbol,
            end_time,
            start_time,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text AS up_token_id,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text AS down_token_id,
            price_to_beat
        FROM pm_market_metadata
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND end_time >= $2
          AND start_time <= $3
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let event_ids: Vec<String> = rows.iter().map(|row| row.market_slug.clone()).collect();
    let settlement_prices = load_event_settlement_prices(pool, &event_ids).await?;

    info!(count = rows.len(), "Loaded events from pm_market_metadata");
    updates.extend(build_event_updates(
        rows,
        &settlement_prices,
        require_official_settlement,
    ));

    Ok(())
}

fn build_event_updates(
    rows: Vec<EventMetadataRow>,
    settlement_prices: &HashMap<(String, String), Decimal>,
    require_official_settlement: bool,
) -> Vec<MarketUpdate> {
    let mut updates = Vec::new();

    for row in rows {
        let symbol = row.symbol.unwrap_or_default();
        let end_time = match row.end_time {
            Some(t) => t,
            None => continue,
        };
        let start_time = match row.start_time {
            Some(t) => t,
            None => continue,
        };

        let up_token: Arc<str> =
            Arc::from(normalize_token_id(&row.up_token_id.unwrap_or_default()));
        let down_token: Arc<str> =
            Arc::from(normalize_token_id(&row.down_token_id.unwrap_or_default()));
        let resolved_up_won = resolve_up_won_from_settlements(
            settlement_prices,
            &row.market_slug,
            &up_token,
            &down_token,
        );
        if require_official_settlement && resolved_up_won.is_none() {
            continue;
        }
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        if symbol.is_empty() || up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        let event_id: Arc<str> = Arc::from(row.market_slug);
        let symbol: Arc<str> = Arc::from(symbol);

        updates.push(MarketUpdate::EventDiscovered {
            event_id: Arc::clone(&event_id),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat: row.price_to_beat,
            resolved_up_won: None,
        });
        updates.push(MarketUpdate::EventExpired {
            event_id,
            end_time,
            resolved_up_won,
        });
    }

    updates
}

fn resolve_up_won_from_settlements(
    settlement_prices: &HashMap<(String, String), Decimal>,
    event_id: &str,
    up_token: &str,
    down_token: &str,
) -> Option<bool> {
    match (
        settlement_prices
            .get(&(event_id.to_string(), up_token.to_string()))
            .copied(),
        settlement_prices
            .get(&(event_id.to_string(), down_token.to_string()))
            .copied(),
    ) {
        (Some(up), Some(down)) if up != down => Some(up > down),
        (Some(up), _) => Some(up > Decimal::new(5, 1)),
        (_, Some(down)) => Some(down < Decimal::new(5, 1)),
        _ => None,
    }
}

async fn load_event_settlement_prices(
    pool: &PgPool,
    event_ids: &[String],
) -> Result<HashMap<(String, String), Decimal>, sqlx::Error> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<(Option<String>, String, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT market_slug, token_id, settled_price
        FROM pm_token_settlements
        WHERE market_slug = ANY($1)
          AND resolved = TRUE
        "#,
    )
    .bind(event_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut prices = HashMap::new();
    for (market_slug, token_id, settled_price) in rows {
        let Some(market_slug) = market_slug else {
            continue;
        };
        let Some(settled_price) = settled_price else {
            continue;
        };
        prices.insert((market_slug, normalize_token_id(&token_id)), settled_price);
    }

    Ok(prices)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::collections::HashMap;

    use super::{
        EventMetadataRow, MarketUpdate, build_event_updates, l2_updates_from_book, near_depth,
    };

    #[test]
    fn official_settlement_backfill_repairs_event_expiry_outcome() {
        let now = Utc::now();
        let row = EventMetadataRow {
            market_slug: "evt-1".into(),
            symbol: Some("BTCUSDT".into()),
            end_time: Some(now + Duration::minutes(5)),
            start_time: Some(now),
            up_token_id: Some("up-1".into()),
            down_token_id: Some("down-1".into()),
            price_to_beat: Some(dec!(100000)),
        };

        let initial = build_event_updates(vec![row.clone()], &HashMap::new(), false);
        assert!(matches!(
            initial.last(),
            Some(MarketUpdate::EventExpired {
                resolved_up_won: None,
                ..
            })
        ));

        let mut settlement_prices = HashMap::new();
        settlement_prices.insert(("evt-1".to_string(), "up-1".to_string()), dec!(1));
        settlement_prices.insert(("evt-1".to_string(), "down-1".to_string()), dec!(0));

        let repaired = build_event_updates(vec![row], &settlement_prices, false);
        assert!(matches!(
            repaired.last(),
            Some(MarketUpdate::EventExpired {
                resolved_up_won: Some(true),
                ..
            })
        ));
    }

    #[test]
    fn official_only_backtest_skips_unresolved_events() {
        let now = Utc::now();
        let row = EventMetadataRow {
            market_slug: "evt-1".into(),
            symbol: Some("BTCUSDT".into()),
            end_time: Some(now + Duration::minutes(5)),
            start_time: Some(now),
            up_token_id: Some("up-1".into()),
            down_token_id: Some("down-1".into()),
            price_to_beat: Some(dec!(100000)),
        };

        let updates = build_event_updates(vec![row], &HashMap::new(), true);
        assert!(
            updates.is_empty(),
            "official-only mode should skip unresolved events"
        );
    }

    #[test]
    fn near_depth_sums_volume_within_mid_range_for_array_pairs() {
        let bids = json!([["100.0", "5.0"], ["99.95", "3.0"], ["99.85", "10.0"]]);
        let asks = json!([["100.05", "4.0"], ["100.10", "2.0"], ["100.25", "7.0"]]);

        let (bid_near, ask_near) = near_depth(&bids, &asks, 100.0, 0.001);

        assert!((bid_near - 8.0).abs() < 1e-9, "bid_near={bid_near}");
        assert!((ask_near - 6.0).abs() < 1e-9, "ask_near={ask_near}");
    }

    #[test]
    fn near_depth_supports_object_levels_and_l2_depth_variant() {
        let ts = Utc::now();
        let bids = json!([
            {"price": "100.0", "size": "1.5"},
            {"price": "99.91", "size": "2.5"},
            {"price": "99.70", "size": "9.0"}
        ]);
        let asks = json!([
            {"price": "100.03", "size": "4.5"},
            {"price": "100.09", "size": "1.0"},
            {"price": "100.30", "size": "8.0"}
        ]);

        let updates = l2_updates_from_book(
            "BTCUSDT",
            0.125,
            9,
            dec!(100.0),
            Some(&bids),
            Some(&asks),
            ts,
        );

        assert!(matches!(
            updates.first(),
            Some(MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts: update_ts
            }) if symbol.as_ref() == "BTCUSDT" && (obi - 0.125).abs() < f64::EPSILON && *spread_bps == 9 && *update_ts == ts
        ));

        assert!(matches!(
            updates.get(1),
            Some(MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ts: update_ts
            }) if symbol.as_ref() == "BTCUSDT"
                && (obi - 0.125).abs() < f64::EPSILON
                && *spread_bps == 9
                && (bid_depth_near - 4.0).abs() < 1e-9
                && (ask_depth_near - 5.5).abs() < 1e-9
                && *update_ts == ts
        ));
    }
}
