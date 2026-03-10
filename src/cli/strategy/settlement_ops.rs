use super::*;

#[derive(Debug, Clone)]
struct CryptoLobDatasetRow {
    executed_at: chrono::DateTime<chrono::Utc>,
    intent_id: uuid::Uuid,
    account_id: String,
    agent_id: String,
    market_slug: String,
    token_id: String,
    market_side: String,
    is_buy: bool,
    limit_price: rust_decimal::Decimal,
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
    settled_price: rust_decimal::Decimal,
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
    output: &std::path::Path,
    rows: &[CryptoLobDatasetRow],
) -> Result<()> {
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
fn sanitize_duckdb_copy_path(path: &std::path::Path) -> std::result::Result<String, duckdb::Error> {
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
    output: &std::path::Path,
    rows: &[CryptoLobDatasetRow],
) -> Result<()> {
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

/// Strategy-related commands

pub(super) async fn report_accuracy_pm_settlement(
    lookback_hours: u64,
    domain: Option<String>,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use anyhow::bail;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let account_id = account_id.or_else(|| std::env::var("PLOY_ACCOUNT__ID").ok());

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let domain_norm = domain
        .as_deref()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty());
    if let Some(ref d) = domain_norm {
        if !matches!(d.as_str(), "crypto" | "sports" | "politics") {
            bail!("invalid --domain: {d} (expected crypto|sports|politics)");
        }
    }

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Accuracy Report (Polymarket Settlement)                      ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} domain={} account_id={} agent_id={} live_only={} limit={} refresh={}",
        lookback_hours,
        domain_norm.as_deref().unwrap_or("all"),
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh
    );

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::persistence::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    // Pull latest entry intents within lookback window.
    //
    // NOTE:
    // - Some strategies express "DOWN" exposure via sells (short) rather than buys,
    //   so we can't filter to `is_buy = TRUE`.
    // - Prefer the explicit signal_type suffix when present.
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
          AND ($2::text IS NULL OR LOWER(domain) = $2)
          AND ($3::text IS NULL OR account_id = $3)
          AND ($4::text IS NULL OR agent_id = $4)
          AND ($5::bool = FALSE OR dry_run = FALSE)
        ORDER BY executed_at DESC
        LIMIT $6
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(domain_norm.as_deref())
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
        let existing = sqlx::query(
            r#"
            SELECT token_id, resolved
            FROM pm_token_settlements
            WHERE token_id = ANY($1)
            "#,
        )
        .bind(&token_ids)
        .fetch_all(store.pool())
        .await
        .context("Failed to query pm_token_settlements")?;

        let mut resolved_map: HashMap<String, bool> = HashMap::new();
        for row in existing {
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            resolved_map.insert(token_id, resolved);
        }

        let mut to_refresh: Vec<String> = token_ids
            .iter()
            .filter(|t| !resolved_map.get(*t).copied().unwrap_or(false))
            .cloned()
            .collect();

        // Avoid hammering Gamma on large windows; refresh a bounded set.
        const MAX_REFRESH: usize = 500;
        if to_refresh.len() > MAX_REFRESH {
            to_refresh.truncate(MAX_REFRESH);
        }

        if !to_refresh.is_empty() {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                to_refresh.len()
            );
        }

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed_markets = 0usize;
        let mut refreshed_tokens = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(&token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "failed to fetch gamma market for token");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
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
                tracing::debug!(
                    token_id = %token_id,
                    market_id = %market.id,
                    "gamma market missing clob_token_ids or outcome_prices; skipping"
                );
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            // Treat as "officially settled" only once the market is closed and prices are 1/0.
            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

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
                .bind(tid)
                .bind(condition_id.as_deref())
                .bind(&market.id)
                .bind(market_slug.as_deref())
                .bind(outcome.as_deref())
                .bind(settled_price)
                .bind(resolved)
                .bind(resolved_at)
                .bind(sqlx::types::Json(raw_market.clone()))
                .execute(store.pool())
                .await
                .context("Failed to upsert pm_token_settlements row")?;

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows",
                refreshed_markets, refreshed_tokens
            );
        }
    }

    // Final join for scoring.
    let scored_rows = sqlx::query(
        r#"
        SELECT
            e.executed_at,
            e.intent_id,
            e.agent_id,
            e.domain,
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
          AND ($2::text IS NULL OR LOWER(e.domain) = $2)
          AND ($3::text IS NULL OR e.account_id = $3)
          AND ($4::text IS NULL OR e.agent_id = $4)
          AND ($5::bool = FALSE OR e.dry_run = FALSE)
        ORDER BY e.executed_at DESC
        LIMIT $6
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(domain_norm.as_deref())
    .bind(account_id.as_deref())
    .bind(agent_id.as_deref())
    .bind(live_only)
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await
    .context("Failed to query joined accuracy rows")?;

    let mut total = 0usize;
    let mut scored = 0usize;
    let mut wins = 0usize;
    let mut pending = 0usize;
    let mut by_agent: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // scored, wins

    #[derive(Debug, Clone, Copy, Default)]
    struct PredAgg {
        n: usize,
        correct: usize,
        brier_sum: f64,
        logloss_sum: f64,
    }

    let mut pred_total = 0usize;
    let mut pred_correct = 0usize;
    let mut pred_brier_sum = 0.0_f64;
    let mut pred_logloss_sum = 0.0_f64;
    let mut pred_by_agent: BTreeMap<String, PredAgg> = BTreeMap::new();

    for row in &scored_rows {
        total += 1;
        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let is_resolved = resolved.unwrap_or(false) && settled_price.is_some();

        if !is_resolved {
            pending += 1;
            continue;
        }

        scored += 1;
        let is_buy: bool = row.get("is_buy");
        let sp = settled_price.unwrap_or(Decimal::ZERO);
        let won = if is_buy {
            sp > dec!(0.5)
        } else {
            sp < dec!(0.5)
        };
        if won {
            wins += 1;
        }

        let agent: String = row.get("agent_id");
        let entry = by_agent.entry(agent).or_insert((0, 0));
        entry.0 += 1;
        if won {
            entry.1 += 1;
        }

        // Optional: prediction scoring for strategies that log p_up.
        // We score p_up against official settlement as y_up in {0,1}.
        let meta: serde_json::Value = row.try_get("metadata").unwrap_or(serde_json::Value::Null);
        let p_up_opt = meta
            .get("p_up")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|p| p.is_finite() && (0.0..=1.0).contains(p));

        if let Some(p_up) = p_up_opt {
            let market_side: String = row.get("market_side");
            let y_up: f64 = match market_side.as_str() {
                "UP" => {
                    if sp > dec!(0.5) {
                        1.0
                    } else {
                        0.0
                    }
                }
                "DOWN" => {
                    if sp > dec!(0.5) {
                        0.0
                    } else {
                        1.0
                    }
                }
                _ => continue,
            };

            let pred_label_up = p_up >= 0.5;
            let y_label_up = y_up >= 0.5;
            let correct = pred_label_up == y_label_up;

            let brier = (p_up - y_up).powi(2);
            let p = p_up.clamp(1e-6, 1.0 - 1e-6);
            let logloss = -(y_up * p.ln() + (1.0 - y_up) * (1.0 - p).ln());

            pred_total += 1;
            if correct {
                pred_correct += 1;
            }
            pred_brier_sum += brier;
            pred_logloss_sum += logloss;

            let agg = pred_by_agent.entry(row.get("agent_id")).or_default();
            agg.n += 1;
            if correct {
                agg.correct += 1;
            }
            agg.brier_sum += brier;
            agg.logloss_sum += logloss;
        }
    }

    let losses = scored.saturating_sub(wins);
    let acc = if scored > 0 {
        100.0 * (wins as f64) / (scored as f64)
    } else {
        0.0
    };

    println!("\n  Summary:");
    println!("  - intents_total:    {}", total);
    println!("  - intents_scored:   {}", scored);
    println!("  - wins:             {}", wins);
    println!("  - losses:           {}", losses);
    println!("  - pending:          {}", pending);
    println!("  - accuracy:         {:.2}%", acc);

    if !by_agent.is_empty() {
        println!("\n  By agent (scored, wins, accuracy):");
        for (agent, (a_scored, a_wins)) in by_agent.iter() {
            let a_acc = if *a_scored > 0 {
                100.0 * (*a_wins as f64) / (*a_scored as f64)
            } else {
                0.0
            };
            println!(
                "  - {:<20} scored={:<5} wins={:<5} acc={:.2}%",
                agent, a_scored, a_wins, a_acc
            );
        }
    }

    if pred_total > 0 {
        let pred_acc = 100.0 * (pred_correct as f64) / (pred_total as f64);
        let brier = pred_brier_sum / (pred_total as f64);
        let logloss = pred_logloss_sum / (pred_total as f64);
        println!("\n  Prediction metrics (p_up vs settlement y_up):");
        println!("  - preds_scored:      {}", pred_total);
        println!("  - preds_acc@0.5:     {:.2}%", pred_acc);
        println!("  - brier_score:       {:.6}", brier);
        println!("  - log_loss:          {:.6}", logloss);

        if !pred_by_agent.is_empty() {
            println!("\n  Prediction by agent (n, acc@0.5, brier, logloss):");
            for (agent, agg) in pred_by_agent.iter() {
                if agg.n == 0 {
                    continue;
                }
                let a_acc = 100.0 * (agg.correct as f64) / (agg.n as f64);
                let a_brier = agg.brier_sum / (agg.n as f64);
                let a_ll = agg.logloss_sum / (agg.n as f64);
                println!(
                    "  - {:<20} n={:<5} acc={:>6.2}% brier={:.6} ll={:.6}",
                    agent, agg.n, a_acc, a_brier, a_ll
                );
            }
        }
    }

    println!("\n  Latest intents:");
    println!("  Time (UTC)          Agent              Side  Dir   Entry  Settled Outcome        Result  Intent");
    println!("  ------------------  ------------------  ----  ----  -----  ------ -------------  ------  ------------------------------------");

    for row in &scored_rows {
        let executed_at: DateTime<Utc> = row.get("executed_at");
        let agent: String = row.get("agent_id");
        let side: String = row.get("market_side");
        let is_buy: bool = row.get("is_buy");
        let entry_price: Decimal = row.get("limit_price");
        let intent_id: uuid::Uuid = row.get("intent_id");

        let resolved: Option<bool> = row.try_get("pm_resolved").ok();
        let settled_price: Option<Decimal> = row.try_get("pm_settled_price").ok();
        let outcome: Option<String> = row.try_get("pm_outcome").ok();

        let (settled_str, outcome_str, result_str) =
            if resolved.unwrap_or(false) && settled_price.is_some() {
                let sp = settled_price.unwrap_or(Decimal::ZERO);
                let won = if is_buy {
                    sp > dec!(0.5)
                } else {
                    sp < dec!(0.5)
                };
                (
                    format!("{:.3}", sp),
                    outcome.unwrap_or_else(|| "-".to_string()),
                    if won { "WIN" } else { "LOSE" }.to_string(),
                )
            } else {
                ("-".to_string(), "-".to_string(), "PENDING".to_string())
            };

        println!(
            "  {}  {:<18}  {:<4}  {:<4}  {:>5.1}¢  {:>6} {:<13}  {:<6}  {}",
            executed_at.format("%Y-%m-%d %H:%M"),
            agent,
            side,
            if is_buy { "BUY" } else { "SELL" },
            entry_price * dec!(100),
            settled_str,
            outcome_str,
            result_str,
            intent_id
        );
    }

    println!();
    Ok(())
}

