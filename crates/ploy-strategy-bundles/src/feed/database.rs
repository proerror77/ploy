//! Database-backed historical feed loader.
//!
//! Loads market data from PostgreSQL into a `Vec<MarketUpdate>` that
//! can be wrapped in [`HistoricalFeed`] for backtesting.
//!
//! Gated behind the `database` feature flag to keep the crate
//! lightweight when only synthetic data or live feeds are needed.
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
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

use crate::traits::MarketUpdate;

/// How far before `from` to load spot prices for EWMA volatility warm-up.
const WARMUP_MINUTES: i64 = 30;

/// Historical research backtests only trust canonical historical PM quote captures.
///
/// `ploy_runner_live` is a synthetic midpoint feed for live/dry-run operation, and
/// the existing `polymarket_ws_collector` history before the next validated cutover
/// is too polluted with placeholder `0.01/0.99` books to mix into research results.
/// Dry-run parity should use recorded replay mode instead of the historical DB path.
const TRUSTED_PM_RESEARCH_QUOTE_SOURCES: &[&str] = &[
    "polymarket_ws",
    "polymarket_ws_collector",
    "ploy_runner_live",
];

/// Additive historical-loader flags for non-crypto datasets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoricalLoadOptions {
    pub include_reference_prices: bool,
    pub reference_symbols: Vec<String>,
    pub include_sports_state: bool,
}

impl HistoricalLoadOptions {
    #[must_use]
    pub fn normalized_reference_symbols(&self) -> Vec<String> {
        self.reference_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}

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
    load_spot_prices(pool, symbols, spot_from, to, &mut updates).await?;

    // 1b. Aggregated trade flow — additive signal stream for roadmap work.
    load_agg_trades(pool, symbols, from, to, &mut updates).await?;

    // 2. Event windows FIRST — must be registered before quotes are processed.
    //    Quotes for a token can arrive before the event's official start_time
    //    (Polymarket tokens are continuous; ~90% of quotes precede start_time).
    //    Loading events before quotes ensures token_symbol mappings exist.
    load_events(pool, symbols, from, to, &mut updates).await?;

    // 3. Polymarket quotes — try clob_quote_ticks first, fall back to orderbook snapshots
    let token_map = load_token_mappings(pool, symbols, from, to).await?;
    let quote_count_before = updates.len();
    load_pm_quotes(pool, &token_map, from, to, &mut updates).await?;
    let quote_count_after = updates.len();

    // If clob_quote_ticks had no real data, try extracting mid prices from
    // clob_orderbook_snapshots (which stores full bid/ask depth as JSONB).
    if quote_count_after == quote_count_before {
        info!("No real quotes in clob_quote_ticks, falling back to orderbook snapshots");
        load_pm_quotes_from_snapshots(pool, &token_map, from, to, &mut updates).await?;
    }

    // 4. L2 orderbook from binance_lob_ticks
    load_l2_data(pool, symbols, from, to, &mut updates).await?;

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
    updates.sort_by_key(|u| update_ts(u));

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

fn update_ts(u: &MarketUpdate) -> DateTime<Utc> {
    match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::AggTrade { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered {
            end_time,
            window_secs,
            ..
        } => {
            // Sort EventDiscovered before all quotes for the same event.
            // Quotes can arrive before the event's official start_time (~90% of cases),
            // so we subtract an extra buffer to guarantee ordering.
            *end_time - chrono::Duration::seconds(*window_secs as i64) - chrono::Duration::hours(1)
        }
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}

fn normalize_token_id(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return hex_to_decimal_string(hex).unwrap_or_else(|| trimmed.to_string());
    }
    trimmed.to_string()
}

fn hex_to_decimal_string(hex: &str) -> Option<String> {
    if hex.is_empty() {
        return None;
    }

    let mut digits = vec![0_u8];

    for ch in hex.chars() {
        let value = ch.to_digit(16)? as u32;
        let mut carry = value;

        for digit in &mut digits {
            let next = (*digit as u32) * 16 + carry;
            *digit = (next % 10) as u8;
            carry = next / 10;
        }

        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }

    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }

    Some(
        digits
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + *digit))
            .collect(),
    )
}

// ── Spot Prices ──────────────────────────────────────────

