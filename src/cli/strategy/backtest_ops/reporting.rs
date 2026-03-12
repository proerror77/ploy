use super::*;

pub(crate) async fn run_backtest_list(database_url: Option<String>, limit: usize) -> Result<()> {
    use crate::adapters::PostgresStore;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        Vec<String>,
        Option<i32>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        Option<rust_decimal::Decimal>,
        Option<f64>,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT run_id, strategy, mode, symbols, total_trades, win_rate,
                total_pnl, sharpe_ratio, max_drawdown, profit_factor, created_at
         FROM backtest_runs ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        println!("No backtest runs found.");
        return Ok(());
    }

    println!(
        "\n  {:<36} {:<14} {:<10} {:<8} {:<7} {:<10} {:<7} {:<7} {}",
        "RUN_ID", "STRATEGY", "MODE", "SYMBOLS", "TRADES", "PNL", "WIN%", "SHARPE", "CREATED"
    );
    println!("  {}", "-".repeat(110));

    for (run_id, strategy, mode, symbols, trades, win_rate, pnl, sharpe, _dd, _pf, created) in &rows
    {
        let sym_str = if symbols.len() > 2 {
            format!("{}+{}", symbols[0], symbols.len() - 1)
        } else {
            symbols.join(",")
        };
        println!(
            "  {:<36} {:<14} {:<10} {:<8} {:<7} ${:<9.2} {:<6.1}% {:<7.2} {}",
            run_id,
            strategy,
            mode,
            sym_str,
            trades.unwrap_or(0),
            pnl.unwrap_or(rust_decimal::Decimal::ZERO),
            win_rate.unwrap_or(0.0) * 100.0,
            sharpe.unwrap_or(0.0),
            created.format("%Y-%m-%d %H:%M"),
        );
    }
    println!();

    Ok(())
}

pub(crate) async fn run_backtest_diff(
    run1: &str,
    run2: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;

    let id1: uuid::Uuid = run1.parse().context("Invalid run1 UUID")?;
    let id2: uuid::Uuid = run2.parse().context("Invalid run2 UUID")?;

    let r1 = backtest_report::load_report(store.pool(), id1).await?;
    let r2 = backtest_report::load_report(store.pool(), id2).await?;

    let w = 64;
    let bar = "=".repeat(w);
    let thin = "-".repeat(w);

    println!("\n{}", bar);
    println!("  BACKTEST COMPARISON");
    println!("{}\n", bar);

    println!("  {:<24} {:<20} {:<20}", "METRIC", "RUN A", "RUN B");
    println!("  {}", thin);
    println!(
        "  {:<24} {:<20} {:<20}",
        "Run ID",
        &r1.run.run_id.to_string()[..8],
        &r2.run.run_id.to_string()[..8]
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Strategy", r1.run.strategy, r2.run.strategy
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Trades", r1.run.total_trades, r2.run.total_trades
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Win Rate",
        format!("{:.1}%", r1.run.win_rate * 100.0),
        format!("{:.1}%", r2.run.win_rate * 100.0)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "PnL",
        format!("${:.2}", r1.run.total_pnl),
        format!("${:.2}", r2.run.total_pnl)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Sharpe",
        format!("{:.2}", r1.run.sharpe_ratio),
        format!("{:.2}", r2.run.sharpe_ratio)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Max Drawdown",
        format!(
            "{:.2}%",
            r1.run.max_drawdown * rust_decimal_macros::dec!(100)
        ),
        format!(
            "{:.2}%",
            r2.run.max_drawdown * rust_decimal_macros::dec!(100)
        )
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Profit Factor",
        format!("{:.2}", r1.run.profit_factor),
        format!("{:.2}", r2.run.profit_factor)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Fee Drag",
        format!("{:.1}%", r1.fee_impact.fee_drag_pct),
        format!("{:.1}%", r2.fee_impact.fee_drag_pct)
    );
    println!(
        "  {:<24} {:<20} {:<20}",
        "Calibration Bias",
        format!("{:+.1}%", r1.calibration.overall_bias * 100.0),
        format!("{:+.1}%", r2.calibration.overall_bias * 100.0)
    );
    println!("\n{}\n", bar);

    Ok(())
}

