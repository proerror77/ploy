use super::{env_i64, env_u64, env_usize};
use crate::adapters::PolymarketClient;
use crate::error::Result;
use chrono::Utc;
use futures_util::StreamExt;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

async fn ensure_pm_token_settlements_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_token_settlements (
            token_id TEXT PRIMARY KEY,
            condition_id TEXT,
            market_id TEXT,
            market_slug TEXT,
            outcome TEXT,
            settled_price NUMERIC(10,6),
            resolved BOOLEAN NOT NULL DEFAULT FALSE,
            resolved_at TIMESTAMPTZ,
            fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            raw_market JSONB
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_condition ON pm_token_settlements(condition_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_market_slug ON pm_token_settlements(market_slug)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_resolved_at ON pm_token_settlements(resolved_at DESC) WHERE resolved_at IS NOT NULL",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_token_settlements_fetched_at ON pm_token_settlements(fetched_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

#[derive(Debug, Default, Clone, Copy)]
struct SettlementRefreshStats {
    targeted_tokens: usize,
    refreshed_markets: usize,
    upserted_rows: usize,
    resolved_markets: usize,
}

pub(crate) fn spawn_pm_token_settlement_persistence(
    pm_client: PolymarketClient,
    pool: PgPool,
    agent_id: String,
    collector_domains: Vec<&'static str>,
) {
    tokio::spawn(async move {
        if let Err(e) = ensure_pm_token_settlements_table(&pool).await {
            warn!(
                agent = %agent_id,
                error = %e,
                "failed to ensure pm_token_settlements table; settlement persistence disabled"
            );
            return;
        }

        let poll_secs = env_u64("PM_SETTLEMENT_POLL_SECS", 120).max(10);
        let targets_limit = env_usize("PM_SETTLEMENT_TARGETS_LIMIT", 1000).clamp(1, 10000);
        let unresolved_limit = env_usize("PM_SETTLEMENT_UNRESOLVED_LIMIT", 1000).clamp(1, 10000);
        let lookback_secs = env_i64("PM_SETTLEMENT_TARGET_LOOKBACK_SECS", 86400).max(0);
        let max_tokens_per_cycle =
            env_usize("PM_SETTLEMENT_MAX_TOKENS_PER_CYCLE", 200).clamp(1, 5000);
        let max_concurrency = env_usize("PM_SETTLEMENT_CONCURRENCY", 2).clamp(1, 32);

        let collector_domains_label = collector_domains.join(",");
        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            match refresh_pm_token_settlements_for_domains(
                &pm_client,
                &pool,
                &collector_domains,
                targets_limit,
                unresolved_limit,
                lookback_secs,
                max_tokens_per_cycle,
                max_concurrency,
            )
            .await
            {
                Ok(stats) => {
                    if stats.targeted_tokens > 0
                        && (stats.resolved_markets > 0 || stats.upserted_rows > 0)
                    {
                        info!(
                            agent = %agent_id,
                            collector_domains = %collector_domains_label,
                            targeted_tokens = stats.targeted_tokens,
                            refreshed_markets = stats.refreshed_markets,
                            upserted_rows = stats.upserted_rows,
                            resolved_markets = stats.resolved_markets,
                            "pm settlement persistence cycle complete"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent = %agent_id,
                        collector_domains = %collector_domains_label,
                        error = %e,
                        "pm settlement persistence cycle failed"
                    );
                }
            }
        }
    });
}

async fn refresh_pm_token_settlements_for_domains(
    pm_client: &PolymarketClient,
    pool: &PgPool,
    collector_domains: &[&str],
    targets_limit: usize,
    unresolved_limit: usize,
    lookback_secs: i64,
    max_tokens_per_cycle: usize,
    max_concurrency: usize,
) -> Result<SettlementRefreshStats> {
    use std::collections::BTreeSet;

    let mut token_ids: BTreeSet<String> = BTreeSet::new();

    for domain in collector_domains {
        let scoped_targets = sqlx::query_scalar::<_, String>(
            r#"
            SELECT token_id
            FROM collector_token_targets
            WHERE domain = $1
              AND (
                    expires_at IS NULL
                 OR expires_at > NOW() - ($2::bigint * INTERVAL '1 second')
              )
            ORDER BY updated_at DESC
            LIMIT $3
            "#,
        )
        .bind(*domain)
        .bind(lookback_secs)
        .bind(targets_limit as i64)
        .fetch_all(pool)
        .await?;
        for token_id in scoped_targets {
            if !token_id.trim().is_empty() {
                token_ids.insert(token_id);
            }
        }
    }

    let unresolved_targets = sqlx::query_scalar::<_, String>(
        r#"
        SELECT token_id
        FROM pm_token_settlements
        WHERE resolved = FALSE
        ORDER BY fetched_at DESC
        LIMIT $1
        "#,
    )
    .bind(unresolved_limit as i64)
    .fetch_all(pool)
    .await?;
    for token_id in unresolved_targets {
        if !token_id.trim().is_empty() {
            token_ids.insert(token_id);
        }
    }

    let mut token_ids: Vec<String> = token_ids.into_iter().collect();
    if token_ids.is_empty() {
        return Ok(SettlementRefreshStats::default());
    }
    if token_ids.len() > max_tokens_per_cycle {
        token_ids.truncate(max_tokens_per_cycle);
    }

    let seen_conditions: Arc<tokio::sync::Mutex<HashSet<String>>> =
        Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let stats: Arc<tokio::sync::Mutex<SettlementRefreshStats>> =
        Arc::new(tokio::sync::Mutex::new(SettlementRefreshStats {
            targeted_tokens: token_ids.len(),
            ..SettlementRefreshStats::default()
        }));

    futures_util::stream::iter(token_ids)
        .for_each_concurrent(max_concurrency, |token_id| {
            let seen_conditions = seen_conditions.clone();
            let stats = stats.clone();
            async move {
                let market = match pm_client.get_gamma_market_by_token_id(&token_id).await {
                    Ok(market) => market,
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            error = %e,
                            "failed to fetch gamma market for settlement refresh"
                        );
                        return;
                    }
                };

                let condition_key = market
                    .condition_id
                    .map(|b| b.to_string())
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| format!("market:{}", market.id));

                {
                    let mut seen = seen_conditions.lock().await;
                    if !seen.insert(condition_key) {
                        return;
                    }
                }

                match upsert_pm_token_settlement_rows(pool, &market).await {
                    Ok((upserted_rows, resolved_market)) => {
                        let mut guard = stats.lock().await;
                        guard.refreshed_markets += 1;
                        guard.upserted_rows += upserted_rows;
                        if resolved_market {
                            guard.resolved_markets += 1;
                        }
                        drop(guard);

                        if let Err(e) =
                            backfill_pm_market_metadata_from_settlement(pool, &market).await
                        {
                            debug!(
                                market_id = %market.id,
                                error = %e,
                                "pm_market_metadata backfill skipped"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            market_id = %market.id,
                            error = %e,
                            "failed to upsert pm settlement rows"
                        );
                    }
                }
            }
        })
        .await;

    let snapshot = { *stats.lock().await };
    Ok(snapshot)
}

