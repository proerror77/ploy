use chrono::{DateTime, TimeZone, Utc};
use ploy::adapters::PostgresStore;
use ploy::config::AppConfig;
use ploy::error::{PloyError, Result};
use serde::Deserialize;
use sqlx::postgres::PgPool;
use sqlx::{Postgres, QueryBuilder};
use std::time::Duration;
use tracing::{debug, info, warn};

const DEFAULT_DERIBIT_PUBLIC_API: &str = "https://www.deribit.com/api/v2/public";
const DEFAULT_SOURCE: &str = "deribit_volatility_index";

#[derive(Debug, Deserialize)]
struct DeribitRpcResponse<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct DeribitVolIndexPage {
    /// [ts_ms, open, high, low, close]
    #[serde(default)]
    data: Vec<(i64, f64, f64, f64, f64)>,
    #[serde(default)]
    continuation: Option<i64>,
}

#[derive(Debug, Clone)]
struct DeribitIvBar {
    ts: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

#[derive(Debug, Clone, Copy)]
enum InsertValue {
    Timestamp,
    Currency,
    ResolutionSecs,
    Source,
    IvOpen,
    IvHigh,
    IvLow,
    IvClose,
    IvSingle,
}

#[derive(Debug, Clone)]
struct InsertColumn {
    name: String,
    value: InsertValue,
}

#[derive(Debug, Clone)]
struct InsertPlan {
    columns: Vec<InsertColumn>,
}

pub async fn run_deribit_iv_backfill_mode(
    config_path: &str,
    currencies: &str,
    start_rfc3339: Option<&str>,
    end_rfc3339: Option<&str>,
    lookback_days: u64,
    resolution_secs: u32,
    sleep_ms: u64,
    base_url: &str,
    dry_run: bool,
) -> Result<()> {
    let base_url = base_url.trim();
    let base_url = if base_url.is_empty() {
        DEFAULT_DERIBIT_PUBLIC_API
    } else {
        base_url
    };

    // Load config for DB URL (accept either config dir or config file).
    let cfg = AppConfig::load_from(config_path).or_else(|_| AppConfig::load())?;
    let store = PostgresStore::new(&cfg.database.url, cfg.database.max_connections).await?;
    let pool = store.pool();

    let end_dt = match end_rfc3339.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => parse_rfc3339(raw)?,
        None => Utc::now(),
    };
    let start_dt = match start_rfc3339.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => parse_rfc3339(raw)?,
        None => {
            let days = i64::try_from(lookback_days).map_err(|_| {
                PloyError::Validation(format!("lookback_days too large: {lookback_days}"))
            })?;
            end_dt - chrono::Duration::days(days)
        }
    };

    if start_dt >= end_dt {
        return Err(PloyError::Validation(format!(
            "invalid range: start ({}) must be < end ({})",
            start_dt.to_rfc3339(),
            end_dt.to_rfc3339()
        )));
    }

    let currency_list: Vec<String> = currencies
        .split(',')
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    if currency_list.is_empty() {
        return Err(PloyError::Validation(
            "no currencies provided; use --currencies BTC,ETH".to_string(),
        ));
    }

    info!(
        currencies = %currency_list.join(","),
        start = %start_dt.to_rfc3339(),
        end = %end_dt.to_rfc3339(),
        resolution_secs,
        dry_run,
        base_url,
        "Starting Deribit IV backfill"
    );

    let table_exists = deribit_iv_table_exists(pool).await?;
    if !table_exists && !dry_run {
        ensure_deribit_iv_table(pool).await?;
    } else if !table_exists && dry_run {
        warn!("deribit_iv_ticks table does not exist (dry_run=true so it will not be created)");
    }

    let insert_plan = if table_exists || !dry_run {
        Some(build_insert_plan(pool).await?)
    } else {
        None
    };

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| PloyError::Internal(format!("reqwest client init failed: {e}")))?;

    for currency in currency_list {
        backfill_currency(
            pool,
            &http,
            base_url,
            &currency,
            start_dt,
            end_dt,
            resolution_secs,
            sleep_ms,
            dry_run,
            insert_plan.as_ref(),
        )
        .await?;
    }

    info!("Deribit IV backfill finished");
    Ok(())
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| PloyError::Validation(format!("invalid RFC3339 timestamp \"{raw}\": {e}")))
}

async fn deribit_iv_table_exists(pool: &PgPool) -> Result<bool> {
    let reg: Option<String> = sqlx::query_scalar("SELECT to_regclass('public.deribit_iv_ticks')::text")
        .fetch_one(pool)
        .await?;
    Ok(reg.is_some())
}