pub(crate) async fn run_live_backtest_compare(
    run_id: &str,
    lookback_hours: u64,
    account_id: Option<String>,
    strategy_id: Option<String>,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::backtest_report;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::ToPrimitive;
    use sqlx::Row;
    use std::collections::HashSet;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    crate::persistence::ensure_strategy_observability_tables(store.pool())
        .await
        .context("Failed to ensure strategy observability tables")?;

    let bt_run_id: uuid::Uuid = run_id.parse().context("Invalid run UUID")?;
    let report = backtest_report::load_report(store.pool(), bt_run_id).await?;

    let signal_types = vec![
        "live_order_submit_result".to_string(),
        "live_order_poll_update".to_string(),
        "live_order_rejected".to_string(),
        "live_order_submit_error".to_string(),
    ];

    let rows = sqlx::query(
        r#"
        SELECT
            signal_type,
            side,
            fair_value,
            market_price,
            context
        FROM signal_history
        WHERE recorded_at >= NOW() - ($1::bigint * INTERVAL '1 hour')
          AND signal_type = ANY($2)
          AND ($3::text IS NULL OR account_id = $3)
          AND ($4::text IS NULL OR strategy_id = $4)
        ORDER BY recorded_at DESC
        "#,
    )
    .bind(lookback_hours as i64)
    .bind(&signal_types)
    .bind(account_id.as_deref())
    .bind(strategy_id.as_deref())
    .fetch_all(store.pool())
    .await
    .context("Failed to query live order observations from signal_history")?;

    let mut submitted: HashSet<String> = HashSet::new();
    let mut rejected: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut touched_fill: HashSet<String> = HashSet::new();
    let mut full_fill: HashSet<String> = HashSet::new();
    let mut slippage_bps_weighted_sum = 0.0f64;
    let mut slippage_weight = 0.0f64;

    for row in rows {
        let signal_type: String = row.get("signal_type");
        let side: Option<String> = row.get("side");
        let limit_price: Option<Decimal> = row.get("fair_value");
        let fill_price: Option<Decimal> = row.get("market_price");
        let context: serde_json::Value = row.get("context");

        let order_key = context
            .get("client_order_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| {
                context
                    .get("order_id")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
            });

        let Some(order_key) = order_key else { continue };
        let status = context
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let filled_qty = context
            .get("filled_qty")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match signal_type.as_str() {
            "live_order_submit_result" => {
                submitted.insert(order_key.clone());
            }
            "live_order_rejected" => {
                submitted.insert(order_key.clone());
                rejected.insert(order_key.clone());
            }
            "live_order_submit_error" => {
                submitted.insert(order_key.clone());
                failed.insert(order_key.clone());
            }
            _ => {}
        }

        if filled_qty > 0
            || status.eq_ignore_ascii_case("filled")
            || status.eq_ignore_ascii_case("partiallyfilled")
        {
            touched_fill.insert(order_key.clone());
        }
        if status.eq_ignore_ascii_case("filled") {
            full_fill.insert(order_key.clone());
        }

        if filled_qty > 0 {
            if let (Some(limit_px), Some(fill_px)) = (limit_price, fill_price) {
                if limit_px > Decimal::ZERO {
                    if let (Some(limit_f64), Some(fill_f64)) = (limit_px.to_f64(), fill_px.to_f64())
                    {
                        let side_lower = side.unwrap_or_else(|| "buy".to_string()).to_lowercase();
                        let slip_bps = if side_lower == "sell" {
                            (limit_f64 - fill_f64) / limit_f64 * 10_000.0
                        } else {
                            (fill_f64 - limit_f64) / limit_f64 * 10_000.0
                        };
                        let weight = filled_qty as f64;
                        slippage_bps_weighted_sum += slip_bps * weight;
                        slippage_weight += weight;
                    }
                }
            }
        }
    }

    let submitted_n = submitted.len();
    let rejected_n = rejected.len();
    let failed_n = failed.len();
    let touched_fill_n = touched_fill.len();
    let full_fill_n = full_fill.len();

    let live_fill_rate = if submitted_n > 0 {
        touched_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_full_fill_rate = if submitted_n > 0 {
        full_fill_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_reject_rate = if submitted_n > 0 {
        rejected_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let live_failed_rate = if submitted_n > 0 {
        failed_n as f64 / submitted_n as f64
    } else {
        0.0
    };
    let avg_slippage_bps = if slippage_weight > 0.0 {
        slippage_bps_weighted_sum / slippage_weight
    } else {
        0.0
    };

    let bt_trades = report.run.total_trades.max(0) as usize;
    let live_vs_bt_trade_ratio = if bt_trades > 0 {
        touched_fill_n as f64 / bt_trades as f64
    } else {
        0.0
    };

    println!("\n{}", "=".repeat(78));
    println!("  LIVE VS BACKTEST");
    println!("{}", "=".repeat(78));
    println!(
        "  backtest_run={}  lookback_hours={}  account_id={}  strategy_id={}",
        report.run.run_id,
        lookback_hours,
        account_id.as_deref().unwrap_or("all"),
        strategy_id.as_deref().unwrap_or("all")
    );
    println!();
    println!("  Backtest:");
    println!(
        "    strategy={} mode={} trades={} win_rate={:.1}% pnl=${:.2} sharpe={:.2}",
        report.run.strategy,
        report.run.mode,
        report.run.total_trades,
        report.run.win_rate * 100.0,
        report.run.total_pnl,
        report.run.sharpe_ratio
    );
    println!("  Live:");
    println!(
        "    submitted={} touched_fill={} full_fill={} rejected={} failed={}",
        submitted_n, touched_fill_n, full_fill_n, rejected_n, failed_n
    );
    println!(
        "    fill_rate={:.1}% full_fill_rate={:.1}% reject_rate={:.1}% failed_rate={:.1}% avg_slippage_bps={:.2}",
        live_fill_rate * 100.0,
        live_full_fill_rate * 100.0,
        live_reject_rate * 100.0,
        live_failed_rate * 100.0,
        avg_slippage_bps
    );
    println!(
        "  Coverage (live_filled_orders / backtest_trades): {:.2}x",
        live_vs_bt_trade_ratio
    );
    println!();

    Ok(())
}