pub(super) async fn backtest_directional_signals_pm_settlement(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PolymarketClient;
    use crate::adapters::PostgresStore;
    use crate::strategy::fee_model::FeeModel;
    use chrono::{DateTime, Utc};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let db_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE__URL").ok())
        .unwrap_or_else(|| "postgres://localhost/ploy".to_string());

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    crate::persistence::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;
    crate::persistence::ensure_pm_token_settlements_table(store.pool())
        .await
        .context("Failed to ensure pm_token_settlements table")?;

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36m║  Directional Signal Backtest (Settlement)                     ║\x1b[0m");
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");
    println!(
        "  lookback_hours={} account_id={} agent_id={} live_only={} limit={} refresh={}",
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        agent_id.as_deref().unwrap_or("all"),
        live_only,
        limit,
        !no_refresh
    );

    let rows = sqlx::query(
        r#"
        SELECT
            recorded_at,
            account_id,
            agent_id,
            strategy_id,
            token_id,
            symbol,
            side,
            confidence,
            market_price,
            edge,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = 'directional_entry'
          AND ($2::text IS NULL OR account_id = $2)
          AND ($3::text IS NULL OR agent_id = $3)
          AND ($4::bool = FALSE OR COALESCE((context->>'dry_run')::bool, false) = false)
        ORDER BY recorded_at DESC
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
    .context("Failed to query signal_history")?;

    if rows.is_empty() {
        println!("\n  No directional signals found in this window.\n");
        return Ok(());
    }

    #[derive(Debug, Clone)]
    struct SignalRow {
        recorded_at: DateTime<Utc>,
        token_id: String,
        symbol: Option<String>,
        entry_price: Decimal,
    }

    let mut signals: Vec<SignalRow> = Vec::with_capacity(rows.len());
    let mut token_ids: Vec<String> = Vec::with_capacity(rows.len());

    for row in rows {
        let recorded_at: DateTime<Utc> = row.get("recorded_at");
        let token_id: Option<String> = row.get("token_id");
        let Some(token_id) = token_id else { continue };
        let entry_price: Option<Decimal> = row.get("market_price");
        let Some(entry_price) = entry_price else {
            continue;
        };

        let symbol: Option<String> = row.get("symbol");
        token_ids.push(token_id.clone());
        signals.push(SignalRow {
            recorded_at,
            token_id,
            symbol,
            entry_price,
        });
    }

    if signals.is_empty() {
        println!("\n  No usable signals found (missing token_id/market_price).\n");
        return Ok(());
    }

    token_ids.sort();
    token_ids.dedup();

    if !no_refresh {
        let existing = sqlx::query(
            r#"
            SELECT token_id, resolved
            FROM pm_token_settlements
            WHERE token_id = ANY($1)
            "#,
        )
        .bind(&token_ids)
        .fetch_all(store.pool())
        .await
        .context("Failed to query pm_token_settlements")?;

        let mut resolved_map: HashMap<String, bool> = HashMap::new();
        for row in existing {
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            resolved_map.insert(token_id, resolved);
        }

        let mut to_refresh: Vec<String> = token_ids
            .iter()
            .filter(|t| !resolved_map.get(*t).copied().unwrap_or(false))
            .cloned()
            .collect();

        const MAX_REFRESH: usize = 500;
        if to_refresh.len() > MAX_REFRESH {
            to_refresh.truncate(MAX_REFRESH);
        }

        if !to_refresh.is_empty() {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                to_refresh.len()
            );
        }

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed_markets = 0usize;
        let mut refreshed_tokens = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(&token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "failed to fetch gamma market for token");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
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
                tracing::debug!(
                    token_id = %token_id,
                    market_id = %market.id,
                    "gamma market missing clob_token_ids or outcome_prices; skipping"
                );
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            // Treat as "officially settled" only once the market is closed and prices are 1/0.
            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

                if let Err(e) = sqlx::query(
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
                .bind(tid)
                .bind(condition_id.as_deref())
                .bind(&market.id)
                .bind(market_slug.as_deref())
                .bind(outcome.as_deref())
                .bind(settled_price)
                .bind(resolved)
                .bind(resolved_at)
                .bind(sqlx::types::Json(raw_market.clone()))
                .execute(store.pool())
                .await
                {
                    warn!(token_id = %token_id, error = %e, "failed to upsert pm_token_settlements row");
                    continue;
                }

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows\n",
                refreshed_markets, refreshed_tokens
            );
        }
    }

    let settlement_rows = sqlx::query(
        r#"
        SELECT token_id, resolved, settled_price, resolved_at
        FROM pm_token_settlements
        WHERE token_id = ANY($1)
        "#,
    )
    .bind(&token_ids)
    .fetch_all(store.pool())
    .await
    .context("Failed to query pm_token_settlements for signal tokens")?;

    #[derive(Debug, Clone)]
    struct SettlementRow {
        resolved: bool,
        settled_price: Option<Decimal>,
    }

    let mut settlements: HashMap<String, SettlementRow> = HashMap::new();
    for row in settlement_rows {
        let token_id: String = row.get("token_id");
        settlements.insert(
            token_id,
            SettlementRow {
                resolved: row.get("resolved"),
                settled_price: row.get("settled_price"),
            },
        );
    }

    let fee_model = FeeModel::crypto();
    let spread_cost = dec!(0.01);

    let mut total = 0usize;
    let mut resolved = 0usize;
    let mut wins = 0usize;
    let mut sum_pnl = 0.0f64;
    let mut equity = 0.0f64;
    let mut peak = 0.0f64;
    let mut max_dd = 0.0f64;

    let mut by_symbol: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new(); // (n, wins, pnl_sum)

    // Process oldest→newest for drawdown.
    signals.sort_by_key(|s| s.recorded_at);
    for s in &signals {
        total += 1;

        let entry_price_f64 = s.entry_price.to_f64().unwrap_or(0.0);
        let fee_rate = fee_model.effective_rate(s.entry_price);
        let fee_per_share = (s.entry_price * fee_rate).to_f64().unwrap_or(0.0);
        let costs = fee_per_share + spread_cost.to_f64().unwrap_or(0.01);

        let Some(settlement) = settlements.get(&s.token_id) else {
            continue;
        };
        if !settlement.resolved {
            continue;
        }
        let Some(settled_price) = settlement.settled_price else {
            continue;
        };

        resolved += 1;
        let payout = settled_price.to_f64().unwrap_or(0.0);
        let win = payout >= 0.99;
        if win {
            wins += 1;
        }

        let pnl = payout - entry_price_f64 - costs;
        sum_pnl += pnl;
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }

        let sym = s.symbol.clone().unwrap_or_else(|| "UNKNOWN".to_string());
        let entry = by_symbol.entry(sym).or_insert((0, 0, 0.0));
        entry.0 += 1;
        if win {
            entry.1 += 1;
        }
        entry.2 += pnl;
    }

    if resolved == 0 {
        println!("\n  Signals: {} (0 resolved yet). Wait for settlements, or run with longer lookback.\n", total);
        return Ok(());
    }

    let win_rate = wins as f64 / resolved as f64;
    let avg_pnl = sum_pnl / resolved as f64;

    println!(
        "\n  Signals: {} (resolved: {}) | Win rate: {:.1}% | Avg PnL/share: {:+.4} | Total PnL: {:+.4} | Max DD: {:.4}\n",
        total,
        resolved,
        win_rate * 100.0,
        avg_pnl,
        sum_pnl,
        max_dd
    );

    println!("  By symbol (resolved only):");
    for (sym, (n, w, pnl_sum)) in by_symbol {
        if n == 0 {
            continue;
        }
        println!(
            "    {:<8} n={:<5} win={:>5.1}% pnl_sum={:+.4} avg={:+.4}",
            sym,
            n,
            (w as f64 / n as f64) * 100.0,
            pnl_sum,
            pnl_sum / n as f64
        );
    }

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
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use anyhow::bail;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{HashMap, HashSet};

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
        let existing = sqlx::query(
            r#"
            SELECT token_id, resolved
            FROM pm_token_settlements
            WHERE token_id = ANY($1)
            "#,
        )
        .bind(&token_ids)
        .fetch_all(store.pool())
        .await
        .context("Failed to query pm_token_settlements")?;

        let mut resolved_map: HashMap<String, bool> = HashMap::new();
        for row in existing {
            let token_id: String = row.get("token_id");
            let resolved: bool = row.get("resolved");
            resolved_map.insert(token_id, resolved);
        }

        let mut to_refresh: Vec<String> = token_ids
            .iter()
            .filter(|t| !resolved_map.get(*t).copied().unwrap_or(false))
            .cloned()
            .collect();

        const MAX_REFRESH: usize = 500;
        if to_refresh.len() > MAX_REFRESH {
            to_refresh.truncate(MAX_REFRESH);
        }

        if !to_refresh.is_empty() {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                to_refresh.len()
            );
        }

        let pm = PolymarketClient::new("https://clob.polymarket.com", true)
            .context("Failed to create Polymarket client")?;

        let mut refreshed_markets = 0usize;
        let mut refreshed_tokens = 0usize;
        let mut seen_conditions: HashSet<String> = HashSet::new();

        for token_id in to_refresh {
            let market = match pm.get_gamma_market_by_token_id(&token_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(token_id = %token_id, error = %e, "failed to fetch gamma market for token");
                    continue;
                }
            };

            if let Some(ref cond) = market.condition_id {
                let cond_str = cond.to_string();
                if !seen_conditions.insert(cond_str) {
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
                tracing::debug!(
                    token_id = %token_id,
                    market_id = %market.id,
                    "gamma market missing clob_token_ids or outcome_prices; skipping"
                );
                continue;
            }

            let mut prices: Vec<Decimal> = Vec::new();
            for s in &price_strs {
                if let Ok(p) = s.parse::<Decimal>() {
                    prices.push(p);
                }
            }

            let resolved = market.closed.unwrap_or(false) && is_market_resolved(&prices);
            let resolved_at: Option<DateTime<Utc>> = resolved.then(|| Utc::now());
            let raw_market = serde_json::to_value(&market).unwrap_or(serde_json::json!({}));

            let market_slug = market.slug.clone();
            let condition_id = market.condition_id.map(|b| b.to_string());

            for (i, tid) in clob_ids.iter().enumerate() {
                let outcome = outcomes.get(i).cloned();
                let settled_price = price_strs.get(i).and_then(|s| s.parse::<Decimal>().ok());

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
                .bind(tid)
                .bind(condition_id.as_deref())
                .bind(&market.id)
                .bind(market_slug.as_deref())
                .bind(outcome.as_deref())
                .bind(settled_price)
                .bind(resolved)
                .bind(resolved_at)
                .bind(sqlx::types::Json(raw_market.clone()))
                .execute(store.pool())
                .await
                .context("Failed to upsert pm_token_settlements row")?;

                refreshed_tokens += 1;
            }

            refreshed_markets += 1;
        }

        if refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows",
                refreshed_markets, refreshed_tokens
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

        // Require core features for training a DL model (MLP).
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

pub(super) fn is_market_resolved(prices: &[rust_decimal::Decimal]) -> bool {
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
