use chrono::{DateTime, Utc};

use crate::DeribitFeatureSnapshot;

pub async fn load_deribit_feature_snapshots(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i64,
) -> Vec<DeribitFeatureSnapshot> {
    let currencies: Vec<String> = symbols
        .iter()
        .filter_map(|symbol| symbol_to_deribit_currency(symbol))
        .collect();
    if currencies.is_empty() {
        return Vec::new();
    }

    let sample_secs = sample_secs.clamp(1, 300) as i32;
    let mut snapshots = Vec::new();
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
            eprintln!("deribit iv load failed: {err}");
            Vec::new()
        }
    };

    for (currency, mark_iv, bid_iv, ask_iv, underlying_price, ts) in current_iv_rows {
        snapshots.push(DeribitFeatureSnapshot {
            symbol: deribit_currency_to_symbol(&currency),
            ts,
            mark_iv: mark_iv.unwrap_or(f64::NAN),
            bid_iv: bid_iv.unwrap_or(f64::NAN),
            ask_iv: ask_iv.unwrap_or(f64::NAN),
            underlying_price: underlying_price.unwrap_or(f64::NAN),
            delta: f64::NAN,
            gamma: f64::NAN,
            vega: f64::NAN,
            theta: f64::NAN,
        });
    }

    if snapshots.is_empty() {
        let legacy_iv_rows: Vec<(String, Option<f64>, Option<f64>, DateTime<Utc>)> =
            match sqlx::query_as(
                r#"
                SELECT
                    currency,
                    COALESCE(atm_iv, iv_close, iv_open)::double precision,
                    iv_close::double precision,
                    timestamp
                FROM deribit_iv_ticks
                WHERE currency = ANY($1)
                  AND timestamp >= $2
                  AND timestamp <= $3
                ORDER BY timestamp
                "#,
            )
            .bind(&currencies)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    eprintln!("legacy deribit iv load skipped: {err}");
                    Vec::new()
                }
            };
        for (currency, atm_iv, iv_close, ts) in legacy_iv_rows {
            let mark_iv = atm_iv.or(iv_close).unwrap_or(f64::NAN);
            snapshots.push(DeribitFeatureSnapshot {
                symbol: deribit_currency_to_symbol(&currency),
                ts,
                mark_iv,
                bid_iv: f64::NAN,
                ask_iv: f64::NAN,
                underlying_price: f64::NAN,
                delta: f64::NAN,
                gamma: f64::NAN,
                vega: f64::NAN,
                theta: f64::NAN,
            });
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
            eprintln!("deribit greeks load failed: {err}");
            Vec::new()
        }
    };

    for (currency, mark_iv, delta, gamma, vega, theta, underlying_price, ts) in greeks_rows {
        snapshots.push(DeribitFeatureSnapshot {
            symbol: deribit_currency_to_symbol(&currency),
            ts,
            mark_iv: mark_iv.unwrap_or(f64::NAN),
            bid_iv: f64::NAN,
            ask_iv: f64::NAN,
            underlying_price: underlying_price.unwrap_or(f64::NAN),
            delta: delta.unwrap_or(f64::NAN),
            gamma: gamma.unwrap_or(f64::NAN),
            vega: vega.unwrap_or(f64::NAN),
            theta: theta.unwrap_or(f64::NAN),
        });
    }

    snapshots.sort_by_key(|snapshot| (snapshot.symbol.clone(), snapshot.ts));
    snapshots
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