async fn load_spot_prices(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    // Try sync_records first (has bn_mid_price)
    let rows: Vec<(DateTime<Utc>, String, Decimal)> = sqlx::query_as(
        r#"
        SELECT timestamp, symbol, bn_mid_price
        FROM sync_records
        WHERE symbol = ANY($1)
          AND timestamp >= $2
          AND timestamp <= $3
        ORDER BY timestamp
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if !rows.is_empty() {
        info!(count = rows.len(), "Loaded spot prices from sync_records");
        for (ts, symbol, price) in rows {
            updates.push(MarketUpdate::SpotPrice { symbol, price, ts });
        }
        return Ok(());
    }

    // Fallback: binance_price_ticks
    let rows: Vec<(DateTime<Utc>, String, Decimal)> = sqlx::query_as(
        r#"
        SELECT trade_time, symbol, price
        FROM binance_price_ticks
        WHERE symbol = ANY($1)
          AND trade_time >= $2
          AND trade_time <= $3
        ORDER BY trade_time
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
        "Loaded spot prices from binance_price_ticks"
    );
    for (ts, symbol, price) in rows {
        updates.push(MarketUpdate::SpotPrice { symbol, price, ts });
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
    let rows: Vec<(DateTime<Utc>, String, i64, Decimal, Decimal, bool)> = sqlx::query_as(
        r#"
        SELECT trade_time, symbol, agg_trade_id, price, quantity, is_buyer_maker
        FROM binance_agg_trade_ticks
        WHERE symbol = ANY($1)
          AND trade_time >= $2
          AND trade_time <= $3
        ORDER BY trade_time, agg_trade_id
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(count = rows.len(), "Loaded agg trades from binance_agg_trade_ticks");
    for (ts, symbol, agg_trade_id, price, quantity, is_buyer_maker) in rows {
        updates.push(MarketUpdate::AggTrade {
            symbol,
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
            symbol: row.symbol,
            source: row.source,
            asset_class: row.asset_class,
            price: row.price,
            full_accuracy_value: row.full_accuracy_value,
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
            game_id: row.game_id,
            league: row.league,
            slug: row.slug,
            home_team: row.home_team,
            away_team: row.away_team,
            status: row.status,
            period: row.period,
            score: row.score,
            elapsed: row.elapsed,
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
) -> Result<(), sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(());
    }
    let trusted_sources: Vec<&str> = TRUSTED_PM_RESEARCH_QUOTE_SOURCES.to_vec();

    // Polymarket UP/DOWN token structure:
    // - UP token:   best_bid is the real price, best_ask may be NULL
    // - DOWN token: best_ask is the real price, best_bid may be NULL
    // Accept rows where EITHER bid OR ask is in the real price range (0.01, 0.99).
    let rows: Vec<(DateTime<Utc>, String, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at, token_id, best_bid, best_ask
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
        ORDER BY date_trunc('second', received_at), token_id, received_at DESC
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .bind(&trusted_sources)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        sources = ?TRUSTED_PM_RESEARCH_QUOTE_SOURCES,
        "Loaded PM quotes from clob_quote_ticks (trusted sources, filtered: bid/ask in 0.02-0.98)"
    );
    for (ts, token_id, bid, ask) in rows {
        updates.push(MarketUpdate::Quote {
            token_id,
            bid,
            ask,
            ts,
        });
    }

    Ok(())
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
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(());
    }

    // For UP tokens: best_bid = MAX(bids), best_ask = best_bid + 0.01 (synthetic)
    // For DOWN tokens: best_ask = MIN(asks), best_bid = best_ask - 0.01 (synthetic)
    // Filter: only include prices in (0.01, 0.99) — exclude the extreme placeholders
    // at exactly 0.01 and 0.99 which represent the full-book sentinel orders.
    let rows: Vec<(DateTime<Utc>, String, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at,
               token_id,
               -- Best bid: highest bid price strictly between 0.01 and 0.99
               (
                   SELECT MAX((elem->>'price')::numeric)
                   FROM jsonb_array_elements(bids) AS elem
                   WHERE (elem->>'price')::numeric > 0.01
                     AND (elem->>'price')::numeric < 0.99
               ) AS best_bid,
               -- Best ask: lowest ask price strictly between 0.01 and 0.99
               (
                   SELECT MIN((elem->>'price')::numeric)
                   FROM jsonb_array_elements(asks) AS elem
                   WHERE (elem->>'price')::numeric > 0.01
                     AND (elem->>'price')::numeric < 0.99
               ) AS best_ask
        FROM clob_orderbook_snapshots
        WHERE received_at >= $1
          AND received_at <= $2
          AND token_id = ANY($3)
        HAVING (
            SELECT MAX((elem->>'price')::numeric)
            FROM jsonb_array_elements(bids) AS elem
            WHERE (elem->>'price')::numeric > 0.01
              AND (elem->>'price')::numeric < 0.99
        ) IS NOT NULL
        OR (
            SELECT MIN((elem->>'price')::numeric)
            FROM jsonb_array_elements(asks) AS elem
            WHERE (elem->>'price')::numeric > 0.01
              AND (elem->>'price')::numeric < 0.99
        ) IS NOT NULL
        ORDER BY date_trunc('second', received_at), token_id, received_at DESC
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        "Loaded PM quotes from clob_orderbook_snapshots (real bid/ask extracted from JSONB depth)"
    );
    for (ts, token_id, bid, ask) in rows {
        updates.push(MarketUpdate::Quote {
            token_id,
            bid,
            ask,
            ts,
        });
    }

    Ok(())
}

// ── L2 Data ──────────────────────────────────────────────

async fn load_l2_data(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    let rows: Vec<(DateTime<Utc>, String, f64, i32)> = sqlx::query_as(
        r#"
        SELECT event_time, symbol,
               COALESCE(obi_5, 0.0) as obi,
               COALESCE(spread_bps, 0) as spread_bps
        FROM binance_lob_ticks
        WHERE symbol = ANY($1)
          AND event_time >= $2
          AND event_time <= $3
        ORDER BY event_time
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(count = rows.len(), "Loaded L2 data from binance_lob_ticks");
    for (ts, symbol, obi, spread_bps) in rows {
        updates.push(MarketUpdate::L2 {
            symbol,
            obi,
            spread_bps: spread_bps as u32,
            ts,
        });
    }

    Ok(())
}

// ── Events ───────────────────────────────────────────────

async fn load_events(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
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
    updates.extend(build_event_updates(rows, &settlement_prices));

    Ok(())
}

fn build_event_updates(
    rows: Vec<EventMetadataRow>,
    settlement_prices: &HashMap<(String, String), Decimal>,
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

        let up_token = normalize_token_id(&row.up_token_id.unwrap_or_default());
        let down_token = normalize_token_id(&row.down_token_id.unwrap_or_default());
        let resolved_up_won = resolve_up_won_from_settlements(
            settlement_prices,
            &row.market_slug,
            &up_token,
            &down_token,
        );
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        if symbol.is_empty() || up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        updates.push(MarketUpdate::EventDiscovered {
            event_id: row.market_slug.clone(),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat: row.price_to_beat,
            resolved_up_won: None,
        });
        updates.push(MarketUpdate::EventExpired {
            event_id: row.market_slug,
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
    use std::collections::HashMap;

    use super::{
        EventMetadataRow, MarketUpdate, build_event_updates, hex_to_decimal_string,
        normalize_token_id,
    };

    #[test]
    fn normalize_token_id_converts_hex_to_decimal() {
        let raw = "\"0x3c38c18444ab803acea0d4de7bcdecae7f0f8ddbcd0466e3323d1cb9e04b6f5d\"";
        let normalized = normalize_token_id(raw);
        assert_eq!(
            normalized,
            "27239049953613250678046988034203198692578441444398010699401021233149338414941"
        );
    }

    #[test]
    fn normalize_token_id_keeps_decimal_ids() {
        let raw = "35165169860573247111698076491591023728797123337726915178028774493274622598566";
        assert_eq!(normalize_token_id(raw), raw);
    }

    #[test]
    fn hex_to_decimal_string_rejects_invalid_hex() {
        assert_eq!(hex_to_decimal_string("xyz"), None);
    }

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

        let initial = build_event_updates(vec![row.clone()], &HashMap::new());
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

        let repaired = build_event_updates(vec![row], &settlement_prices);
        assert!(matches!(
            repaired.last(),
            Some(MarketUpdate::EventExpired {
                resolved_up_won: Some(true),
                ..
            })
        ));
    }
}
