use super::super::{MarketUpdate, UpdateType};
use super::{
    symbol_filter,
    token_mappings::{TokenMappings, infer_symbol_from_slug},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

pub(super) async fn load_event_updates(
    pool: &PgPool,
    updates: &mut Vec<MarketUpdate>,
    spot_series: &HashMap<String, Vec<(DateTime<Utc>, Decimal)>>,
    mappings: &mut TokenMappings,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    sync_records_exists: bool,
    pm_market_metadata_exists: bool,
    pm_token_settlements_exists: bool,
) {
    let mut event_rows: Vec<(
        String,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        Option<Decimal>,
    )> = if pm_market_metadata_exists {
        sqlx::query_as(
            r#"
                SELECT market_slug, symbol, start_time, end_time, price_to_beat
                FROM pm_market_metadata
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR end_time >= $2)
                  AND ($3::timestamptz IS NULL OR start_time <= $3)
                ORDER BY start_time
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    if event_rows.is_empty() && pm_token_settlements_exists {
        let raw_rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
            r#"
                SELECT DISTINCT ON (market_slug) market_slug, raw_market
                FROM pm_token_settlements
                WHERE raw_market IS NOT NULL
                  AND market_slug IS NOT NULL
                  AND market_slug != ''
                  AND ($1::timestamptz IS NULL OR fetched_at >= $1)
                  AND ($2::timestamptz IS NULL OR fetched_at <= $2)
                ORDER BY market_slug, fetched_at DESC
                "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for (slug, raw) in raw_rows {
            let start_time =
                parse_market_datetime(&raw, &["eventStartTime", "startDate", "start_date"]);
            let end_time = parse_market_datetime(&raw, &["endDate", "end_date"]);

            let price_to_beat = raw
                .get("groupItemThreshold")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Decimal>().ok())
                .or_else(|| {
                    let upper = raw
                        .get("upperBound")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO);
                    let lower = raw
                        .get("lowerBound")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO);
                    let mid = (upper + lower) / Decimal::from(2);
                    if mid > Decimal::ZERO { Some(mid) } else { None }
                });

            let symbol = infer_symbol_from_slug(&slug);
            event_rows.push((slug, symbol, start_time, end_time, price_to_beat));
        }
        info!(
            "Derived {} event windows from pm_token_settlements.raw_market",
            event_rows.len()
        );
    }

    let mut event_open_count = 0usize;
    for (slug, sym, start_time, end_time, price_to_beat) in &event_rows {
        let (Some(st), Some(end)) = (*start_time, *end_time) else {
            continue;
        };

        let corrected_end = corrected_window_end(slug, st, end);
        let symbol = sym
            .clone()
            .or_else(|| mappings.slug_to_symbol.get(slug.as_str()).cloned())
            .or_else(|| infer_symbol_from_slug(slug))
            .unwrap_or_default();

        if symbol.is_empty() {
            continue;
        }

        let s0 = match price_to_beat {
            Some(p) if *p > Decimal::ZERO => Some(*p),
            _ => spot_series
                .get(symbol.as_str())
                .and_then(|series| spot_at_or_before(series, st)),
        };

        let Some(s0) = s0 else { continue };

        if !mappings.slug_to_symbol.contains_key(slug) {
            mappings.slug_to_symbol.insert(slug.clone(), symbol.clone());
        }
        updates.push(MarketUpdate {
            timestamp: st,
            symbol,
            update_type: UpdateType::EventState {
                event_slug: slug.clone(),
                end_time: Some(corrected_end),
                price_to_beat: Some(s0),
                outcome: None,
            },
        });
        event_open_count += 1;
    }
    info!(
        "Loaded {} event window rows (pm_market_metadata + derived), emitted {} EventState opens",
        event_rows.len(),
        event_open_count
    );

    if event_open_count == 0 && sync_records_exists {
        let rows: anyhow::Result<Vec<(String, String, DateTime<Utc>)>> = sqlx::query_as(
            r#"
                SELECT DISTINCT ON (pm_market_slug)
                    pm_market_slug,
                    symbol,
                    timestamp
                FROM sync_records
                WHERE pm_market_slug IS NOT NULL
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY pm_market_slug, timestamp ASC
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .map_err(Into::into);

        match rows {
            Ok(slugs) => {
                for (slug, sym, st) in slugs {
                    let Some(duration_secs) = infer_window_duration_secs(&slug) else {
                        continue;
                    };

                    let s0 = spot_series
                        .get(sym.as_str())
                        .and_then(|series| spot_at_or_before(series, st));
                    let Some(s0) = s0 else { continue };

                    let end = st + chrono::Duration::seconds(duration_secs);
                    if !mappings.slug_to_symbol.contains_key(&slug) {
                        mappings.slug_to_symbol.insert(slug.clone(), sym.clone());
                    }
                    updates.push(MarketUpdate {
                        timestamp: st,
                        symbol: sym,
                        update_type: UpdateType::EventState {
                            event_slug: slug,
                            end_time: Some(end),
                            price_to_beat: Some(s0),
                            outcome: None,
                        },
                    });
                    event_open_count += 1;
                }
                if event_open_count > 0 {
                    info!(
                        "Derived {} event windows from sync_records (no pm_market_metadata rows)",
                        event_open_count
                    );
                }
            }
            Err(e) => {
                info!("sync_records window-derivation query failed: {e}");
            }
        }
    }

    let settlement_rows: Vec<(String, String, Decimal, Option<DateTime<Utc>>)> =
        if pm_token_settlements_exists {
            sqlx::query_as(
                r#"
                SELECT market_slug, outcome, settled_price, resolved_at
                FROM pm_token_settlements
                WHERE resolved = true
                  AND LOWER(outcome) = 'up'
                  AND ($1::timestamptz IS NULL OR resolved_at >= $1)
                  AND ($2::timestamptz IS NULL OR resolved_at <= $2)
                ORDER BY resolved_at
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

    for (slug, _outcome, settled_price, resolved_at) in &settlement_rows {
        if let Some(rat) = resolved_at {
            let symbol = mappings
                .slug_to_symbol
                .get(slug.as_str())
                .cloned()
                .or_else(|| infer_symbol_from_slug(slug))
                .unwrap_or_default();
            if symbol.is_empty() {
                continue;
            }
            updates.push(MarketUpdate {
                timestamp: *rat,
                symbol,
                update_type: UpdateType::EventState {
                    event_slug: slug.clone(),
                    end_time: None,
                    price_to_beat: None,
                    outcome: Some(*settled_price == Decimal::ONE),
                },
            });
        }
    }
    info!("Loaded {} settlement records", settlement_rows.len());
}

fn infer_window_duration_secs(slug: &str) -> Option<i64> {
    let s = slug.to_ascii_lowercase();
    if s.contains("15m") {
        return Some(900);
    }
    if s.contains("5m") {
        return Some(300);
    }
    if s.contains("60m") {
        return Some(3600);
    }
    None
}

fn spot_at_or_before(series: &[(DateTime<Utc>, Decimal)], ts: DateTime<Utc>) -> Option<Decimal> {
    if series.is_empty() {
        return None;
    }
    match series.binary_search_by_key(&ts, |(t, _)| *t) {
        Ok(i) => Some(series[i].1),
        Err(0) => None,
        Err(i) => Some(series[i - 1].1),
    }
}

fn parse_market_datetime(raw: &serde_json::Value, keys: &[&str]) -> Option<DateTime<Utc>> {
    keys.iter().find_map(|key| {
        raw.get(*key)
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    })
}

pub(super) fn corrected_window_end(
    slug: &str,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
) -> DateTime<Utc> {
    let duration_from_slug = if slug.contains("-5m-") {
        Some(chrono::Duration::seconds(300))
    } else if slug.contains("-15m-") {
        Some(chrono::Duration::seconds(900))
    } else {
        None
    };

    if let Some(dur) = duration_from_slug {
        let expected_end = start_time + dur;
        if (end_time - start_time) > dur * 2 {
            expected_end
        } else {
            end_time
        }
    } else {
        end_time
    }
}
