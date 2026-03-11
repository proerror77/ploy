use super::{BookAskLevel, MarketUpdate, UpdateType};
use crate::domain::Side;
use alloy::primitives::U256;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

pub(super) async fn load_database_updates(
    pool: &PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Vec<MarketUpdate>> {
    let mut updates: Vec<MarketUpdate> = Vec::new();
    let mut spot_series: HashMap<String, Vec<(DateTime<Utc>, Decimal)>> = HashMap::new();

    let sync_records_exists: bool = table_exists(pool, "public.sync_records")
        .await
        .unwrap_or(false);
    let price_ticks_exists: bool = table_exists(pool, "public.binance_price_ticks")
        .await
        .unwrap_or(false);
    let klines_exists: bool = table_exists(pool, "public.binance_klines")
        .await
        .unwrap_or(false);
    let quote_ticks_exists: bool = table_exists(pool, "public.clob_quote_ticks")
        .await
        .unwrap_or(false);
    let lob_snaps_exists: bool = table_exists(pool, "public.clob_orderbook_snapshots")
        .await
        .unwrap_or(false);
    let binance_lob_ticks_exists: bool = table_exists(pool, "public.binance_lob_ticks")
        .await
        .unwrap_or(false);
    let pm_market_metadata_exists: bool = table_exists(pool, "public.pm_market_metadata")
        .await
        .unwrap_or(false);
    let pm_token_settlements_exists: bool = table_exists(pool, "public.pm_token_settlements")
        .await
        .unwrap_or(false);

    let spot_rows: Vec<(DateTime<Utc>, String, Decimal)> = if sync_records_exists {
        sqlx::query_as(
            r#"
                SELECT timestamp, symbol, bn_mid_price
                FROM sync_records
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR timestamp >= $2)
                  AND ($3::timestamptz IS NULL OR timestamp <= $3)
                ORDER BY timestamp
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };

    if !spot_rows.is_empty() {
        for (ts, sym, price) in &spot_rows {
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: sym.clone(),
                update_type: UpdateType::SpotTrade {
                    price: *price,
                    quantity: None,
                },
            });
            spot_series
                .entry(sym.clone())
                .or_default()
                .push((*ts, *price));
        }
        info!("Loaded {} spot records from sync_records", spot_rows.len());
    } else if price_ticks_exists {
        let price_rows: Vec<(DateTime<Utc>, String, Decimal, Option<Decimal>)> = sqlx::query_as(
            r#"
                SELECT trade_time, symbol, price, quantity
                FROM binance_price_ticks
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR trade_time >= $2)
                  AND ($3::timestamptz IS NULL OR trade_time <= $3)
                ORDER BY trade_time
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?;

        for (ts, sym, price, qty) in &price_rows {
            updates.push(MarketUpdate {
                timestamp: *ts,
                symbol: sym.clone(),
                update_type: UpdateType::SpotTrade {
                    price: *price,
                    quantity: *qty,
                },
            });
            spot_series
                .entry(sym.clone())
                .or_default()
                .push((*ts, *price));
        }
        info!(
            "Loaded {} spot records from binance_price_ticks (sync_records was empty)",
            price_rows.len()
        );
    } else {
        info!("No sync_records or binance_price_ticks available for spot replay");
    }

    let kline_spot_rows: Vec<(DateTime<Utc>, String, Decimal)> = if klines_exists {
        sqlx::query_as(
            r#"
                SELECT close_time, symbol, close
                FROM binance_klines
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR close_time >= $2)
                  AND ($3::timestamptz IS NULL OR close_time <= $3)
                ORDER BY close_time
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

    for (ts, sym, price) in &kline_spot_rows {
        updates.push(MarketUpdate {
            timestamp: *ts,
            symbol: sym.clone(),
            update_type: UpdateType::SpotTrade {
                price: *price,
                quantity: None,
            },
        });
        spot_series
            .entry(sym.clone())
            .or_default()
            .push((*ts, *price));
    }
    if !kline_spot_rows.is_empty() {
        info!(
            "Supplemented with {} kline spot records",
            kline_spot_rows.len()
        );
    }

    let binance_l2_rows: Vec<(DateTime<Utc>, String, serde_json::Value, serde_json::Value)> =
        if binance_lob_ticks_exists {
        sqlx::query_as(
            r#"
                SELECT
                    event_time,
                    symbol,
                    bids,
                    asks
                FROM binance_lob_ticks
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR event_time >= $2)
                  AND ($3::timestamptz IS NULL OR event_time <= $3)
                ORDER BY event_time ASC, symbol ASC
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

    for (ts, sym, bids, asks) in &binance_l2_rows {
        if let Some(update) = build_binance_l2_update(*ts, sym, bids, asks) {
            updates.push(update);
        }
    }
    if !binance_l2_rows.is_empty() {
        info!(
            "Loaded {} Binance L2 feature rows from binance_lob_ticks",
            binance_l2_rows.len()
        );
    }

    for series in spot_series.values_mut() {
        series.sort_by_key(|(ts, _)| *ts);
    }

    let mut mappings = build_token_mappings(
        pool,
        symbols,
        from,
        to,
        sync_records_exists,
        pm_market_metadata_exists,
        pm_token_settlements_exists,
    )
    .await;

    load_quote_updates(
        pool,
        &mut updates,
        &mappings,
        symbols,
        from,
        to,
        sync_records_exists,
        quote_ticks_exists,
    )
    .await;

    load_event_updates(
        pool,
        &mut updates,
        &spot_series,
        &mut mappings,
        symbols,
        from,
        to,
        sync_records_exists,
        pm_market_metadata_exists,
        pm_token_settlements_exists,
    )
    .await;

    load_lob_updates(pool, &mut updates, &mappings, from, to, lob_snaps_exists).await;

    updates.sort_by_key(|u| u.timestamp);
    info!("HistoricalFeed ready: {} total events", updates.len());
    Ok(updates)
}

async fn table_exists(pool: &PgPool, table_name: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(table_name)
            .fetch_one(pool)
            .await
            .unwrap_or(None)
            .is_some(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BinanceBookLevel {
    price: Decimal,
    size: Decimal,
}

fn build_binance_l2_update(
    timestamp: DateTime<Utc>,
    symbol: &str,
    bids_json: &serde_json::Value,
    asks_json: &serde_json::Value,
) -> Option<MarketUpdate> {
    let bids = parse_binance_book_levels(bids_json, true);
    let asks = parse_binance_book_levels(asks_json, false);
    if bids.len() < 20 || asks.len() < 20 {
        return None;
    }

    let best_bid = bids.first()?.price;
    let best_ask = asks.first()?.price;
    if best_bid <= Decimal::ZERO || best_ask <= Decimal::ZERO || best_ask <= best_bid {
        return None;
    }

    Some(MarketUpdate {
        timestamp,
        symbol: symbol.to_string(),
        update_type: UpdateType::BinanceL2 {
            obi_1: calculate_obi(&bids, &asks, 1)?,
            obi_2: calculate_obi(&bids, &asks, 2)?,
            obi_3: calculate_obi(&bids, &asks, 3)?,
            obi_5: calculate_obi(&bids, &asks, 5)?,
            obi_10: calculate_obi(&bids, &asks, 10)?,
            obi_20: calculate_obi(&bids, &asks, 20)?,
            bid_volume_5: side_volume(&bids, 5),
            ask_volume_5: side_volume(&asks, 5),
            spread_bps: ((best_ask - best_bid) / best_bid) * Decimal::from(10000),
        },
    })
}

fn parse_binance_book_levels(raw: &serde_json::Value, is_bid: bool) -> Vec<BinanceBookLevel> {
    let Some(levels) = raw.as_array() else {
        return Vec::new();
    };

    let mut parsed = levels
        .iter()
        .filter_map(|level| {
            let price = level.get("price")?.as_str()?.parse::<Decimal>().ok()?;
            let size = level.get("size")?.as_str()?.parse::<Decimal>().ok()?;
            if price <= Decimal::ZERO || size <= Decimal::ZERO {
                return None;
            }
            Some(BinanceBookLevel { price, size })
        })
        .collect::<Vec<_>>();

    if is_bid {
        parsed.sort_by(|left, right| right.price.cmp(&left.price));
    } else {
        parsed.sort_by(|left, right| left.price.cmp(&right.price));
    }
    parsed
}

fn calculate_obi(
    bids: &[BinanceBookLevel],
    asks: &[BinanceBookLevel],
    depth: usize,
) -> Option<Decimal> {
    let bid_sum = side_volume(bids, depth);
    let ask_sum = side_volume(asks, depth);
    let total = bid_sum + ask_sum;
    if total <= Decimal::ZERO {
        return None;
    }
    Some((bid_sum - ask_sum) / total)
}

fn side_volume(levels: &[BinanceBookLevel], depth: usize) -> Decimal {
    levels.iter().take(depth).map(|level| level.size).sum()
}

fn symbol_filter(symbols: &[String]) -> Option<Vec<String>> {
    if symbols.is_empty() {
        None
    } else {
        Some(symbols.to_vec())
    }
}

#[derive(Default)]
struct TokenMappings {
    token_to_symbol: HashMap<String, String>,
    token_to_slug: HashMap<String, String>,
    token_to_side: HashMap<String, Side>,
    slug_to_symbol: HashMap<String, String>,
}

impl TokenMappings {
    fn known_token_ids(&self) -> Vec<String> {
        self.token_to_slug.keys().cloned().collect()
    }

    fn resolve_symbol(&self, token_id: &str, event_slug: &str) -> Option<String> {
        self.token_to_symbol
            .get(token_id)
            .cloned()
            .or_else(|| self.slug_to_symbol.get(event_slug).cloned())
            .or_else(|| infer_symbol_from_slug(event_slug))
    }
}

async fn build_token_mappings(
    pool: &PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    sync_records_exists: bool,
    pm_market_metadata_exists: bool,
    pm_token_settlements_exists: bool,
) -> TokenMappings {
    let mut mappings = TokenMappings::default();

    if sync_records_exists {
        let sync_map_rows: Result<Vec<(String, String, Option<String>, Option<String>)>> =
            sqlx::query_as(
                r#"
                    SELECT DISTINCT pm_market_slug, symbol, pm_yes_token_id, pm_no_token_id
                    FROM sync_records
                    WHERE pm_market_slug IS NOT NULL
                      AND ($1::text[] IS NULL OR symbol = ANY($1))
                      AND ($2::timestamptz IS NULL OR timestamp >= $2)
                      AND ($3::timestamptz IS NULL OR timestamp <= $3)
                    "#,
            )
            .bind(symbol_filter(symbols))
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await
            .map_err(Into::into);

        match sync_map_rows {
            Ok(rows) => {
                for (slug, sym, yes_token_id, no_token_id) in rows {
                    if !is_supported_5m_slug(&slug) {
                        continue;
                    }
                    if !slug.is_empty() && !sym.is_empty() {
                        mappings.slug_to_symbol.insert(slug.clone(), sym.clone());
                    }
                    if let Some(t) = yes_token_id {
                        mappings.token_to_slug.insert(t.clone(), slug.clone());
                        mappings.token_to_side.insert(t.clone(), Side::Up);
                        if !sym.is_empty() {
                            mappings.token_to_symbol.insert(t, sym.clone());
                        }
                    }
                    if let Some(t) = no_token_id {
                        mappings.token_to_slug.insert(t.clone(), slug.clone());
                        mappings.token_to_side.insert(t.clone(), Side::Down);
                        if !sym.is_empty() {
                            mappings.token_to_symbol.insert(t, sym.clone());
                        }
                    }
                }
                info!(
                    "Built token mapping from sync_records: {} tokens, {} slugs",
                    mappings.token_to_slug.len(),
                    mappings.slug_to_symbol.len()
                );
            }
            Err(e) => {
                info!("sync_records mapping query failed (older schema?): {e}");
            }
        }
    }

    if pm_token_settlements_exists {
        let settlement_map_rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
                SELECT token_id, market_slug, outcome
                FROM pm_token_settlements
                WHERE market_slug IS NOT NULL AND market_slug != ''
                  AND ($1::timestamptz IS NULL OR fetched_at >= $1)
                  AND ($2::timestamptz IS NULL OR fetched_at <= $2)
                "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for (token_id, market_slug, outcome) in settlement_map_rows {
            let Some(slug) = market_slug else { continue };
            if !is_supported_5m_slug(&slug) {
                continue;
            }
            mappings
                .token_to_slug
                .entry(token_id.clone())
                .or_insert_with(|| slug.clone());
            if let Some(sym) = mappings
                .slug_to_symbol
                .get(&slug)
                .cloned()
                .or_else(|| infer_symbol_from_slug(&slug))
            {
                mappings
                    .token_to_symbol
                    .entry(token_id.clone())
                    .or_insert(sym);
            }
            if !mappings.slug_to_symbol.contains_key(&slug) {
                if let Some(sym) = infer_symbol_from_slug(&slug) {
                    mappings.slug_to_symbol.insert(slug.clone(), sym);
                }
            }
            if let Some(side) = outcome
                .as_deref()
                .and_then(parse_token_outcome_side)
            {
                mappings.token_to_side.entry(token_id).or_insert(side);
            }
        }
        info!(
            "Built token mapping from pm_token_settlements: {} tokens",
            mappings.token_to_slug.len()
        );
    }

    if pm_market_metadata_exists {
        let before = mappings.token_to_slug.len();
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            r#"
                SELECT DISTINCT
                    market_slug,
                    symbol,
                    jsonb_array_elements_text((raw_market->>'clobTokenIds')::jsonb) AS token_id
                FROM pm_market_metadata
                WHERE raw_market IS NOT NULL
                  AND raw_market ? 'clobTokenIds'
                  AND ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR end_time >= $2)
                  AND ($3::timestamptz IS NULL OR start_time <= $3)
                "#,
        )
        .bind(symbol_filter(symbols))
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for (slug, sym, token_id) in rows {
            if slug.is_empty() || token_id.is_empty() {
                continue;
            }
            if !is_supported_5m_slug(&slug) {
                continue;
            }
            let Some(token_id_norm) = normalize_clob_token_id(&token_id) else {
                continue;
            };

            mappings
                .token_to_slug
                .entry(token_id_norm.clone())
                .or_insert_with(|| slug.clone());

            let symbol = sym
                .filter(|s| !s.is_empty())
                .or_else(|| mappings.slug_to_symbol.get(&slug).cloned())
                .or_else(|| infer_symbol_from_slug(&slug));

            if let Some(symbol) = symbol {
                if !symbol.is_empty() {
                    mappings
                        .token_to_symbol
                        .entry(token_id_norm)
                        .or_insert(symbol.clone());
                    mappings.slug_to_symbol.entry(slug).or_insert(symbol);
                }
            }
        }

        let after = mappings.token_to_slug.len();
        if after > before {
            info!(
                "Supplemented token mapping from pm_market_metadata.raw_market.clobTokenIds: +{} tokens (now {})",
                after - before,
                after
            );
        }
    }

    mappings
}