async fn upsert_pm_token_settlement_rows(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<(usize, bool)> {
    let clob_token_ids: Vec<String> = market
        .clob_token_ids
        .as_ref()
        .map(|ids| ids.iter().map(|id| id.to_string()).collect())
        .unwrap_or_default();
    let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
    let outcome_prices: Vec<String> = market
        .outcome_prices
        .as_ref()
        .map(|ps| ps.iter().map(|d| d.to_string()).collect())
        .unwrap_or_default();

    if clob_token_ids.is_empty() || outcome_prices.is_empty() {
        return Ok((0, false));
    }

    let parsed_prices: Vec<rust_decimal::Decimal> = outcome_prices
        .iter()
        .filter_map(|v| v.parse::<rust_decimal::Decimal>().ok())
        .collect();
    let resolved_market = market.closed.unwrap_or(false) && is_market_resolved(&parsed_prices);
    let resolved_at: Option<chrono::DateTime<Utc>> = resolved_market.then(Utc::now);
    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    let mut upserted_rows = 0usize;
    for (idx, token_id) in clob_token_ids.iter().enumerate() {
        let outcome = outcomes.get(idx).cloned();
        let settled_price = outcome_prices
            .get(idx)
            .and_then(|v| v.parse::<rust_decimal::Decimal>().ok());

        sqlx::query(
            r#"
            INSERT INTO pm_token_settlements (
                token_id,
                condition_id,
                market_id,
                market_slug,
                outcome,
                settled_price,
                resolved,
                resolved_at,
                fetched_at,
                raw_market
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
            ON CONFLICT (token_id) DO UPDATE SET
                condition_id = EXCLUDED.condition_id,
                market_id = EXCLUDED.market_id,
                market_slug = EXCLUDED.market_slug,
                outcome = EXCLUDED.outcome,
                settled_price = EXCLUDED.settled_price,
                resolved = EXCLUDED.resolved,
                resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                fetched_at = NOW(),
                raw_market = EXCLUDED.raw_market
            "#,
        )
        .bind(token_id)
        .bind(market.condition_id.map(|b| b.to_string()))
        .bind(&market.id)
        .bind(market.slug.as_deref())
        .bind(outcome.as_deref())
        .bind(settled_price)
        .bind(resolved_market)
        .bind(resolved_at)
        .bind(sqlx::types::Json(raw_market.clone()))
        .execute(pool)
        .await?;

        upserted_rows += 1;
    }

    Ok((upserted_rows, resolved_market))
}

async fn backfill_pm_market_metadata_from_settlement(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<()> {
    let slug = match market.slug.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    let end_time: Option<chrono::DateTime<Utc>> = market
        .end_date_iso
        .map(|d| d.and_hms_opt(23, 59, 59).unwrap_or_default())
        .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    let start_time: Option<chrono::DateTime<Utc>> = raw_market
        .get("eventStartTime")
        .or_else(|| raw_market.get("startDate"))
        .or_else(|| raw_market.get("start_date"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let symbol = if slug.starts_with("btc-") {
        Some("BTCUSDT")
    } else if slug.starts_with("eth-") {
        Some("ETHUSDT")
    } else if slug.starts_with("sol-") {
        Some("SOLUSDT")
    } else {
        None
    };

    let horizon = if slug.contains("-5m-") {
        Some("5m")
    } else if slug.contains("-15m-") {
        Some("15m")
    } else if slug.contains("-60m-") {
        Some("60m")
    } else {
        match (start_time, end_time) {
            (Some(s), Some(e)) => {
                let secs = (e - s).num_seconds();
                if secs <= 360 {
                    Some("5m")
                } else if secs <= 1080 {
                    Some("15m")
                } else {
                    Some("60m")
                }
            }
            _ => None,
        }
    };

    let threshold: Option<rust_decimal::Decimal> = market
        .group_item_threshold
        .as_deref()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            let upper: Option<f64> = raw_market
                .get("upperBound")
                .or_else(|| raw_market.get("upper_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            let lower: Option<f64> = raw_market
                .get("lowerBound")
                .or_else(|| raw_market.get("lower_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            match (upper, lower) {
                (Some(u), Some(l)) => rust_decimal::Decimal::try_from((u + l) / 2.0).ok(),
                _ => None,
            }
        });

    let price_to_beat = match threshold {
        Some(p) if !p.is_zero() => p,
        _ => match (symbol, start_time) {
            (Some(sym), Some(st)) => {
                let row = sqlx::query_scalar::<_, rust_decimal::Decimal>(
                    "SELECT price FROM binance_price_ticks WHERE symbol = $1 AND trade_time <= $2 ORDER BY trade_time DESC LIMIT 1"
                )
                .bind(sym)
                .bind(st)
                .fetch_optional(pool)
                .await
                .unwrap_or(None);
                row.unwrap_or(rust_decimal::Decimal::ZERO)
            }
            _ => rust_decimal::Decimal::ZERO,
        },
    };

    sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = EXCLUDED.price_to_beat,
            start_time    = COALESCE(EXCLUDED.start_time, pm_market_metadata.start_time),
            end_time      = COALESCE(EXCLUDED.end_time, pm_market_metadata.end_time),
            horizon       = COALESCE(EXCLUDED.horizon, pm_market_metadata.horizon),
            symbol        = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
            raw_market    = EXCLUDED.raw_market,
            updated_at    = NOW()
        "#,
    )
    .bind(slug)
    .bind(price_to_beat)
    .bind(start_time)
    .bind(end_time)
    .bind(horizon)
    .bind(symbol)
    .bind(sqlx::types::Json(raw_market))
    .execute(pool)
    .await?;

    Ok(())
}

#[allow(dead_code)]
fn parse_json_array_strings_relaxed(
    input: &str,
) -> std::result::Result<Vec<String>, serde_json::Error> {
    let s = input.trim();
    if s.is_empty() || s == "null" {
        return Ok(Vec::new());
    }

    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Ok(v);
    }

    let vals = serde_json::from_str::<Vec<serde_json::Value>>(s)?;
    Ok(vals
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect())
}

fn is_market_resolved(prices: &[rust_decimal::Decimal]) -> bool {
    if prices.is_empty() {
        return false;
    }

    let winners = prices
        .iter()
        .filter(|p| **p >= rust_decimal_macros::dec!(0.99))
        .count();
    let losers = prices
        .iter()
        .filter(|p| **p <= rust_decimal_macros::dec!(0.01))
        .count();

    winners == 1 && losers == prices.len().saturating_sub(1)
}
