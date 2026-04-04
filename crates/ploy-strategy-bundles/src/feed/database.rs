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
const TRUSTED_PM_RESEARCH_QUOTE_SOURCES: &[&str] = &["polymarket_ws"];

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
    let mut updates: Vec<MarketUpdate> = Vec::new();

    // 1. Spot prices — load earlier for EWMA warm-up
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);
    load_spot_prices(pool, symbols, spot_from, to, &mut updates).await?;

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

    // Sort by timestamp
    updates.sort_by_key(|u| update_ts(u));

    info!(
        count = updates.len(),
        symbols = ?symbols,
        from = %from,
        to = %to,
        "Loaded historical data from database",
    );

    Ok(updates)
}

fn update_ts(u: &MarketUpdate) -> DateTime<Utc> {
    match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
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

    // Filter out degenerate quotes (bid < 0.02 or ask > 0.98).
    // Polymarket CLOB orderbooks often have extreme placeholder orders
    // (bid=0.01, ask=0.99) when there is no real liquidity. These are
    // useless for strategy evaluation — the real market price is the midpoint.
    // Only load quotes where both bid and ask are in a tradeable range from
    // trusted historical capture sources.
    let rows: Vec<(DateTime<Utc>, String, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at, token_id, best_bid, best_ask
        FROM clob_quote_ticks
        WHERE received_at >= $1
          AND received_at <= $2
          AND token_id = ANY($3)
          AND source = ANY($4)
          AND best_bid  IS NOT NULL AND best_bid  > 0.02 AND best_bid  < 0.98
          AND best_ask  IS NOT NULL AND best_ask  > 0.02 AND best_ask  < 0.98
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
/// Extracts the innermost real bid/ask from the JSONB depth arrays.
/// Used when `clob_quote_ticks` has no real data (all 0.01/0.99 placeholders).
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

    // Extract the best real bid/ask from JSONB depth arrays.
    // "Real" means price is in (0.02, 0.98) — not a placeholder order.
    // We compute mid = (best_bid + best_ask) / 2 and apply a 0.5% synthetic spread.
    let rows: Vec<(DateTime<Utc>, String, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at,
               token_id,
               -- Best real bid: highest bid price in (0.02, 0.98)
               (
                   SELECT MAX((elem->>'price')::numeric)
                   FROM jsonb_array_elements(bids) AS elem
                   WHERE (elem->>'price')::numeric > 0.02
                     AND (elem->>'price')::numeric < 0.98
               ) AS best_bid,
               -- Best real ask: lowest ask price in (0.02, 0.98)
               (
                   SELECT MIN((elem->>'price')::numeric)
                   FROM jsonb_array_elements(asks) AS elem
                   WHERE (elem->>'price')::numeric > 0.02
                     AND (elem->>'price')::numeric < 0.98
               ) AS best_ask
        FROM clob_orderbook_snapshots
        WHERE received_at >= $1
          AND received_at <= $2
          AND token_id = ANY($3)
        HAVING (
            SELECT MAX((elem->>'price')::numeric)
            FROM jsonb_array_elements(bids) AS elem
            WHERE (elem->>'price')::numeric > 0.02
              AND (elem->>'price')::numeric < 0.98
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
        SELECT received_at, symbol,
               COALESCE(obi_5, 0.0) as obi,
               COALESCE(spread_bps, 0) as spread_bps
        FROM binance_lob_ticks
        WHERE symbol = ANY($1)
          AND received_at >= $2
          AND received_at <= $3
        ORDER BY received_at
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
    let rows: Vec<(
        String,                // market_slug (event_id)
        Option<String>,        // symbol
        Option<DateTime<Utc>>, // end_time
        Option<DateTime<Utc>>, // start_time
        Option<String>,        // up_token_id (first element)
        Option<String>,        // down_token_id (second element)
        Option<Decimal>,       // price_to_beat
    )> = sqlx::query_as(
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

    let event_ids: Vec<String> = rows.iter().map(|(event_id, ..)| event_id.clone()).collect();
    let settlement_prices = load_event_settlement_prices(pool, &event_ids).await?;

    info!(count = rows.len(), "Loaded events from pm_market_metadata");
    for (event_id, symbol, end_time, start_time, up_token, down_token, price_to_beat) in rows {
        let symbol = symbol.unwrap_or_default();
        let end_time = match end_time {
            Some(t) => t,
            None => continue,
        };
        let start_time = match start_time {
            Some(t) => t,
            None => continue,
        };

        let up_token = normalize_token_id(&up_token.unwrap_or_default());
        let down_token = normalize_token_id(&down_token.unwrap_or_default());

        // Compute settlement outcome for EventExpired (NOT EventDiscovered — no lookahead).
        let resolved_up_won = match (
            settlement_prices
                .get(&(event_id.clone(), up_token.clone()))
                .copied(),
            settlement_prices
                .get(&(event_id.clone(), down_token.clone()))
                .copied(),
        ) {
            (Some(up), Some(down)) if up != down => Some(up > down),
            (Some(up), _) => Some(up > Decimal::new(5, 1)),
            (_, Some(down)) => Some(down < Decimal::new(5, 1)),
            _ => None,
        };

        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        if symbol.is_empty() || up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        updates.push(MarketUpdate::EventDiscovered {
            event_id: event_id.clone(),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat,
            // Never set resolved_up_won here — that would be lookahead bias.
            // Settlement is only known after expiry; see EventExpired below.
            resolved_up_won: None,
        });

        // EventExpired carries the settlement outcome so the strategy can settle
        // positions correctly without having seen the result at discovery time.
        updates.push(MarketUpdate::EventExpired {
            event_id,
            end_time,
            resolved_up_won,
        });
    }

    Ok(())
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
    use super::{hex_to_decimal_string, normalize_token_id};

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
}