async fn load_quote_updates(
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
        Option<Decimal>,
        Option<Decimal>,
    )> = if quote_ticks_exists && !known_token_ids.is_empty() {
        sqlx::query_as(
            r#"
                    SELECT DISTINCT ON (date_trunc('second', received_at), token_id, side)
                           received_at, token_id, side, best_bid, best_ask, bid_size, ask_size
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

    for (ts, token_id, side, best_bid, best_ask, bid_size, ask_size) in &quote_rows {
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
                bid_size: *bid_size,
                ask_size: *ask_size,
            },
        });
    }
    info!(
        "Loaded {} quote ticks (pre-filtered to {} known tokens)",
        quote_rows.len(),
        known_token_ids.len()
    );

    if quote_rows.is_empty() && sync_records_exists {
        let sync_quote_rows: Result<
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
                    if !is_supported_5m_slug(&slug) {
                        continue;
                    }
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
                                bid_size: None,
                                ask_size: None,
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
                                bid_size: None,
                                ask_size: None,
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

async fn load_event_updates(
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
            if !is_supported_5m_slug(&slug) {
                continue;
            }
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
                    if mid > Decimal::ZERO {
                        Some(mid)
                    } else {
                        None
                    }
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
    let mut event_end_by_slug: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut event_symbol_by_slug: HashMap<String, String> = HashMap::new();
    for (slug, sym, start_time, end_time, price_to_beat) in &event_rows {
        if !is_supported_5m_slug(slug) {
            continue;
        }
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
            symbol: symbol.clone(),
            update_type: UpdateType::EventState {
                event_slug: slug.clone(),
                end_time: Some(corrected_end),
                price_to_beat: Some(s0),
                outcome: None,
            },
        });
        event_end_by_slug.insert(slug.clone(), corrected_end);
        event_symbol_by_slug.insert(slug.clone(), symbol);
        event_open_count += 1;
    }
    info!(
        "Loaded {} event window rows (pm_market_metadata + derived), emitted {} EventState opens",
        event_rows.len(),
        event_open_count
    );

    if event_open_count == 0 && sync_records_exists {
        let rows: Result<Vec<(String, String, DateTime<Utc>)>> = sqlx::query_as(
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
                    if !is_supported_5m_slug(&slug) {
                        continue;
                    }
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
                        symbol: sym.clone(),
                        update_type: UpdateType::EventState {
                            event_slug: slug.clone(),
                            end_time: Some(end),
                            price_to_beat: Some(s0),
                            outcome: None,
                        },
                    });
                    event_end_by_slug.insert(slug.clone(), end);
                    event_symbol_by_slug.insert(slug, sym);
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

    let opened_slugs: Vec<String> = event_end_by_slug.keys().cloned().collect();
    let settlement_rows: Vec<(String, String, Decimal, Option<DateTime<Utc>>)> =
        if pm_token_settlements_exists && !opened_slugs.is_empty() {
            sqlx::query_as(
                r#"
                SELECT DISTINCT ON (market_slug) market_slug, outcome, settled_price, resolved_at
                FROM pm_token_settlements
                WHERE resolved = true
                  AND LOWER(outcome) = 'up'
                  AND market_slug = ANY($1)
                ORDER BY market_slug, COALESCE(resolved_at, fetched_at) DESC
                "#,
            )
            .bind(&opened_slugs)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            Vec::new()
        };

    for (slug, _outcome, settled_price, resolved_at) in &settlement_rows {
        let Some(timestamp) = event_end_by_slug
            .get(slug.as_str())
            .copied()
            .or(*resolved_at)
        else {
            continue;
        };
        let symbol = event_symbol_by_slug
            .get(slug.as_str())
            .cloned()
            .or_else(|| mappings.slug_to_symbol.get(slug.as_str()).cloned())
            .or_else(|| infer_symbol_from_slug(slug))
            .unwrap_or_default();
        if symbol.is_empty() {
            continue;
        }
        updates.push(MarketUpdate {
            timestamp,
            symbol,
            update_type: UpdateType::EventState {
                event_slug: slug.clone(),
                end_time: Some(timestamp),
                price_to_beat: None,
                outcome: Some(*settled_price == Decimal::ONE),
            },
        });
    }
    info!("Loaded {} settlement records", settlement_rows.len());
}

