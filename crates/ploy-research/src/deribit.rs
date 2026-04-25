use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

use crate::DeribitFeatureSnapshot;

pub async fn load_deribit_feature_snapshots(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i64,
) -> Vec<DeribitFeatureSnapshot> {
    let mut currency_set = BTreeSet::new();
    let mut unsupported_symbols = Vec::new();
    for symbol in symbols {
        match symbol_to_deribit_currency(symbol) {
            Some(currency) => {
                currency_set.insert(currency);
            }
            None => unsupported_symbols.push(symbol.clone()),
        }
    }
    if !unsupported_symbols.is_empty() {
        tracing::warn!(
            symbols = ?unsupported_symbols,
            "deribit feature loader skipped unsupported symbols"
        );
    }

    let currencies: Vec<String> = currency_set.into_iter().collect();
    if currencies.is_empty() {
        tracing::warn!("deribit feature loader has no supported currencies");
        return Vec::new();
    }

    let sample_secs = sample_secs.clamp(1, 300) as i32;
    let mut snapshots: BTreeMap<(String, DateTime<Utc>), DeribitFeatureSnapshot> = BTreeMap::new();
    let current_iv_rows: Vec<(
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        DateTime<Utc>,
    )> = match sqlx::query_as(
        r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.bid_iv::double precision,
            d.ask_iv::double precision,
            d.underlying_price::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, bid_iv, ask_iv, underlying_price
            FROM deribit_iv_ticks d
            WHERE d.currency = c.currency
              AND d.creation_ts <= b.bucket_ts
              AND d.creation_ts > b.bucket_ts - interval '5 minutes'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.creation_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#,
    )
    .bind(&currencies)
    .bind(start)
    .bind(end)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "deribit iv load failed");
            Vec::new()
        }
    };

    for (currency, mark_iv, bid_iv, ask_iv, underlying_price, ts) in current_iv_rows {
        let symbol = deribit_currency_to_symbol(&currency);
        merge_deribit_snapshot(
            &mut snapshots,
            symbol,
            ts,
            DeribitFeatureSnapshot {
                symbol: String::new(),
                ts,
                mark_iv: mark_iv.unwrap_or(f64::NAN),
                bid_iv: bid_iv.unwrap_or(f64::NAN),
                ask_iv: ask_iv.unwrap_or(f64::NAN),
                underlying_price: underlying_price.unwrap_or(f64::NAN),
                delta: f64::NAN,
                gamma: f64::NAN,
                vega: f64::NAN,
                theta: f64::NAN,
            },
        );
    }

    if snapshots.is_empty() {
        let legacy_iv_rows: Vec<(String, Option<f64>, Option<f64>, DateTime<Utc>)> =
            match sqlx::query_as(
                r#"
                WITH currencies AS (
                    SELECT unnest($1::text[]) AS currency
                ),
                buckets AS (
                    SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
                )
                SELECT
                    c.currency,
                    COALESCE(d.atm_iv, d.iv_close, d.iv_open)::double precision,
                    d.iv_close::double precision,
                    b.bucket_ts
                FROM currencies c
                CROSS JOIN buckets b
                JOIN LATERAL (
                    SELECT atm_iv, iv_close, iv_open, timestamp
                    FROM deribit_iv_ticks d
                    WHERE d.currency = c.currency
                      AND d.timestamp <= b.bucket_ts
                      AND d.timestamp > b.bucket_ts - interval '5 minutes'
                      AND COALESCE(d.atm_iv, d.iv_close, d.iv_open) IS NOT NULL
                    ORDER BY d.timestamp DESC
                    LIMIT 1
                ) d ON true
                ORDER BY b.bucket_ts
                "#,
            )
            .bind(&currencies)
            .bind(start)
            .bind(end)
            .bind(sample_secs)
            .fetch_all(pool)
            .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    tracing::warn!(error = %err, "legacy deribit iv load skipped");
                    Vec::new()
                }
            };
        for (currency, atm_iv, iv_close, ts) in legacy_iv_rows {
            let mark_iv = atm_iv.or(iv_close).unwrap_or(f64::NAN);
            let symbol = deribit_currency_to_symbol(&currency);
            merge_deribit_snapshot(
                &mut snapshots,
                symbol,
                ts,
                DeribitFeatureSnapshot {
                    symbol: String::new(),
                    ts,
                    mark_iv,
                    bid_iv: f64::NAN,
                    ask_iv: f64::NAN,
                    underlying_price: f64::NAN,
                    delta: f64::NAN,
                    gamma: f64::NAN,
                    vega: f64::NAN,
                    theta: f64::NAN,
                },
            );
        }
    }

    let greeks_rows: Vec<(
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        DateTime<Utc>,
    )> = match sqlx::query_as(
        r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.delta::double precision,
            d.gamma::double precision,
            d.vega::double precision,
            d.theta::double precision,
            d.underlying_price::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, delta, gamma, vega, theta, underlying_price
            FROM deribit_atm_greeks_ticks d
            WHERE d.currency = c.currency
              AND d.source_ts <= b.bucket_ts
              AND d.source_ts > b.bucket_ts - interval '5 minutes'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.source_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#,
    )
    .bind(&currencies)
    .bind(start)
    .bind(end)
    .bind(sample_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "deribit greeks load failed");
            Vec::new()
        }
    };

    for (currency, mark_iv, delta, gamma, vega, theta, underlying_price, ts) in greeks_rows {
        let symbol = deribit_currency_to_symbol(&currency);
        merge_deribit_snapshot(
            &mut snapshots,
            symbol,
            ts,
            DeribitFeatureSnapshot {
                symbol: String::new(),
                ts,
                mark_iv: mark_iv.unwrap_or(f64::NAN),
                bid_iv: f64::NAN,
                ask_iv: f64::NAN,
                underlying_price: underlying_price.unwrap_or(f64::NAN),
                delta: delta.unwrap_or(f64::NAN),
                gamma: gamma.unwrap_or(f64::NAN),
                vega: vega.unwrap_or(f64::NAN),
                theta: theta.unwrap_or(f64::NAN),
            },
        );
    }

    let mut snapshots: Vec<_> = snapshots.into_values().collect();
    snapshots.sort_by_key(|snapshot| (snapshot.symbol.clone(), snapshot.ts));
    snapshots
}

