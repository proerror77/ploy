use anyhow::bail;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::Row;
use std::collections::BTreeMap;

use crate::adapters::PostgresStore;

use super::settlement_refresh::refresh_pm_token_settlements_for_tokens;
use super::*;

pub(in crate::cli::strategy) async fn report_accuracy_pm_settlement(
    lookback_hours: u64,
    domain: Option<String>,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
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
    let mut by_agent: BTreeMap<String, (usize, usize)> = BTreeMap::new();

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

        let meta: serde_json::Value = row.try_get("metadata").unwrap_or(serde_json::Value::Null);
        let p_up_opt = meta
            .get("p_up")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<f64>().ok())
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
        for (agent, (agent_scored, agent_wins)) in &by_agent {
            let agent_acc = if *agent_scored > 0 {
                100.0 * (*agent_wins as f64) / (*agent_scored as f64)
            } else {
                0.0
            };
            println!(
                "  - {:<20} scored={:<5} wins={:<5} acc={:.2}%",
                agent, agent_scored, agent_wins, agent_acc
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
            for (agent, agg) in &pred_by_agent {
                if agg.n == 0 {
                    continue;
                }
                let agent_acc = 100.0 * (agg.correct as f64) / (agg.n as f64);
                let agent_brier = agg.brier_sum / (agg.n as f64);
                let agent_logloss = agg.logloss_sum / (agg.n as f64);
                println!(
                    "  - {:<20} n={:<5} acc={:>6.2}% brier={:.6} ll={:.6}",
                    agent, agg.n, agent_acc, agent_brier, agent_logloss
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
                let settled = settled_price.unwrap_or(Decimal::ZERO);
                let won = if is_buy {
                    settled > dec!(0.5)
                } else {
                    settled < dec!(0.5)
                };
                (
                    format!("{:.3}", settled),
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