async fn load_lob_updates(
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
        if let Some(update) = build_lob_update(*ts, token_id, asks_json, mappings) {
            updates.push(update);
            lob_count += 1;
        }
    }
    info!(
        "Loaded {} LOB snapshots ({} mapped to symbols)",
        lob_rows.len(),
        lob_count
    );
}

fn infer_symbol_from_slug(slug: &str) -> Option<String> {
    let s = slug.to_ascii_lowercase();
    if s.starts_with("btc-") || s.starts_with("bitcoin-") {
        return Some("BTCUSDT".to_string());
    }
    if s.starts_with("eth-") || s.starts_with("ethereum-") {
        return Some("ETHUSDT".to_string());
    }
    if s.starts_with("sol-") || s.starts_with("solana-") {
        return Some("SOLUSDT".to_string());
    }
    None
}

fn parse_token_outcome_side(outcome: &str) -> Option<Side> {
    match outcome.trim().to_ascii_lowercase().as_str() {
        "up" | "yes" => Some(Side::Up),
        "down" | "no" => Some(Side::Down),
        _ => None,
    }
}

fn build_lob_update(
    timestamp: DateTime<Utc>,
    token_id: &str,
    asks_json: &Option<serde_json::Value>,
    mappings: &TokenMappings,
) -> Option<MarketUpdate> {
    const MAX_REPLAY_ASK_LEVELS: usize = 5;

    let event_slug = mappings.token_to_slug.get(token_id)?.clone();
    let symbol = mappings.resolve_symbol(token_id, &event_slug)?;
    if symbol.is_empty() {
        return None;
    }
    let side = *mappings.token_to_side.get(token_id)?;

    let (total_depth, best_ask_size, ask_levels, best_ask_price) = match asks_json {
        Some(arr) if arr.is_array() => {
            let levels = arr.as_array().unwrap();
            let mut depth = 0u64;
            let mut parsed_levels = Vec::new();
            for level in levels.iter() {
                if let (Some(size_str), Some(price_str)) = (
                    level.get("size").and_then(|v| v.as_str()),
                    level.get("price").and_then(|v| v.as_str()),
                ) {
                    if let (Ok(size), Ok(price)) =
                        (size_str.parse::<f64>(), price_str.parse::<Decimal>())
                    {
                        let size_shares = size.floor() as u64;
                        if size_shares > 0 {
                            depth += size_shares;
                            parsed_levels.push(BookAskLevel { price, size_shares });
                        }
                    }
                }
            }
            parsed_levels.sort_by(|left, right| left.price.cmp(&right.price));
            let best = parsed_levels.first().copied();
            let ask_levels = parsed_levels
                .iter()
                .copied()
                .take(MAX_REPLAY_ASK_LEVELS)
                .collect::<Vec<_>>();
            (
                depth,
                best.map(|level| level.size_shares).unwrap_or(0),
                ask_levels,
                best.map(|level| level.price),
            )
        }
        _ => return None,
    };

    if total_depth == 0 || best_ask_size == 0 || ask_levels.is_empty() {
        return None;
    }

    Some(MarketUpdate {
        timestamp,
        symbol,
        update_type: UpdateType::LobSnapshot {
            event_slug,
            token_id: token_id.to_string(),
            side,
            ask_depth_shares: total_depth,
            best_ask_size_shares: best_ask_size,
            ask_levels,
            best_ask: best_ask_price,
        },
    })
}

