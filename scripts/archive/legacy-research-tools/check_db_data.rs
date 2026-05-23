#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! tokio = { version = "1", features = ["full"] }
//! sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "chrono"] }
//! chrono = "0.4"
//! ```

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = "postgresql://postgres:postgres@localhost:5432/ploy";
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    println!("=== Database Data Completeness Check ===\n");

    // Check table existence
    let tables = vec![
        "sync_records",
        "binance_price_ticks",
        "clob_quote_ticks",
        "pm_market_metadata",
        "binance_lob_ticks",
    ];

    for table in &tables {
        let exists: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = '{}')",
            table
        ))
        .fetch_one(&pool)
        .await?;

        println!("Table '{}': {}", table, if exists { "EXISTS" } else { "MISSING" });
    }

    println!("\n=== Data Range Analysis ===\n");

    // Symbols to check
    let symbols = vec!["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT"];

    // Check binance_price_ticks
    println!("--- binance_price_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM binance_price_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            println!("  {}: {} rows, {} to {}",
                symbol,
                count,
                min_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string()),
                max_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string())
            );
        }
    }

    // Check clob_quote_ticks
    println!("\n--- clob_quote_ticks ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM clob_quote_ticks"
    )
    .fetch_optional(&pool)
    .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!("  Total: {} rows, {} to {}",
            count,
            min_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string()),
            max_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string())
        );
    }

    // Check pm_market_metadata
    println!("\n--- pm_market_metadata ---");
    let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT COUNT(*), MIN(start_time), MAX(end_time) FROM pm_market_metadata"
    )
    .fetch_optional(&pool)
    .await?;

    if let Some((count, min_ts, max_ts)) = result {
        println!("  Total: {} markets, {} to {}",
            count,
            min_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string()),
            max_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string())
        );
    }

    // Check binance_lob_ticks
    println!("\n--- binance_lob_ticks ---");
    for symbol in &symbols {
        let result: Option<(i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM binance_lob_ticks WHERE symbol = $1"
        )
        .bind(symbol)
        .fetch_optional(&pool)
        .await?;

        if let Some((count, min_ts, max_ts)) = result {
            println!("  {}: {} rows, {} to {}",
                symbol,
                count,
                min_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string()),
                max_ts.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string())
            );
        }
    }

    Ok(())
}
