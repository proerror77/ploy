use super::settlement_refresh::refresh_pm_token_settlements_for_tokens;
use crate::adapters::PostgresStore;
use crate::cli::strategy::CryptoLobDatasetFormat;
use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::Row;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct CryptoLobDatasetRow {
    executed_at: DateTime<Utc>,
    intent_id: uuid::Uuid,
    account_id: String,
    agent_id: String,
    market_slug: String,
    token_id: String,
    market_side: String,
    is_buy: bool,
    limit_price: Decimal,
    p_up: Option<f64>,
    obi5: f64,
    obi10: f64,
    spread_bps: f64,
    bid_volume_5: f64,
    ask_volume_5: f64,
    momentum_1s: f64,
    momentum_5s: f64,
    pm_up_ask: Option<f64>,
    pm_down_ask: Option<f64>,
    settled_price: Decimal,
    y_up: i32,
    model_type: String,
    model_version: String,
    config_hash: String,
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('\"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('\"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_crypto_lob_dataset_csv(
    output: &Path,
    rows: &[CryptoLobDatasetRow],
) -> anyhow::Result<()> {
    let mut f = std::fs::File::create(output).context("Failed to create output file")?;
    writeln!(
        f,
        "executed_at,intent_id,account_id,agent_id,market_slug,token_id,market_side,is_buy,limit_price,p_up,obi5,obi10,spread_bps,bid_volume_5,ask_volume_5,momentum_1s,momentum_5s,pm_up_ask,pm_down_ask,settled_price,y_up,model_type,model_version,config_hash"
    )?;

    for r in rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&r.executed_at.to_rfc3339()),
            csv_escape(&r.intent_id.to_string()),
            csv_escape(&r.account_id),
            csv_escape(&r.agent_id),
            csv_escape(&r.market_slug),
            csv_escape(&r.token_id),
            csv_escape(&r.market_side),
            if r.is_buy { "1" } else { "0" },
            r.limit_price,
            r.p_up.map(|v| format!("{v:.6}")).unwrap_or_default(),
            format!("{:.10}", r.obi5),
            format!("{:.10}", r.obi10),
            format!("{:.10}", r.spread_bps),
            format!("{:.10}", r.bid_volume_5),
            format!("{:.10}", r.ask_volume_5),
            format!("{:.10}", r.momentum_1s),
            format!("{:.10}", r.momentum_5s),
            r.pm_up_ask.map(|v| format!("{v:.10}")).unwrap_or_default(),
            r.pm_down_ask
                .map(|v| format!("{v:.10}"))
                .unwrap_or_default(),
            r.settled_price,
            r.y_up,
            csv_escape(&r.model_type),
            csv_escape(&r.model_version),
            csv_escape(&r.config_hash),
        )?;
    }

    Ok(())
}

#[cfg(feature = "analysis")]
fn sanitize_duckdb_copy_path(path: &Path) -> std::result::Result<String, duckdb::Error> {
    let s = path.display().to_string();
    if s.contains('\'') || s.contains(';') || s.contains("--") {
        return Err(duckdb::Error::InvalidParameterName(
            "path contains SQL metacharacters".into(),
        ));
    }
    Ok(s)
}

#[cfg(feature = "analysis")]
fn write_crypto_lob_dataset_parquet(
    output: &Path,
    rows: &[CryptoLobDatasetRow],
) -> anyhow::Result<()> {
    use duckdb::{params, Connection};
    use rust_decimal::prelude::ToPrimitive;

    let conn = Connection::open_in_memory().context("Failed to open DuckDB")?;
    conn.execute_batch(
        r#"
        CREATE TABLE dataset (
          executed_at VARCHAR,
          intent_id VARCHAR,
          account_id VARCHAR,
          agent_id VARCHAR,
          market_slug VARCHAR,
          token_id VARCHAR,
          market_side VARCHAR,
          is_buy BOOLEAN,
          limit_price DOUBLE,
          p_up DOUBLE,
          obi5 DOUBLE,
          obi10 DOUBLE,
          spread_bps DOUBLE,
          bid_volume_5 DOUBLE,
          ask_volume_5 DOUBLE,
          momentum_1s DOUBLE,
          momentum_5s DOUBLE,
          pm_up_ask DOUBLE,
          pm_down_ask DOUBLE,
          settled_price DOUBLE,
          y_up INTEGER,
          model_type VARCHAR,
          model_version VARCHAR,
          config_hash VARCHAR
        );
        "#,
    )
    .context("Failed to create DuckDB dataset table")?;

    let mut stmt = conn
        .prepare(
            r#"
            INSERT INTO dataset VALUES (
              ?,?,?,?,?,?,?,?,
              ?,?,?,?,?,?,?,?,
              ?,?,?,?,?,?,?,?
            )
            "#,
        )
        .context("Failed to prepare DuckDB insert statement")?;

    for r in rows {
        let limit_price = r
            .limit_price
            .to_f64()
            .context("Failed to convert limit_price to f64")?;
        let settled_price = r
            .settled_price
            .to_f64()
            .context("Failed to convert settled_price to f64")?;

        stmt.execute(params![
            r.executed_at.to_rfc3339(),
            r.intent_id.to_string(),
            r.account_id.as_str(),
            r.agent_id.as_str(),
            r.market_slug.as_str(),
            r.token_id.as_str(),
            r.market_side.as_str(),
            r.is_buy,
            limit_price,
            r.p_up,
            r.obi5,
            r.obi10,
            r.spread_bps,
            r.bid_volume_5,
            r.ask_volume_5,
            r.momentum_1s,
            r.momentum_5s,
            r.pm_up_ask,
            r.pm_down_ask,
            settled_price,
            r.y_up,
            r.model_type.as_str(),
            r.model_version.as_str(),
            r.config_hash.as_str(),
        ])
        .context("Failed to insert row into DuckDB")?;
    }

    if output.exists() {
        std::fs::remove_file(output).context("Failed to remove existing output file")?;
    }
    let out = sanitize_duckdb_copy_path(output).context("Invalid output path for DuckDB COPY")?;
    let copy_sql = format!("COPY dataset TO '{out}' (FORMAT PARQUET);");
    conn.execute_batch(&copy_sql)
        .context("Failed to COPY dataset to Parquet")?;

    Ok(())
}

