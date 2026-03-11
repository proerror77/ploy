use super::super::{MarketUpdate, UpdateType};
use super::token_mappings::TokenMappings;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::info;

pub(super) async fn load_lob_updates(
    pool: &PgPool,
    updates: &mut Vec<MarketUpdate>,
    mappings: &TokenMappings,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    lob_snaps_exists: bool,
) {
    let known_token_ids = mappings.known_token_ids();
    let lob_rows: Vec<(DateTime<Utc>, String, Option<serde_json::Value>)> = if lob_snaps_exists
        && !known_token_ids.is_empty()
    {
        sqlx::query_as(
            r#"
                    SELECT DISTINCT ON (
                        (EXTRACT(EPOCH FROM received_at)::bigint / 5),
                        token_id
                    )
                        received_at, token_id, asks
                    FROM clob_orderbook_snapshots
                    WHERE ($1::timestamptz IS NULL OR received_at >= $1)
                      AND ($2::timestamptz IS NULL OR received_at <= $2)
                      AND token_id = ANY($3)
                      AND jsonb_array_length(asks) > 0
                    ORDER BY (EXTRACT(EPOCH FROM received_at)::bigint / 5), token_id, received_at DESC
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

    let mut lob_count = 0u64;
    for (ts, token_id, asks_json) in &lob_rows {
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

        let (total_depth, best_ask_price) = match asks_json {
            Some(arr) if arr.is_array() => {
                let levels = arr.as_array().unwrap();
                let mut depth = 0.0f64;
                let mut best = None;
                for level in levels {
                    if let (Some(size_str), Some(price_str)) = (
                        level.get("size").and_then(|v| v.as_str()),
                        level.get("price").and_then(|v| v.as_str()),
                    ) {
                        if let Ok(size) = size_str.parse::<f64>() {
                            depth += size;
                        }
                        if best.is_none() {
                            if let Ok(p) = price_str.parse::<Decimal>() {
                                best = Some(p);
                            }
                        }
                    }
                }
                (depth as u64, best)
            }
            _ => continue,
        };

        if total_depth == 0 {
            continue;
        }

        updates.push(MarketUpdate {
            timestamp: *ts,
            symbol,
            update_type: UpdateType::LobSnapshot {
                side: "BOTH".to_string(),
                ask_depth_shares: total_depth,
                best_ask: best_ask_price,
            },
        });
        lob_count += 1;
    }
    info!(
        "Loaded {} LOB snapshots ({} mapped to symbols)",
        lob_rows.len(),
        lob_count
    );
}
