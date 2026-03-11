mod event_queries;
mod lob_queries;
mod quote_queries;
mod token_mappings;

use super::{MarketUpdate, UpdateType};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

use self::{
    event_queries::load_event_updates, lob_queries::load_lob_updates,
    quote_queries::load_quote_updates, token_mappings::build_token_mappings,
};

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

    let binance_l2_rows: Vec<(
        DateTime<Utc>,
        String,
        Decimal,
        Decimal,
        Decimal,
        Decimal,
        Decimal,
    )> = if binance_lob_ticks_exists {
        sqlx::query_as(
            r#"
                SELECT DISTINCT ON (date_trunc('second', event_time), symbol)
                    event_time,
                    symbol,
                    obi_5,
                    obi_10,
                    bid_volume_5,
                    ask_volume_5,
                    spread_bps
                FROM binance_lob_ticks
                WHERE ($1::text[] IS NULL OR symbol = ANY($1))
                  AND ($2::timestamptz IS NULL OR event_time >= $2)
                  AND ($3::timestamptz IS NULL OR event_time <= $3)
                ORDER BY date_trunc('second', event_time), symbol, event_time DESC
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

    for (ts, sym, obi_5, obi_10, bid_volume_5, ask_volume_5, spread_bps) in &binance_l2_rows {
        updates.push(MarketUpdate {
            timestamp: *ts,
            symbol: sym.clone(),
            update_type: UpdateType::BinanceL2 {
                obi_5: *obi_5,
                obi_10: *obi_10,
                bid_volume_5: *bid_volume_5,
                ask_volume_5: *ask_volume_5,
                spread_bps: *spread_bps,
            },
        });
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

fn symbol_filter(symbols: &[String]) -> Option<Vec<String>> {
    if symbols.is_empty() {
        None
    } else {
        Some(symbols.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::{event_queries::corrected_window_end, token_mappings::normalize_clob_token_id};
    use alloy::primitives::U256;
    use chrono::{DateTime, Utc};

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
}