async fn ensure_deribit_iv_table(pool: &PgPool) -> Result<()> {
    // Create with a schema that supports both "single IV tick" and OHLC bars.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS deribit_iv_ticks (
            id BIGSERIAL PRIMARY KEY,
            timestamp TIMESTAMPTZ NOT NULL,
            currency TEXT NOT NULL,
            iv_open DOUBLE PRECISION,
            iv_high DOUBLE PRECISION,
            iv_low DOUBLE PRECISION,
            iv_close DOUBLE PRECISION,
            atm_iv DOUBLE PRECISION,
            resolution_secs INT NOT NULL DEFAULT 60,
            source TEXT NOT NULL DEFAULT 'deribit_volatility_index',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS uniq_deribit_iv_ticks
            ON deribit_iv_ticks(currency, resolution_secs, timestamp, source)
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_deribit_iv_ticks_ts
            ON deribit_iv_ticks(timestamp DESC)
        "#,
    )
    .execute(pool)
    .await?;

    info!("Ensured deribit_iv_ticks table");
    Ok(())
}

async fn build_insert_plan(pool: &PgPool) -> Result<InsertPlan> {
    let cols: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'deribit_iv_ticks'
        ORDER BY ordinal_position
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut by_name = std::collections::HashSet::new();
    for c in cols {
        by_name.insert(c);
    }

    let mut columns: Vec<InsertColumn> = Vec::new();

    // Timestamp column(s)
    for name in ["timestamp", "ts"] {
        if by_name.contains(name) {
            columns.push(InsertColumn {
                name: name.to_string(),
                value: InsertValue::Timestamp,
            });
        }
    }

    // Currency/Symbol column(s)
    for name in ["currency", "symbol"] {
        if by_name.contains(name) {
            columns.push(InsertColumn {
                name: name.to_string(),
                value: InsertValue::Currency,
            });
        }
    }

    // Preferred: single ATM IV column (close)
    for name in ["atm_iv", "iv", "mark_iv", "dvol"] {
        if by_name.contains(name) {
            columns.push(InsertColumn {
                name: name.to_string(),
                value: InsertValue::IvSingle,
            });
        }
    }

    // Optional OHLC columns
    let ohlc = [
        ("iv_open", InsertValue::IvOpen),
        ("iv_high", InsertValue::IvHigh),
        ("iv_low", InsertValue::IvLow),
        ("iv_close", InsertValue::IvClose),
    ];
    for (name, value) in ohlc {
        if by_name.contains(name) {
            columns.push(InsertColumn {
                name: name.to_string(),
                value,
            });
        }
    }

    // Optional metadata columns
    if by_name.contains("resolution_secs") {
        columns.push(InsertColumn {
            name: "resolution_secs".to_string(),
            value: InsertValue::ResolutionSecs,
        });
    }
    if by_name.contains("source") {
        columns.push(InsertColumn {
            name: "source".to_string(),
            value: InsertValue::Source,
        });
    }

    // Validate: must have at least timestamp + currency + iv.
    let has_ts = columns.iter().any(|c| matches!(c.value, InsertValue::Timestamp));
    let has_ccy = columns.iter().any(|c| matches!(c.value, InsertValue::Currency));
    let has_iv = columns.iter().any(|c| {
        matches!(
            c.value,
            InsertValue::IvSingle | InsertValue::IvOpen | InsertValue::IvHigh | InsertValue::IvLow | InsertValue::IvClose
        )
    });

    if !has_ts || !has_ccy || !has_iv {
        return Err(PloyError::Validation(format!(
            "deribit_iv_ticks schema not supported; need timestamp+currency+iv columns, found columns: {:?}",
            columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        )));
    }

    Ok(InsertPlan { columns })
}

