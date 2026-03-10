use super::*;

mod pm_backfill;
mod support;

pub(super) use pm_backfill::{backfill_pm_replay_tables, backfill_pm_token_settlements};
pub(super) use support::{run_integrity_check, run_nba_comeback, seed_nba_stats};

// ─────────────────────────────────────────────────────────────
// Backfill handlers
// ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) async fn backfill_klines(
    symbols: &str,
    from: &str,
    to: &str,
    interval: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::collector::BinanceKlineClient;
    use chrono::DateTime;

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();
    if symbol_list.is_empty() {
        anyhow::bail!("No symbols provided");
    }

    let from_dt = DateTime::parse_from_rfc3339(from)
        .or_else(|_| DateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --from date (expected ISO 8601, e.g. 2026-02-20T00:00:00Z)")?;

    let to_dt = DateTime::parse_from_rfc3339(to)
        .or_else(|_| DateTime::parse_from_str(to, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --to date (expected ISO 8601, e.g. 2026-02-28T00:00:00Z)")?;

    if to_dt <= from_dt {
        anyhow::bail!("--to must be after --from");
    }

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    // Ensure binance_klines table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_klines (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            interval TEXT NOT NULL,
            open_time TIMESTAMPTZ NOT NULL,
            close_time TIMESTAMPTZ NOT NULL,
            open NUMERIC(20,10) NOT NULL,
            high NUMERIC(20,10) NOT NULL,
            low NUMERIC(20,10) NOT NULL,
            close NUMERIC(20,10) NOT NULL,
            volume NUMERIC(20,10) NOT NULL,
            quote_volume NUMERIC(20,10) NOT NULL,
            trades BIGINT NOT NULL DEFAULT 0,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (symbol, interval, open_time)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure binance_klines table")?;

    let client = BinanceKlineClient::new();

    println!(
        "\nBackfilling klines: {} symbols, interval={}, {} → {}",
        symbol_list.len(),
        interval,
        from_dt.format("%Y-%m-%d"),
        to_dt.format("%Y-%m-%d")
    );

    let mut grand_total = 0usize;
    for sym in &symbol_list {
        print!("  {} ... ", sym);
        std::io::stdout().flush().ok();

        let klines = client
            .fetch_klines_range(sym, interval, from_dt, to_dt)
            .await
            .with_context(|| format!("Failed to fetch klines for {}", sym))?;

        let fetched = klines.len();
        let saved = BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines)
            .await
            .with_context(|| format!("Failed to save klines for {}", sym))?;

        println!("{} fetched, {} new", fetched, saved);
        grand_total += saved;
    }

    println!("\nDone. {} new klines inserted total.\n", grand_total);
    Ok(())
}