pub(super) async fn export_crypto_lob_dataset(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    no_refresh: bool,
    limit: usize,
    format: CryptoLobDatasetFormat,
    output: Option<PathBuf>,
    database_url: Option<String>,
) -> anyhow::Result<()> {
    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let output: PathBuf = output.unwrap_or_else(|| match format {
        CryptoLobDatasetFormat::Csv => PathBuf::from("./data/crypto_lob_dataset.csv"),
        CryptoLobDatasetFormat::Parquet => PathBuf::from("./data/crypto_lob_dataset.parquet"),
    });

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Export Dataset (crypto LOB)                                  ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} account_id={} agent_id={} live_only={} limit={} refresh={} format={:?} output={}",
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh,
        format,
        output.display()
    );

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::persistence::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    let rows = sqlx::query(
        r#"
        SELECT
            executed_at,
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            market_side,
            is_buy,
            limit_price,
            dry_run,
            filled_shares,
            metadata
        FROM agent_order_executions
        WHERE executed_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND filled_shares > 0
          AND (
                (metadata ? 'signal_type' AND RIGHT(metadata->>'signal_type', 6) = '_entry')
             OR (NOT (metadata ? 'signal_type') AND is_buy = TRUE)
          )
          AND LOWER(domain) = 'crypto'
          AND ($2::text IS NULL OR account_id = $2)
          AND ($3::text IS NULL OR agent_id = $3)
          AND ($4::bool = FALSE OR dry_run = FALSE)
        ORDER BY executed_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query agent_order_executions")?;

    if rows.is_empty() {
        println!("\n  No filled entry intents found in this window.\n");
        return Ok(());
    }

    let mut token_ids: Vec<String> = Vec::with_capacity(rows.len());
    for row in &rows {
        let token_id: String = row.get("token_id");
        token_ids.push(token_id);
    }
    token_ids.sort();
    token_ids.dedup();

    if !no_refresh {
        const MAX_REFRESH: usize = 500;
        let summary =
            refresh_pm_token_settlements_for_tokens(store.pool(), &token_ids, MAX_REFRESH).await?;

        if summary.requested_tokens > 0 {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                summary.requested_tokens
            );
        }
        if summary.refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows",
                summary.refreshed_markets, summary.refreshed_tokens
            );
        }
    }

    let scored_rows = sqlx::query(
        r#"
        SELECT
            e.executed_at,
            e.intent_id,
            e.agent_id,
            e.account_id,
            e.market_slug,
            e.token_id,
            e.market_side,
            e.is_buy,
            e.limit_price,
            e.dry_run,
            e.metadata,
            s.resolved as pm_resolved,
            s.settled_price as pm_settled_price,
            s.outcome as pm_outcome
        FROM agent_order_executions e
        LEFT JOIN pm_token_settlements s
          ON s.token_id = e.token_id
        WHERE e.executed_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND e.filled_shares > 0
          AND (
                (e.metadata ? 'signal_type' AND RIGHT(e.metadata->>'signal_type', 6) = '_entry')
             OR (NOT (e.metadata ? 'signal_type') AND e.is_buy = TRUE)
          )
          AND LOWER(e.domain) = 'crypto'
          AND ($2::text IS NULL OR e.account_id = $2)
          AND ($3::text IS NULL OR e.agent_id = $3)
          AND ($4::bool = FALSE OR e.dry_run = FALSE)
        ORDER BY e.executed_at DESC
        LIMIT $5
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query joined export rows")?;

    if scored_rows.is_empty() {
        bail!("no rows returned for export query (unexpected)");
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).context("Failed to create output directory")?;
        }
    }

    fn meta_f64(meta: &serde_json::Value, key: &str) -> Option<f64> {
        let v = meta.get(key)?;
        match v {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
        .filter(|x| x.is_finite())
    }

    fn meta_str<'a>(meta: &'a serde_json::Value, key: &str) -> Option<&'a str> {
        meta.get(key).and_then(|v| v.as_str())
    }

    let mut dataset: Vec<CryptoLobDatasetRow> = Vec::new();
    let mut skipped_pending = 0usize;
    let mut skipped_missing = 0usize;

    for row in &scored_rows {
        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let is_resolved = resolved.unwrap_or(false) && settled_price.is_some();
        if !is_resolved {
            skipped_pending += 1;
            continue;
        }

        let sp = settled_price.unwrap_or(Decimal::ZERO);
        let market_side: String = row.get("market_side");
        let y_up: i32 = match market_side.as_str() {
            "UP" => {
                if sp > dec!(0.5) {
                    1
                } else {
                    0
                }
            }
            "DOWN" => {
                if sp > dec!(0.5) {
                    0
                } else {
                    1
                }
            }
            _ => continue,
        };

        let meta: serde_json::Value = row.try_get("metadata").unwrap_or(serde_json::Value::Null);

        let p_up = meta_f64(&meta, "p_up");
        let obi5 = meta_f64(&meta, "lob_obi_5");
        let obi10 = meta_f64(&meta, "lob_obi_10");
        let spread = meta_f64(&meta, "lob_spread_bps");
        let bidv5 = meta_f64(&meta, "lob_bid_volume_5");
        let askv5 = meta_f64(&meta, "lob_ask_volume_5");
        let m1 = meta_f64(&meta, "signal_momentum_1s");
        let m5 = meta_f64(&meta, "signal_momentum_5s");
        let pm_up_ask = meta_f64(&meta, "pm_up_ask");
        let pm_down_ask = meta_f64(&meta, "pm_down_ask");
        let model_type = meta_str(&meta, "model_type").unwrap_or("").to_string();
        let model_version = meta_str(&meta, "model_version").unwrap_or("").to_string();
        let config_hash = meta_str(&meta, "config_hash").unwrap_or("").to_string();

        if obi5.is_none()
            || obi10.is_none()
            || spread.is_none()
            || bidv5.is_none()
            || askv5.is_none()
            || m1.is_none()
            || m5.is_none()
        {
            skipped_missing += 1;
            continue;
        }

        let executed_at: DateTime<Utc> = row.get("executed_at");
        let intent_id: uuid::Uuid = row.get("intent_id");
        let account_id: String = row.get("account_id");
        let agent_id: String = row.get("agent_id");
        let market_slug: String = row.get("market_slug");
        let token_id: String = row.get("token_id");
        let is_buy: bool = row.get("is_buy");
        let limit_price: Decimal = row.get("limit_price");

        dataset.push(CryptoLobDatasetRow {
            executed_at,
            intent_id,
            account_id,
            agent_id,
            market_slug,
            token_id,
            market_side,
            is_buy,
            limit_price,
            p_up,
            obi5: obi5.unwrap_or(0.0),
            obi10: obi10.unwrap_or(0.0),
            spread_bps: spread.unwrap_or(0.0),
            bid_volume_5: bidv5.unwrap_or(0.0),
            ask_volume_5: askv5.unwrap_or(0.0),
            momentum_1s: m1.unwrap_or(0.0),
            momentum_5s: m5.unwrap_or(0.0),
            pm_up_ask,
            pm_down_ask,
            settled_price: sp,
            y_up,
            model_type,
            model_version,
            config_hash,
        });
    }

    if dataset.is_empty() {
        println!("\n  No resolved rows to export (all pending/missing features).\n");
        return Ok(());
    }

    match format {
        CryptoLobDatasetFormat::Csv => write_crypto_lob_dataset_csv(output.as_path(), &dataset)?,
        CryptoLobDatasetFormat::Parquet => {
            #[cfg(feature = "analysis")]
            {
                write_crypto_lob_dataset_parquet(output.as_path(), &dataset)?;
            }
            #[cfg(not(feature = "analysis"))]
            {
                bail!("parquet export requires building with --features analysis");
            }
        }
    }

    println!("\n  Export complete:");
    println!("  - exported_rows:    {}", dataset.len());
    println!("  - skipped_pending:  {}", skipped_pending);
    println!("  - skipped_missing:  {}", skipped_missing);
    println!("  - output:           {}", output.display());
    println!();

    Ok(())
}