fn is_supported_5m_slug(slug: &str) -> bool {
    infer_window_duration_secs(slug) == Some(300)
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

fn normalize_clob_token_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return U256::from_str_radix(hex, 16).ok().map(|u| u.to_string());
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Some(s.to_string());
    }
    U256::from_str_radix(s, 16).ok().map(|u| u.to_string())
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

fn corrected_window_end(
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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn normalize_clob_token_id_accepts_hex_without_prefix() {
        assert_eq!(
            normalize_clob_token_id("0f"),
            Some(U256::from(15u8).to_string())
        );
    }

    #[test]
    fn corrected_window_end_caps_bad_metadata_for_short_windows() {
        let start_time = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let bad_end = DateTime::parse_from_rfc3339("2025-01-01T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            corrected_window_end("btc-updown-5m-123", start_time, bad_end),
            DateTime::parse_from_rfc3339("2025-01-01T00:05:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn parse_token_outcome_side_accepts_up_down_aliases() {
        assert_eq!(parse_token_outcome_side("UP"), Some(Side::Up));
        assert_eq!(parse_token_outcome_side("yes"), Some(Side::Up));
        assert_eq!(parse_token_outcome_side("DOWN"), Some(Side::Down));
        assert_eq!(parse_token_outcome_side("no"), Some(Side::Down));
        assert_eq!(parse_token_outcome_side("other"), None);
    }

    #[test]
    fn build_lob_update_uses_token_side_mapping() {
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut mappings = TokenMappings::default();
        mappings
            .token_to_slug
            .insert("token-up".to_string(), "btc-updown-5m-test".to_string());
        mappings
            .token_to_symbol
            .insert("token-up".to_string(), "BTCUSDT".to_string());
        mappings
            .token_to_side
            .insert("token-up".to_string(), Side::Up);

        let asks_json = Some(json!([
            {"price": "0.43", "size": "25"},
            {"price": "0.44", "size": "10"}
        ]));

        let update = build_lob_update(ts, "token-up", &asks_json, &mappings).expect("lob update");
        assert_eq!(update.symbol, "BTCUSDT");
        match update.update_type {
            UpdateType::LobSnapshot {
                event_slug,
                token_id,
                side,
                ask_depth_shares,
                best_ask_size_shares,
                ask_levels,
                best_ask,
            } => {
                assert_eq!(event_slug, "btc-updown-5m-test");
                assert_eq!(token_id, "token-up");
                assert_eq!(side, Side::Up);
                assert_eq!(ask_depth_shares, 35);
                assert_eq!(best_ask_size_shares, 25);
                assert_eq!(
                    ask_levels,
                    vec![
                        BookAskLevel {
                            price: Decimal::new(43, 2),
                            size_shares: 25,
                        },
                        BookAskLevel {
                            price: Decimal::new(44, 2),
                            size_shares: 10,
                        },
                    ]
                );
                assert_eq!(best_ask, Some(Decimal::new(43, 2)));
            }
            other => panic!("unexpected update type: {other:?}"),
        }
    }

    #[test]
    fn build_lob_update_sorts_unsorted_asks_before_replay() {
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut mappings = TokenMappings::default();
        mappings
            .token_to_slug
            .insert("token-up".to_string(), "btc-updown-5m-test".to_string());
        mappings
            .token_to_symbol
            .insert("token-up".to_string(), "BTCUSDT".to_string());
        mappings
            .token_to_side
            .insert("token-up".to_string(), Side::Up);

        let asks_json = Some(json!([
            {"price": "0.44", "size": "10"},
            {"price": "0.43", "size": "25"},
            {"price": "0.45", "size": "5"}
        ]));

        let update = build_lob_update(ts, "token-up", &asks_json, &mappings).expect("lob update");
        match update.update_type {
            UpdateType::LobSnapshot {
                best_ask_size_shares,
                ask_levels,
                best_ask,
                ..
            } => {
                assert_eq!(best_ask_size_shares, 25);
                assert_eq!(best_ask, Some(Decimal::new(43, 2)));
                assert_eq!(
                    ask_levels,
                    vec![
                        BookAskLevel {
                            price: Decimal::new(43, 2),
                            size_shares: 25,
                        },
                        BookAskLevel {
                            price: Decimal::new(44, 2),
                            size_shares: 10,
                        },
                        BookAskLevel {
                            price: Decimal::new(45, 2),
                            size_shares: 5,
                        },
                    ]
                );
            }
            other => panic!("unexpected update type: {other:?}"),
        }
    }

    #[test]
    fn build_binance_l2_update_reconstructs_native_obi_ladder() {
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let bids = json!([
            {"price": "99.1", "size": "11"},
            {"price": "99.8", "size": "18"},
            {"price": "99.4", "size": "14"},
            {"price": "99.7", "size": "17"},
            {"price": "99.2", "size": "12"},
            {"price": "99.9", "size": "19"},
            {"price": "99.5", "size": "15"},
            {"price": "99.3", "size": "13"},
            {"price": "100.0", "size": "20"},
            {"price": "99.6", "size": "16"},
            {"price": "98.1", "size": "1"},
            {"price": "98.3", "size": "3"},
            {"price": "98.2", "size": "2"},
            {"price": "98.5", "size": "5"},
            {"price": "98.4", "size": "4"},
            {"price": "98.7", "size": "7"},
            {"price": "98.6", "size": "6"},
            {"price": "98.9", "size": "9"},
            {"price": "98.8", "size": "8"},
            {"price": "99.0", "size": "10"}
        ]);
        let asks = json!([
            {"price": "100.9", "size": "9"},
            {"price": "100.2", "size": "2"},
            {"price": "100.6", "size": "6"},
            {"price": "100.1", "size": "1"},
            {"price": "100.4", "size": "4"},
            {"price": "100.5", "size": "5"},
            {"price": "100.3", "size": "3"},
            {"price": "100.7", "size": "7"},
            {"price": "100.8", "size": "8"},
            {"price": "101.0", "size": "10"},
            {"price": "101.1", "size": "11"},
            {"price": "101.2", "size": "12"},
            {"price": "101.3", "size": "13"},
            {"price": "101.4", "size": "14"},
            {"price": "101.5", "size": "15"},
            {"price": "101.6", "size": "16"},
            {"price": "101.7", "size": "17"},
            {"price": "101.8", "size": "18"},
            {"price": "101.9", "size": "19"},
            {"price": "102.0", "size": "20"}
        ]);

        let update = build_binance_l2_update(ts, "BTCUSDT", &bids, &asks).expect("l2 update");
        match update.update_type {
            UpdateType::BinanceL2 {
                obi_1,
                obi_2,
                obi_3,
                obi_5,
                obi_10,
                obi_20,
                bid_volume_5,
                ask_volume_5,
                spread_bps,
            } => {
                assert_eq!(obi_1, dec!(19) / dec!(21));
                assert_eq!(obi_2, dec!(36) / dec!(42));
                assert_eq!(obi_3, dec!(51) / dec!(63));
                assert_eq!(obi_5, dec!(75) / dec!(105));
                assert_eq!(obi_10, dec!(100) / dec!(210));
                assert_eq!(obi_20, Decimal::ZERO);
                assert_eq!(bid_volume_5, dec!(90));
                assert_eq!(ask_volume_5, dec!(15));
                assert_eq!(spread_bps, dec!(10));
            }
            other => panic!("unexpected update type: {other:?}"),
        }
    }

    #[test]
    fn build_binance_l2_update_rejects_partial_depth_books() {
        let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let bids = json!([
            {"price": "100.0", "size": "10"},
            {"price": "99.9", "size": "9"},
            {"price": "99.8", "size": "8"}
        ]);
        let asks = json!([
            {"price": "100.1", "size": "1"},
            {"price": "100.2", "size": "2"},
            {"price": "100.3", "size": "3"}
        ]);

        assert!(build_binance_l2_update(ts, "BTCUSDT", &bids, &asks).is_none());
    }

    #[test]
    fn supported_5m_slug_filter_rejects_other_horizons() {
        assert!(is_supported_5m_slug("btc-updown-5m-123"));
        assert!(!is_supported_5m_slug("btc-updown-15m-123"));
        assert!(!is_supported_5m_slug("btc-updown-60m-123"));
    }
}
