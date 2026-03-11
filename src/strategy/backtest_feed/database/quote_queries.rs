use super::super::{MarketUpdate, UpdateType};
use super::{symbol_filter, token_mappings::TokenMappings};
use crate::domain::Side;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::info;

pub(super) async fn load_quote_updates(
    pool: &PgPool,
    updates: &mut Vec<MarketUpdate>,
    mappings: &TokenMappings,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    sync_records_exists: bool,
    quote_ticks_exists: bool,
) {
    let known_token_ids = mappings.known_token_ids();
    let quote_rows: Vec<(
        DateTime<Utc>,
        String,
        String,
        Option<Decimal>,
        Option<Decimal>,
    )> = if quote_ticks_exists && !known_token_ids.is_empty() {
        sqlx::query_as(
            r#"
                    SELECT DISTINCT ON (date_trunc('second', received_at), token_id, side)
                           received_at, token_id, side, best_bid, best_ask
                    FROM clob_quote_ticks
                    WHERE ($1::timestamptz IS NULL OR received_at >= $1)
                      AND ($2::timestamptz IS NULL OR received_at <= $2)
                      AND token_id = ANY($3)
                    ORDER BY date_trunc('second', received_at), token_id, side, received_at DESC
                    "#,
        )
        .bind(from)
        .bind(to)
        .bind(&known_token_ids)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    for (ts, token_id, side, best_bid, best_ask) in &quote_rows {
        let event_slug = match mappings.token_to_slug.get(token_id.as_str()) {
            Some(s) => s.clone(),
            None => continue,
        };
        let Some(symbol) = mappings.resolve_symbol(token_id, &event_slug) else {
            continue;
        };
        if symbol.is_empty() {
            continue;
        }
        let side = match side.as_str() {
            "UP" => Side::Up,
            "DOWN" => Side::Down,
            _ => continue,
        };

        updates.push(MarketUpdate {
            timestamp: *ts,
            symbol,
            update_type: UpdateType::PmQuote {
                event_slug,
                token_id: token_id.clone(),
                side,
                best_bid: *best_bid,
                best_ask: *best_ask,
            },
        });
    }
    info!(
        "Loaded {} quote ticks (pre-filtered to {} known tokens)",
        quote_rows.len(),
        known_token_ids.len()
    );

    if quote_rows.is_empty() && sync_records_exists {
        let sync_quote_rows: anyhow::Result<
            Vec<(
                DateTime<Utc>,
                String,
                String,
                Option<Decimal>,
                Option<Decimal>,
                Option<String>,
                Option<String>,
            )>,
        > = sqlx::query_as(
            r#"
                SELECT DISTINCT ON (date_trunc('second', timestamp), pm_market_slug)
                    timestamp,
                    symbol,
                    pm_market_slug,
                    pm_yes_price,
                    pm_no_price,
                    pm_yes_token_id,
                    pm_no_token_id
                FROM sync_records
                WHERE pm_market_slug IS NOT NULL
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY date_trunc('second', timestamp), pm_market_slug, timestamp DESC
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .map_err(Into::into);

        match sync_quote_rows {
            Ok(rows) => {
                let row_count = rows.len();
                for (ts, sym, slug, yes, no, yes_token_id, no_token_id) in rows {
                    if let Some(ask) = yes {
                        updates.push(MarketUpdate {
                            timestamp: ts,
                            symbol: sym.clone(),
                            update_type: UpdateType::PmQuote {
                                event_slug: slug.clone(),
                                token_id: yes_token_id.unwrap_or_else(|| format!("{}:UP", slug)),
                                side: Side::Up,
                                best_bid: None,
                                best_ask: Some(ask),
                            },
                        });
                    }
                    if let Some(ask) = no {
                        updates.push(MarketUpdate {
                            timestamp: ts,
                            symbol: sym.clone(),
                            update_type: UpdateType::PmQuote {
                                event_slug: slug.clone(),
                                token_id: no_token_id.unwrap_or_else(|| format!("{}:DOWN", slug)),
                                side: Side::Down,
                                best_bid: None,
                                best_ask: Some(ask),
                            },
                        });
                    }
                }
                info!(
                    "Supplemented with {} PM quotes from sync_records",
                    row_count
                );
            }
            Err(e) => {
                info!("sync_records PM quote replay query failed: {e}");
            }
        }
    }
}