async fn backfill_currency(
    pool: &PgPool,
    http: &reqwest::Client,
    base_url: &str,
    currency: &str,
    start_dt: DateTime<Utc>,
    end_dt: DateTime<Utc>,
    resolution_secs: u32,
    sleep_ms: u64,
    dry_run: bool,
    insert_plan: Option<&InsertPlan>,
) -> Result<()> {
    let start_ms = start_dt.timestamp_millis();
    let mut end_ms = end_dt.timestamp_millis();
    let mut total_seen: u64 = 0;
    let mut total_inserted: u64 = 0;

    info!(
        currency,
        start = %start_dt.to_rfc3339(),
        end = %end_dt.to_rfc3339(),
        resolution_secs,
        "Backfilling Deribit volatility index"
    );

    for page_idx in 0.. {
        let page = fetch_vol_index_page(http, base_url, currency, start_ms, end_ms, resolution_secs)
            .await?;

        if page.data.is_empty() {
            debug!(
                currency,
                page_idx,
                start_ms,
                end_ms,
                continuation = ?page.continuation,
                "Empty Deribit page"
            );
            if let Some(cont) = page.continuation {
                if cont >= end_ms || cont <= start_ms {
                    break;
                }
                end_ms = cont;
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                continue;
            }
            break;
        }

        let mut bars: Vec<DeribitIvBar> = Vec::with_capacity(page.data.len());
        for (ts_ms, o, h, l, c) in page.data {
            let Some(ts) = Utc.timestamp_millis_opt(ts_ms).single() else {
                continue;
            };
            let open = normalize_iv(o);
            let high = normalize_iv(h);
            let low = normalize_iv(l);
            let close = normalize_iv(c);
            if !(open.is_finite() && high.is_finite() && low.is_finite() && close.is_finite()) {
                continue;
            }
            bars.push(DeribitIvBar {
                ts,
                open,
                high,
                low,
                close,
            });
        }

        total_seen += bars.len() as u64;

        if !dry_run {
            let plan = insert_plan.ok_or_else(|| {
                PloyError::Internal("insert_plan missing (this is a bug)".to_string())
            })?;
            let inserted = insert_bars(pool, plan, currency, resolution_secs, DEFAULT_SOURCE, &bars)
                .await?;
            total_inserted += inserted;
        }

        if page_idx % 10 == 0 {
            info!(
                currency,
                page_idx,
                end_ms,
                seen = total_seen,
                inserted = total_inserted,
                continuation = ?page.continuation,
                "Deribit IV backfill progress"
            );
        }

        let Some(cont) = page.continuation else {
            break;
        };

        if cont >= end_ms {
            // Defensive: no progress.
            break;
        }

        if cont <= start_ms {
            break;
        }

        end_ms = cont;
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
    }

    info!(
        currency,
        seen = total_seen,
        inserted = total_inserted,
        dry_run,
        "Finished currency backfill"
    );

    Ok(())
}

async fn fetch_vol_index_page(
    http: &reqwest::Client,
    base_url: &str,
    currency: &str,
    start_ms: i64,
    end_ms: i64,
    resolution_secs: u32,
) -> Result<DeribitVolIndexPage> {
    let url = format!(
        "{}/get_volatility_index_data?currency={}&start_timestamp={}&end_timestamp={}&resolution={}",
        base_url,
        currency.to_ascii_uppercase(),
        start_ms,
        end_ms,
        resolution_secs
    );

    let resp = http.get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(PloyError::Internal(format!(
            "Deribit get_volatility_index_data failed (status={status}): {body}"
        )));
    }

    let body: DeribitRpcResponse<DeribitVolIndexPage> = resp.json().await?;
    Ok(body.result)
}

fn normalize_iv(raw: f64) -> f64 {
    if raw > 3.0 {
        raw / 100.0
    } else {
        raw
    }
}

async fn insert_bars(
    pool: &PgPool,
    plan: &InsertPlan,
    currency: &str,
    resolution_secs: u32,
    source: &str,
    bars: &[DeribitIvBar],
) -> Result<u64> {
    if bars.is_empty() {
        return Ok(0);
    }

    // Build: INSERT INTO deribit_iv_ticks (col1, col2, ...) VALUES ...
    let cols_joined = plan
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "INSERT INTO deribit_iv_ticks ({cols_joined}) "
    ));

    qb.push_values(bars.iter(), |mut b, bar| {
        for col in &plan.columns {
            match col.value {
                InsertValue::Timestamp => {
                    b.push_bind(bar.ts);
                }
                InsertValue::Currency => {
                    b.push_bind(currency);
                }
                InsertValue::ResolutionSecs => {
                    b.push_bind(i32::try_from(resolution_secs).unwrap_or(i32::MAX));
                }
                InsertValue::Source => {
                    b.push_bind(source);
                }
                InsertValue::IvOpen => {
                    b.push_bind(bar.open);
                }
                InsertValue::IvHigh => {
                    b.push_bind(bar.high);
                }
                InsertValue::IvLow => {
                    b.push_bind(bar.low);
                }
                InsertValue::IvClose => {
                    b.push_bind(bar.close);
                }
                InsertValue::IvSingle => {
                    b.push_bind(bar.close);
                }
            }
        }
    });

    // Be conservative: if the table has a unique constraint, skip duplicates.
    qb.push(" ON CONFLICT DO NOTHING");

    let result = qb.build().execute(pool).await?;
    Ok(result.rows_affected())
}
