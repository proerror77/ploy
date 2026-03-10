use super::*;

mod accuracy_report;
mod dataset_export;
mod settlement_refresh;

/// Strategy-related commands

pub(super) use accuracy_report::report_accuracy_pm_settlement;

pub(super) async fn backtest_directional_signals_pm_settlement(
    lookback_hours: u64,
    account_id: Option<String>,
    agent_id: Option<String>,
    live_only: bool,
    limit: usize,
    no_refresh: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::fee_model::FeeModel;
    use chrono::{DateTime, Utc};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use sqlx::Row;
    use std::collections::{BTreeMap, HashMap};

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
        const MAX_REFRESH: usize = 500;
        let summary = settlement_refresh::refresh_pm_token_settlements_for_tokens(
            store.pool(),
            &token_ids,
            MAX_REFRESH,
        )
        .await?;

        if summary.requested_tokens > 0 {
            println!(
                "\n  Refreshing settlement status for {} token(s) via Gamma...",
                summary.requested_tokens
            );
        }
        if summary.refreshed_markets > 0 {
            println!(
                "  ✓ Refreshed {} market(s), {} token rows\n",
                summary.refreshed_markets, summary.refreshed_tokens
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
    dataset_export::export_crypto_lob_dataset(
        lookback_hours,
        account_id,
        agent_id,
        live_only,
        no_refresh,
        limit,
        format,
        output,
        database_url,
    )
    .await
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
