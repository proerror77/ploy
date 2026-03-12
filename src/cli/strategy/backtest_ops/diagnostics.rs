use super::*;
use alloy::primitives::U256;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;
use sqlx::Row;
use std::collections::{HashMap, HashSet};

use crate::strategy::backtest_feed::ReplayEventWindow;

const STRICT_SPOT_TAIL_SECS: i64 = 15;
const STRICT_L2_TAIL_SECS: i64 = 15;
const STRICT_PM_QUOTE_TAIL_SECS: i64 = 30;
const STRICT_PM_LOB_TAIL_SECS: i64 = 30;
const EVENT_AUDIT_PREVIEW_ROWS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventWindowQuality {
    KeepStrict,
    KeepResearch,
    Drop,
}

impl EventWindowQuality {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeepStrict => "KEEP_STRICT",
            Self::KeepResearch => "KEEP_RESEARCH",
            Self::Drop => "DROP",
        }
    }

    fn meets_minimum(self, minimum: PmReplayQuality) -> bool {
        match minimum {
            PmReplayQuality::Strict => self == Self::KeepStrict,
            PmReplayQuality::Research => matches!(self, Self::KeepStrict | Self::KeepResearch),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SourceCoverage {
    rows: i64,
    distinct_tokens: i64,
    first_ts: Option<DateTime<Utc>>,
    last_ts: Option<DateTime<Utc>>,
}

impl SourceCoverage {
    fn tail_gap_secs(&self, end_time: Option<DateTime<Utc>>) -> Option<i64> {
        end_time.zip(self.last_ts).map(|(end_time, last_ts)| {
            if last_ts >= end_time {
                0
            } else {
                (end_time - last_ts).num_seconds()
            }
        })
    }
}

#[derive(Debug, Clone)]
struct EventWindowAuditRow {
    market_slug: String,
    symbol: String,
    horizon: String,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    corrected_end_time: Option<DateTime<Utc>>,
    price_to_beat: Option<Decimal>,
    expected_token_count: usize,
    quote: SourceCoverage,
    lob: SourceCoverage,
    spot: SourceCoverage,
    l2: SourceCoverage,
    quality: EventWindowQuality,
    issues: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub(super) struct PmEventReplaySelection {
    pub(super) minimum_quality: PmReplayQuality,
    pub(super) total_windows: usize,
    pub(super) kept_windows: usize,
    pub(super) kept_strict_windows: usize,
    pub(super) kept_research_windows: usize,
    pub(super) dropped_windows: usize,
    pub(super) effective_from: Option<DateTime<Utc>>,
    pub(super) effective_to: Option<DateTime<Utc>>,
    pub(super) windows: Vec<ReplayEventWindow>,
}

pub(super) async fn print_backtest_db_diagnostics(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    println!("\n=== Backtest DB diagnostics ===");
    println!("symbols: {}", symbols.join(", "));
    println!("from: {}", fmt_ts(from));
    println!("to:   {}", fmt_ts(to));

    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

    if !table_exists(pool, "sync_records").await? {
        println!("\n[sync_records] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(timestamp),
              MAX(timestamp),
              COUNT(DISTINCT pm_market_slug)::bigint
            FROM sync_records
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR timestamp >= $2)
              AND ($3::timestamptz IS NULL OR timestamp <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, slugs)) => {
                println!("\n[sync_records]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct pm_market_slug: {slugs}");
            }
            Err(e) => {
                println!("\n[sync_records] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "binance_price_ticks").await? {
        println!("\n[binance_price_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT COUNT(*)::bigint, MIN(trade_time), MAX(trade_time)
            FROM binance_price_ticks
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR trade_time >= $2)
              AND ($3::timestamptz IS NULL OR trade_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts)) => {
                println!("\n[binance_price_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[binance_price_ticks] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "binance_klines").await? {
        println!("\n[binance_klines] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(open_time),
              MAX(close_time),
              COUNT(DISTINCT interval)::bigint
            FROM binance_klines
            WHERE ($1::text[] IS NULL OR symbol = ANY($1))
              AND ($2::timestamptz IS NULL OR close_time >= $2)
              AND ($3::timestamptz IS NULL OR open_time <= $3)
            "#,
        )
        .bind(symbol_list.clone())
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, intervals)) => {
                println!("\n[binance_klines]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct intervals: {intervals}");
            }
            Err(e) => {
                println!("\n[binance_klines] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "clob_quote_ticks").await? {
        println!("\n[clob_quote_ticks] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_quote_ticks
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_quote_ticks]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_quote_ticks] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "clob_orderbook_snapshots").await? {
        println!("\n[clob_orderbook_snapshots] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              MIN(received_at),
              MAX(received_at),
              COUNT(DISTINCT token_id)::bigint
            FROM clob_orderbook_snapshots
            WHERE ($1::timestamptz IS NULL OR received_at >= $1)
              AND ($2::timestamptz IS NULL OR received_at <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, min_ts, max_ts, tokens)) => {
                println!("\n[clob_orderbook_snapshots]");
                println!(
                    "rows: {count}, ts_range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
                println!("distinct token_id: {tokens}");
            }
            Err(e) => {
                println!("\n[clob_orderbook_snapshots] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "pm_market_metadata").await? {
        println!("\n[pm_market_metadata] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(*) FILTER (WHERE start_time IS NOT NULL AND end_time IS NOT NULL)::bigint,
              COUNT(*) FILTER (WHERE price_to_beat IS NOT NULL AND price_to_beat > 0)::bigint,
              MIN(start_time),
              MAX(end_time)
            FROM pm_market_metadata
            WHERE ($1::timestamptz IS NULL OR end_time >= $1)
              AND ($2::timestamptz IS NULL OR start_time <= $2)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, windows, with_s0, min_ts, max_ts)) => {
                println!("\n[pm_market_metadata]");
                println!("rows: {count}, window_rows: {windows}, with price_to_beat>0: {with_s0}");
                println!("ts_range: {} .. {}", fmt_ts(min_ts), fmt_ts(max_ts));
            }
            Err(e) => {
                println!("\n[pm_market_metadata] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "pm_token_settlements").await? {
        println!("\n[pm_token_settlements] MISSING");
    } else {
        match sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
            r#"
            SELECT
              COUNT(*)::bigint,
              COUNT(DISTINCT market_slug)::bigint,
              COUNT(*) FILTER (WHERE resolved = true)::bigint,
              MIN(resolved_at),
              MAX(resolved_at)
            FROM pm_token_settlements
            WHERE ($1::timestamptz IS NULL OR resolved_at >= $1 OR resolved_at IS NULL)
              AND ($2::timestamptz IS NULL OR resolved_at <= $2 OR resolved_at IS NULL)
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(pool)
        .await
        {
            Ok((count, slugs, resolved, min_ts, max_ts)) => {
                println!("\n[pm_token_settlements]");
                println!("rows: {count}, distinct market_slug: {slugs}, resolved_rows: {resolved}");
                println!(
                    "resolved_at range: {} .. {}",
                    fmt_ts(min_ts),
                    fmt_ts(max_ts)
                );
            }
            Err(e) => {
                println!("\n[pm_token_settlements] query failed: {e}");
            }
        }
    }

    if !table_exists(pool, "deribit_iv_ticks").await? {
        println!("\n[deribit_iv_ticks] MISSING");
    } else {
        let mut printed = false;

        if let Ok((count, min_ts, max_ts, ccy)) =
            sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(timestamp),
                  MAX(timestamp),
                  COUNT(DISTINCT currency)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR timestamp >= $1)
                  AND ($2::timestamptz IS NULL OR timestamp <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
        {
            printed = true;
            println!("\n[deribit_iv_ticks]");
            println!(
                "rows: {count}, ts_range: {} .. {}",
                fmt_ts(min_ts),
                fmt_ts(max_ts)
            );
            println!("distinct currency: {ccy}");
        }

        if !printed {
            match sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64)>(
                r#"
                SELECT
                  COUNT(*)::bigint,
                  MIN(ts),
                  MAX(ts),
                  COUNT(DISTINCT symbol)::bigint
                FROM deribit_iv_ticks
                WHERE ($1::timestamptz IS NULL OR ts >= $1)
                  AND ($2::timestamptz IS NULL OR ts <= $2)
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_one(pool)
            .await
            {
                Ok((count, min_ts, max_ts, symbols)) => {
                    println!("\n[deribit_iv_ticks]");
                    println!(
                        "rows: {count}, ts_range: {} .. {}",
                        fmt_ts(min_ts),
                        fmt_ts(max_ts)
                    );
                    println!("distinct symbol: {symbols}");
                }
                Err(e) => {
                    println!("\n[deribit_iv_ticks] query failed: {e}");
                }
            }
        }
    }

    print_event_window_audit(pool, symbols, from, to).await?;

    println!("\nHint:");
    println!("- PM 5m backtest needs: clob_quote_ticks + pm_market_metadata (or pm_token_settlements.raw_market) + spot (sync_records or binance_price_ticks/klines).");
    println!("- Deribit IV (optional): populate deribit_iv_ticks (e.g. `ploy deribit-iv-backfill`) to enable IV-aware research/backtests.");

    Ok(())
}

pub(super) async fn verify_backtest_trades_gamma(
    pool: &sqlx::PgPool,
    run_id: uuid::Uuid,
) -> Result<()> {
    use super::super::settlement_ops::is_market_resolved;
    use crate::adapters::PolymarketClient;

    crate::persistence::ensure_pm_token_settlements_table(pool)
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    let trade_rows = sqlx::query(
        "SELECT id, symbol, direction, entry_time, exit_time, exit_reason, won
         FROM backtest_trades WHERE run_id = $1 ORDER BY entry_time",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .context("Failed to load backtest trades")?;

    if trade_rows.is_empty() {
        info!("No trades to verify");
        return Ok(());
    }

    struct TradeMapping {
        trade_id: i64,
        won: bool,
        direction: String,
        market_slug: String,
    }

    let mut mappings: Vec<TradeMapping> = Vec::new();
    let mut slugs_needed: HashSet<String> = HashSet::new();

    for row in &trade_rows {
        let trade_id: i64 = row.get("id");
        let symbol: String = row.get("symbol");
        let direction: String = row.get("direction");
        let entry_time: DateTime<Utc> = row.get("entry_time");
        let won: bool = row.get("won");

        let slug_row = sqlx::query_scalar::<_, String>(
            "SELECT market_slug FROM pm_market_metadata
             WHERE symbol = $1 AND start_time <= $2 AND end_time >= $2
             LIMIT 1",
        )
        .bind(&symbol)
        .bind(entry_time)
        .fetch_optional(pool)
        .await?;

        if let Some(slug) = slug_row {
            slugs_needed.insert(slug.clone());
            mappings.push(TradeMapping {
                trade_id,
                won,
                direction,
                market_slug: slug,
            });
        }
    }

    if mappings.is_empty() {
        info!("No trades could be mapped to market slugs");
        return Ok(());
    }

    let slugs_vec: Vec<String> = slugs_needed.into_iter().collect();
    let existing_settlements = sqlx::query(
        "SELECT token_id, market_slug, outcome, resolved, settled_price
         FROM pm_token_settlements WHERE market_slug = ANY($1)",
    )
    .bind(&slugs_vec)
    .fetch_all(pool)
    .await?;

    struct SettlementInfo {
        token_id: String,
        resolved: bool,
        settled_price: Option<Decimal>,
    }

    let mut slug_settlements: HashMap<String, HashMap<String, SettlementInfo>> = HashMap::new();
    for row in &existing_settlements {
        let slug: String = row.get("market_slug");
        let outcome: Option<String> = row.get("outcome");
        let token_id: String = row.get("token_id");
        let resolved: bool = row.get("resolved");
        let settled_price: Option<Decimal> = row.get("settled_price");
        if let Some(outcome) = outcome {
            slug_settlements.entry(slug).or_default().insert(
                outcome,
                SettlementInfo {
                    token_id,
                    resolved,
                    settled_price,
                },
            );
        }
    }

    let mut unresolved_tokens: Vec<String> = Vec::new();
    for settlements in slug_settlements.values() {
        for info in settlements.values() {
            if !info.resolved {
                unresolved_tokens.push(info.token_id.clone());
            }
        }
    }

    let mut missing_slugs: Vec<&str> = Vec::new();
    for slug in &slugs_vec {
        if !slug_settlements.contains_key(slug) {
            missing_slugs.push(slug);
        }
    }

    if !missing_slugs.is_empty() {
        let extra_tokens: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT s.token_id FROM pm_token_settlements s
             WHERE s.market_slug = ANY($1) AND s.resolved = false",
        )
        .bind(
            &missing_slugs
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        unresolved_tokens.extend(extra_tokens);
    }

    unresolved_tokens.sort();
    unresolved_tokens.dedup();

    if !unresolved_tokens.is_empty() {
        const MAX_REFRESH: usize = 500;
        let to_refresh = if unresolved_tokens.len() > MAX_REFRESH {
            &unresolved_tokens[..MAX_REFRESH]
        } else {
            &unresolved_tokens
        };

        println!(
            "\n  Refreshing settlement status for {} token(s) via Gamma...",
            to_refresh.len()
        );

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "gamma fetch failed");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                if !seen_conditions.insert(cond.to_string()) {
                    continue;
                }
            }

            let clob_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default();
            let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
            let price_strs: Vec<String> = market
                .outcome_prices
                .as_ref()
                .map(|ps| ps.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();

            if clob_ids.is_empty() || price_strs.is_empty() {
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(Utc::now);
            let raw_market = serde_json::json!({
                "id": market.id,
                "slug": market.slug,
                "closed": market.closed,
                "condition_id": market.condition_id,
            });

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                let _ = sqlx::query(
                    r#"INSERT INTO pm_token_settlements (
                        token_id, condition_id, market_id, market_slug, outcome,
                        settled_price, resolved, resolved_at, fetched_at, raw_market
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
                    ON CONFLICT (token_id) DO UPDATE SET
                        settled_price = EXCLUDED.settled_price,
                        resolved = EXCLUDED.resolved,
                        resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                        fetched_at = NOW(),
                        raw_market = EXCLUDED.raw_market"#,
                )
                .bind(tid)
                .bind(condition_id.as_deref())
                .bind(&market.id)
                .bind(market_slug.as_deref())
                .bind(outcome.as_deref())
                .bind(settled_price)
                .bind(resolved)
                .bind(resolved_at)
                .bind(sqlx::types::Json(raw_market.clone()))
                .execute(pool)
                .await;
            }
            refreshed += 1;
        }

        if refreshed > 0 {
            println!("  Refreshed {} market(s)\n", refreshed);
        }

        let refreshed_rows = sqlx::query(
            "SELECT token_id, market_slug, outcome, resolved, settled_price
             FROM pm_token_settlements WHERE market_slug = ANY($1)",
        )
        .bind(&slugs_vec)
        .fetch_all(pool)
        .await?;

        slug_settlements.clear();
        for row in &refreshed_rows {
            let slug: String = row.get("market_slug");
            let outcome: Option<String> = row.get("outcome");
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            let settled_price: Option<Decimal> = row.get("settled_price");
            if let Some(outcome) = outcome {
                slug_settlements.entry(slug).or_default().insert(
                    outcome,
                    SettlementInfo {
                        token_id,
                        resolved,
                        settled_price,
                    },
                );
            }
        }
    }

    let mut verified = 0usize;
    let mut matched = 0usize;
    let mut mismatched = 0usize;

    for mapping in &mappings {
        let Some(outcomes) = slug_settlements.get(&mapping.market_slug) else {
            continue;
        };

        let outcome_key = if mapping.direction == "UP" {
            "Up"
        } else {
            "Down"
        };
        let Some(info) = outcomes.get(outcome_key) else {
            continue;
        };

        if !info.resolved {
            continue;
        }

        let Some(settled_price) = info.settled_price else {
            continue;
        };

        let gamma_won = settled_price >= dec!(0.99);
        let gamma_match = mapping.won == gamma_won;

        sqlx::query(
            "UPDATE backtest_trades
             SET gamma_settled_price = $2, gamma_resolved = true, gamma_match = $3
             WHERE id = $1",
        )
        .bind(mapping.trade_id)
        .bind(settled_price)
        .bind(gamma_match)
        .execute(pool)
        .await?;

        verified += 1;
        if gamma_match {
            matched += 1;
        } else {
            mismatched += 1;
        }
    }

    let unverified = mappings.len() - verified;
    println!(
        "  Gamma verification: {} verified ({} matched, {} mismatched), {} unverified\n",
        verified, matched, mismatched, unverified
    );

    Ok(())
}

fn fmt_ts(ts: Option<DateTime<Utc>>) -> String {
    ts.map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".to_string())
}

async fn table_exists(pool: &sqlx::PgPool, table: &str) -> Result<bool> {
    let reg: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(format!("public.{table}"))
        .fetch_one(pool)
        .await?;
    Ok(reg.is_some())
}

async fn print_event_window_audit(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<()> {
    if !table_exists(pool, "pm_market_metadata").await? {
        println!("\n[event_window_audit] skipped: pm_market_metadata missing");
        return Ok(());
    }

    let rows = load_event_window_audit_rows(pool, symbols, from, to).await?;
    if rows.is_empty() {
        println!("\n[event_window_audit] no overlapping 5m/15m windows found");
        return Ok(());
    }

    let strict = rows
        .iter()
        .filter(|row| row.quality == EventWindowQuality::KeepStrict)
        .count();
    let research = rows
        .iter()
        .filter(|row| row.quality == EventWindowQuality::KeepResearch)
        .count();
    let dropped = rows
        .iter()
        .filter(|row| row.quality == EventWindowQuality::Drop)
        .count();
    let missing_quote = rows.iter().filter(|row| row.quote.rows == 0).count();
    let missing_lob = rows.iter().filter(|row| row.lob.rows == 0).count();
    let missing_l2 = rows.iter().filter(|row| row.l2.rows == 0).count();

    println!("\n[event_window_audit]");
    println!(
        "windows: {}, keep_strict: {}, keep_research: {}, drop: {}",
        rows.len(),
        strict,
        research,
        dropped
    );
    println!(
        "missing sources: pm_quote={}, pm_lob={}, binance_l2={}",
        missing_quote, missing_lob, missing_l2
    );

    let suspicious: Vec<&EventWindowAuditRow> = rows
        .iter()
        .filter(|row| row.quality != EventWindowQuality::KeepStrict)
        .take(EVENT_AUDIT_PREVIEW_ROWS)
        .collect();

    if !suspicious.is_empty() {
        println!("sample suspicious windows (up to {}):", EVENT_AUDIT_PREVIEW_ROWS);
        for row in suspicious {
            println!(
                "- [{}] {} {} {} .. {} | quote={}/{} lob={}/{} spot={} l2={} | issues={}",
                row.quality.as_str(),
                row.symbol,
                row.horizon,
                fmt_ts(row.start_time),
                fmt_ts(row.corrected_end_time.or(row.end_time)),
                row.quote.rows,
                row.quote.distinct_tokens,
                row.lob.rows,
                row.lob.distinct_tokens,
                row.spot.rows,
                row.l2.rows,
                row.issues.join(",")
            );
            println!("  slug: {}", row.market_slug);
        }
    }

    Ok(())
}

pub(super) async fn build_pm_event_replay_selection(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    minimum_quality: PmReplayQuality,
) -> Result<PmEventReplaySelection> {
    let rows = load_event_window_audit_rows(pool, symbols, from, to).await?;
    Ok(select_pm_event_replay_windows(&rows, minimum_quality))
}

async fn load_event_window_audit_rows(
    pool: &sqlx::PgPool,
    symbols: &[String],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Vec<EventWindowAuditRow>> {
    let symbol_list = if symbols.is_empty() {
        None::<Vec<String>>
    } else {
        Some(symbols.to_vec())
    };

    let event_rows = sqlx::query(
        r#"
        SELECT market_slug, symbol, horizon, start_time, end_time, price_to_beat, raw_market
        FROM pm_market_metadata
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND ($2::timestamptz IS NULL OR end_time >= $2 OR end_time IS NULL)
          AND ($3::timestamptz IS NULL OR start_time <= $3 OR start_time IS NULL)
          AND COALESCE(
                horizon,
                CASE
                    WHEN market_slug ILIKE '%-5m-%' THEN '5m'
                    WHEN market_slug ILIKE '%-15m-%' THEN '15m'
                    ELSE NULL
                END
              ) IN ('5m', '15m')
        ORDER BY start_time NULLS LAST, market_slug
        "#,
    )
    .bind(symbol_list.clone())
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    if event_rows.is_empty() {
        return Ok(Vec::new());
    }

    let market_slugs: Vec<String> = event_rows
        .iter()
        .filter_map(|row| row.try_get::<String, _>("market_slug").ok())
        .collect();
    let settlement_rows: Vec<(String, String)> = if market_slugs.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            r#"
            SELECT market_slug, token_id
            FROM pm_token_settlements
            WHERE market_slug = ANY($1)
            GROUP BY market_slug, token_id
            "#,
        )
        .bind(&market_slugs)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    };

    let mut fallback_tokens: HashMap<String, Vec<String>> = HashMap::new();
    for (market_slug, token_id) in settlement_rows {
        fallback_tokens
            .entry(market_slug)
            .or_default()
            .push(token_id);
    }
    for token_ids in fallback_tokens.values_mut() {
        token_ids.sort();
        token_ids.dedup();
    }

    let quote_exists = table_exists(pool, "clob_quote_ticks").await?;
    let lob_exists = table_exists(pool, "clob_orderbook_snapshots").await?;
    let spot_exists = table_exists(pool, "binance_price_ticks").await?;
    let l2_exists = table_exists(pool, "binance_lob_ticks").await?;

    let mut audits = Vec::with_capacity(event_rows.len());
    for row in event_rows {
        let market_slug: String = row.get("market_slug");
        let symbol = row
            .try_get::<Option<String>, _>("symbol")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty())
            .or_else(|| infer_symbol_from_slug(&market_slug))
            .unwrap_or_default();
        let start_time = row.try_get::<Option<DateTime<Utc>>, _>("start_time").ok().flatten();
        let end_time = row.try_get::<Option<DateTime<Utc>>, _>("end_time").ok().flatten();
        let corrected_end_time = start_time.zip(end_time).map(|(start_time, end_time)| {
            corrected_window_end(&market_slug, start_time, end_time)
        });
        let horizon = row
            .try_get::<Option<String>, _>("horizon")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty())
            .or_else(|| infer_horizon_from_slug(&market_slug).map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        let price_to_beat = row
            .try_get::<Option<Decimal>, _>("price_to_beat")
            .ok()
            .flatten()
            .filter(|value| *value > Decimal::ZERO);
        let raw_market = row.try_get::<Option<Value>, _>("raw_market").ok().flatten();

        let mut token_ids = extract_clob_token_ids(raw_market.as_ref());
        if token_ids.is_empty() {
            token_ids = fallback_tokens
                .get(&market_slug)
                .cloned()
                .unwrap_or_default();
        }
        token_ids.sort();
        token_ids.dedup();

        let quote = if quote_exists {
            fetch_token_coverage(
                pool,
                "clob_quote_ticks",
                "received_at",
                &token_ids,
                start_time,
                corrected_end_time.or(end_time),
            )
            .await?
        } else {
            SourceCoverage::default()
        };
        let lob = if lob_exists {
            fetch_token_coverage(
                pool,
                "clob_orderbook_snapshots",
                "received_at",
                &token_ids,
                start_time,
                corrected_end_time.or(end_time),
            )
            .await?
        } else {
            SourceCoverage::default()
        };
        let spot = if spot_exists && !symbol.is_empty() {
            fetch_symbol_coverage(
                pool,
                "binance_price_ticks",
                "trade_time",
                &symbol,
                start_time,
                corrected_end_time.or(end_time),
            )
            .await?
        } else {
            SourceCoverage::default()
        };
        let l2 = if l2_exists && !symbol.is_empty() {
            fetch_symbol_coverage(
                pool,
                "binance_lob_ticks",
                "event_time",
                &symbol,
                start_time,
                corrected_end_time.or(end_time),
            )
            .await?
        } else {
            SourceCoverage::default()
        };

        let mut audit = EventWindowAuditRow {
            market_slug,
            symbol,
            horizon,
            start_time,
            end_time,
            corrected_end_time,
            price_to_beat,
            expected_token_count: token_ids.len(),
            quote,
            lob,
            spot,
            l2,
            quality: EventWindowQuality::Drop,
            issues: Vec::new(),
        };
        let (quality, issues) = classify_event_window_audit(&audit);
        audit.quality = quality;
        audit.issues = issues;
        audits.push(audit);
    }

    Ok(audits)
}

fn select_pm_event_replay_windows(
    rows: &[EventWindowAuditRow],
    minimum_quality: PmReplayQuality,
) -> PmEventReplaySelection {
    let filtered_rows: Vec<&EventWindowAuditRow> =
        rows.iter().filter(|row| row.horizon == "5m").collect();

    let mut windows = Vec::new();
    let mut kept_strict_windows = 0usize;
    let mut kept_research_windows = 0usize;

    for row in &filtered_rows {
        if !row.quality.meets_minimum(minimum_quality) {
            continue;
        }
        let Some(start_time) = row.start_time else {
            continue;
        };
        let Some(end_time) = row.corrected_end_time.or(row.end_time) else {
            continue;
        };
        if row.symbol.is_empty() {
            continue;
        }

        match row.quality {
            EventWindowQuality::KeepStrict => {
                kept_strict_windows = kept_strict_windows.saturating_add(1)
            }
            EventWindowQuality::KeepResearch => {
                kept_research_windows = kept_research_windows.saturating_add(1)
            }
            EventWindowQuality::Drop => {}
        }

        windows.push(ReplayEventWindow {
            market_slug: row.market_slug.clone(),
            symbol: row.symbol.clone(),
            start_time,
            end_time,
        });
    }

    let effective_from = windows.iter().map(|window| window.start_time).min();
    let effective_to = windows.iter().map(|window| window.end_time).max();
    let kept_windows = windows.len();
    let total_windows = filtered_rows.len();

    PmEventReplaySelection {
        minimum_quality,
        total_windows,
        kept_windows,
        kept_strict_windows,
        kept_research_windows,
        dropped_windows: total_windows.saturating_sub(kept_windows),
        effective_from,
        effective_to,
        windows,
    }
}

async fn fetch_symbol_coverage(
    pool: &sqlx::PgPool,
    table: &str,
    ts_column: &str,
    symbol: &str,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
) -> Result<SourceCoverage> {
    let sql = format!(
        "SELECT COUNT(*)::bigint, MIN({ts_column}), MAX({ts_column}) \
         FROM {table} \
         WHERE symbol = $1 \
           AND ($2::timestamptz IS NULL OR {ts_column} >= $2) \
           AND ($3::timestamptz IS NULL OR {ts_column} <= $3)"
    );
    let (rows, first_ts, last_ts) =
        sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(sql.as_str())
            .bind(symbol)
            .bind(start_time)
            .bind(end_time)
            .fetch_one(pool)
            .await?;
    Ok(SourceCoverage {
        rows,
        distinct_tokens: 0,
        first_ts,
        last_ts,
    })
}

async fn fetch_token_coverage(
    pool: &sqlx::PgPool,
    table: &str,
    ts_column: &str,
    token_ids: &[String],
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
) -> Result<SourceCoverage> {
    if token_ids.is_empty() {
        return Ok(SourceCoverage::default());
    }

    let sql = format!(
        "SELECT COUNT(*)::bigint, COUNT(DISTINCT token_id)::bigint, MIN({ts_column}), MAX({ts_column}) \
         FROM {table} \
         WHERE token_id = ANY($1) \
           AND ($2::timestamptz IS NULL OR {ts_column} >= $2) \
           AND ($3::timestamptz IS NULL OR {ts_column} <= $3)"
    );
    let (rows, distinct_tokens, first_ts, last_ts) = sqlx::query_as::<
        _,
        (i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>),
    >(sql.as_str())
    .bind(token_ids)
    .bind(start_time)
    .bind(end_time)
    .fetch_one(pool)
    .await?;
    Ok(SourceCoverage {
        rows,
        distinct_tokens,
        first_ts,
        last_ts,
    })
}

fn classify_event_window_audit(row: &EventWindowAuditRow) -> (EventWindowQuality, Vec<&'static str>) {
    let mut issues = Vec::new();

    if row.start_time.is_none() || row.corrected_end_time.or(row.end_time).is_none() {
        issues.push("missing_window_bounds");
    }
    if row.price_to_beat.is_none() {
        issues.push("missing_price_to_beat");
    }
    if row.expected_token_count == 0 {
        issues.push("missing_token_mapping");
    }
    if row.quote.rows == 0 {
        issues.push("missing_pm_quote");
    }
    if row.lob.rows == 0 {
        issues.push("missing_pm_lob");
    }
    if row.spot.rows == 0 {
        issues.push("missing_spot");
    }
    if row.l2.rows == 0 {
        issues.push("missing_binance_l2");
    }
    if row.expected_token_count >= 2 && row.quote.distinct_tokens < 2 {
        issues.push("pm_quote_missing_side");
    }
    if row.expected_token_count >= 2 && row.lob.distinct_tokens < 2 {
        issues.push("pm_lob_missing_side");
    }
    if row.quote.tail_gap_secs(row.corrected_end_time.or(row.end_time)) > Some(STRICT_PM_QUOTE_TAIL_SECS) {
        issues.push("stale_pm_quote_tail");
    }
    if row.lob.tail_gap_secs(row.corrected_end_time.or(row.end_time)) > Some(STRICT_PM_LOB_TAIL_SECS) {
        issues.push("stale_pm_lob_tail");
    }
    if row.spot.tail_gap_secs(row.corrected_end_time.or(row.end_time)) > Some(STRICT_SPOT_TAIL_SECS) {
        issues.push("stale_spot_tail");
    }
    if row.l2.tail_gap_secs(row.corrected_end_time.or(row.end_time)) > Some(STRICT_L2_TAIL_SECS) {
        issues.push("stale_binance_l2_tail");
    }

    let research_ready = row.start_time.is_some()
        && row.corrected_end_time.or(row.end_time).is_some()
        && row.expected_token_count > 0
        && row.quote.rows > 0
        && row.spot.rows > 0;
    let strict_ready = research_ready
        && row.price_to_beat.is_some()
        && row.lob.rows > 0
        && row.l2.rows > 0
        && row.expected_token_count >= 2
        && row.quote.distinct_tokens >= 2
        && row.lob.distinct_tokens >= 2
        && row.quote.tail_gap_secs(row.corrected_end_time.or(row.end_time))
            <= Some(STRICT_PM_QUOTE_TAIL_SECS)
        && row.lob.tail_gap_secs(row.corrected_end_time.or(row.end_time))
            <= Some(STRICT_PM_LOB_TAIL_SECS)
        && row.spot.tail_gap_secs(row.corrected_end_time.or(row.end_time))
            <= Some(STRICT_SPOT_TAIL_SECS)
        && row.l2.tail_gap_secs(row.corrected_end_time.or(row.end_time))
            <= Some(STRICT_L2_TAIL_SECS);

    let quality = if strict_ready {
        EventWindowQuality::KeepStrict
    } else if research_ready {
        EventWindowQuality::KeepResearch
    } else {
        EventWindowQuality::Drop
    };
    (quality, issues)
}

fn extract_clob_token_ids(raw_market: Option<&Value>) -> Vec<String> {
    let Some(raw_market) = raw_market else {
        return Vec::new();
    };
    let Some(raw_ids) = raw_market.get("clobTokenIds") else {
        return Vec::new();
    };

    let mut token_ids = Vec::new();
    match raw_ids {
        Value::Array(items) => {
            for item in items {
                if let Some(token_id) = item.as_str().and_then(normalize_clob_token_id) {
                    token_ids.push(token_id);
                }
            }
        }
        Value::String(encoded) => {
            if let Ok(items) = serde_json::from_str::<Vec<String>>(encoded) {
                for item in items {
                    if let Some(token_id) = normalize_clob_token_id(&item) {
                        token_ids.push(token_id);
                    }
                }
            } else if let Some(token_id) = normalize_clob_token_id(encoded) {
                token_ids.push(token_id);
            }
        }
        _ => {}
    }

    token_ids.sort();
    token_ids.dedup();
    token_ids
}

fn normalize_clob_token_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return U256::from_str_radix(hex, 16).ok().map(|value| value.to_string());
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }
    U256::from_str_radix(trimmed, 16)
        .ok()
        .map(|value| value.to_string())
}

fn infer_symbol_from_slug(slug: &str) -> Option<String> {
    let normalized = slug.to_ascii_lowercase();
    if normalized.starts_with("btc-") || normalized.starts_with("bitcoin-") {
        return Some("BTCUSDT".to_string());
    }
    if normalized.starts_with("eth-") || normalized.starts_with("ethereum-") {
        return Some("ETHUSDT".to_string());
    }
    if normalized.starts_with("sol-") || normalized.starts_with("solana-") {
        return Some("SOLUSDT".to_string());
    }
    if normalized.starts_with("xrp-") {
        return Some("XRPUSDT".to_string());
    }
    None
}

fn infer_horizon_from_slug(slug: &str) -> Option<&'static str> {
    let normalized = slug.to_ascii_lowercase();
    if normalized.contains("-15m-") {
        return Some("15m");
    }
    if normalized.contains("-5m-") {
        return Some("5m");
    }
    None
}

fn corrected_window_end(
    slug: &str,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
) -> DateTime<Utc> {
    let duration = infer_horizon_from_slug(slug).and_then(|horizon| match horizon {
        "5m" => Some(chrono::Duration::seconds(300)),
        "15m" => Some(chrono::Duration::seconds(900)),
        _ => None,
    });

    if let Some(duration) = duration {
        let expected_end = start_time + duration;
        if (end_time - start_time) > duration * 2 {
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
    use serde_json::json;

    fn strict_row() -> EventWindowAuditRow {
        let start_time = DateTime::parse_from_rfc3339("2026-03-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end_time = DateTime::parse_from_rfc3339("2026-03-12T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        EventWindowAuditRow {
            market_slug: "btc-updown-5m-test".to_string(),
            symbol: "BTCUSDT".to_string(),
            horizon: "5m".to_string(),
            start_time: Some(start_time),
            end_time: Some(end_time),
            corrected_end_time: Some(end_time),
            price_to_beat: Some(dec!(100000)),
            expected_token_count: 2,
            quote: SourceCoverage {
                rows: 40,
                distinct_tokens: 2,
                first_ts: Some(start_time),
                last_ts: Some(end_time - chrono::Duration::seconds(5)),
            },
            lob: SourceCoverage {
                rows: 20,
                distinct_tokens: 2,
                first_ts: Some(start_time),
                last_ts: Some(end_time - chrono::Duration::seconds(10)),
            },
            spot: SourceCoverage {
                rows: 60,
                distinct_tokens: 0,
                first_ts: Some(start_time),
                last_ts: Some(end_time - chrono::Duration::seconds(1)),
            },
            l2: SourceCoverage {
                rows: 30,
                distinct_tokens: 0,
                first_ts: Some(start_time),
                last_ts: Some(end_time - chrono::Duration::seconds(2)),
            },
            quality: EventWindowQuality::Drop,
            issues: Vec::new(),
        }
    }

    #[test]
    fn classify_event_window_audit_marks_strict_window() {
        let row = strict_row();
        let (quality, issues) = classify_event_window_audit(&row);
        assert_eq!(quality, EventWindowQuality::KeepStrict);
        assert!(issues.is_empty());
    }

    #[test]
    fn classify_event_window_audit_drops_missing_pm_quotes() {
        let mut row = strict_row();
        row.quote = SourceCoverage::default();
        let (quality, issues) = classify_event_window_audit(&row);
        assert_eq!(quality, EventWindowQuality::Drop);
        assert!(issues.contains(&"missing_pm_quote"));
    }

    #[test]
    fn classify_event_window_audit_keeps_research_without_l2_or_lob() {
        let mut row = strict_row();
        row.lob = SourceCoverage::default();
        row.l2 = SourceCoverage::default();
        let (quality, issues) = classify_event_window_audit(&row);
        assert_eq!(quality, EventWindowQuality::KeepResearch);
        assert!(issues.contains(&"missing_pm_lob"));
        assert!(issues.contains(&"missing_binance_l2"));
    }

    #[test]
    fn select_pm_event_replay_windows_respects_minimum_quality() {
        let mut strict = strict_row();
        let (quality, issues) = classify_event_window_audit(&strict);
        strict.quality = quality;
        strict.issues = issues;
        let mut research = strict_row();
        research.market_slug = "btc-updown-5m-research".to_string();
        research.lob = SourceCoverage::default();
        research.l2 = SourceCoverage::default();
        let (quality, issues) = classify_event_window_audit(&research);
        research.quality = quality;
        research.issues = issues;

        let strict_selection = select_pm_event_replay_windows(
            &[strict.clone(), research.clone()],
            PmReplayQuality::Strict,
        );
        assert_eq!(strict_selection.total_windows, 2);
        assert_eq!(strict_selection.kept_windows, 1);
        assert_eq!(strict_selection.kept_strict_windows, 1);
        assert_eq!(strict_selection.kept_research_windows, 0);
        assert_eq!(strict_selection.dropped_windows, 1);
        assert_eq!(strict_selection.windows[0].market_slug, strict.market_slug);

        let research_selection = select_pm_event_replay_windows(
            &[strict.clone(), research.clone()],
            PmReplayQuality::Research,
        );
        assert_eq!(research_selection.kept_windows, 2);
        assert_eq!(research_selection.kept_strict_windows, 1);
        assert_eq!(research_selection.kept_research_windows, 1);
        assert_eq!(research_selection.dropped_windows, 0);
    }

    #[test]
    fn extract_clob_token_ids_accepts_json_array_string() {
        let raw_market = json!({
            "clobTokenIds": "[\"0f\", \"16\"]"
        });
        assert_eq!(
            extract_clob_token_ids(Some(&raw_market)),
            vec![U256::from(15u8).to_string(), "16".to_string()]
        );
    }

    #[test]
    fn extract_clob_token_ids_accepts_json_array() {
        let raw_market = json!({
            "clobTokenIds": ["0f", "16"]
        });
        assert_eq!(
            extract_clob_token_ids(Some(&raw_market)),
            vec![U256::from(15u8).to_string(), "16".to_string()]
        );
    }

    #[test]
    fn corrected_window_end_caps_bad_metadata_for_short_windows() {
        let start_time = DateTime::parse_from_rfc3339("2026-03-12T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let bad_end = DateTime::parse_from_rfc3339("2026-03-12T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            corrected_window_end("btc-updown-5m-test", start_time, bad_end),
            DateTime::parse_from_rfc3339("2026-03-12T00:05:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }
}