fn merge_deribit_snapshot(
    snapshots: &mut BTreeMap<(String, DateTime<Utc>), DeribitFeatureSnapshot>,
    symbol: String,
    ts: DateTime<Utc>,
    mut incoming: DeribitFeatureSnapshot,
) {
    incoming.symbol = symbol.clone();
    incoming.ts = ts;
    snapshots
        .entry((symbol, ts))
        .and_modify(|existing| merge_finite_fields(existing, &incoming))
        .or_insert(incoming);
}

fn merge_finite_fields(existing: &mut DeribitFeatureSnapshot, incoming: &DeribitFeatureSnapshot) {
    update_if_finite(&mut existing.mark_iv, incoming.mark_iv);
    update_if_finite(&mut existing.bid_iv, incoming.bid_iv);
    update_if_finite(&mut existing.ask_iv, incoming.ask_iv);
    update_if_finite(&mut existing.underlying_price, incoming.underlying_price);
    update_if_finite(&mut existing.delta, incoming.delta);
    update_if_finite(&mut existing.gamma, incoming.gamma);
    update_if_finite(&mut existing.vega, incoming.vega);
    update_if_finite(&mut existing.theta, incoming.theta);
}

fn update_if_finite(target: &mut f64, value: f64) {
    if value.is_finite() {
        *target = value;
    }
}

fn symbol_to_deribit_currency(symbol: &str) -> Option<String> {
    let upper = symbol.trim().to_ascii_uppercase();
    if upper.starts_with("BTC") {
        Some("BTC".to_string())
    } else if upper.starts_with("ETH") {
        Some("ETH".to_string())
    } else if upper.starts_with("SOL") {
        Some("SOL".to_string())
    } else {
        None
    }
}

fn deribit_currency_to_symbol(currency: &str) -> String {
    match currency.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTCUSDT".to_string(),
        "ETH" => "ETHUSDT".to_string(),
        "SOL" => "SOLUSDT".to_string(),
        other => format!("{other}USDT"),
    }
}
