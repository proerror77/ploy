use ploy::error::Result;

pub async fn run_history(
    limit: usize,
    symbol: Option<String>,
    stats_only: bool,
    open_only: bool,
) -> Result<()> {
    use ploy::strategy::TradeLogger;
    use std::path::PathBuf;

    let logger = TradeLogger::new(PathBuf::from("data/trades.json"));

    if let Err(e) = logger.load().await {
        eprintln!("Warning: Could not load trades: {}", e);
    }

    let stats = logger.get_stats().await;

    if stats.total_trades == 0 {
        println!("\n  No trading history found.");
        println!("  Trade data will be saved to: data/trades.json\n");
        return Ok(());
    }

    println!("{}", logger.format_stats().await);

    if stats_only {
        return Ok(());
    }

    let trades = if open_only {
        logger.get_open_trades().await
    } else if let Some(ref sym) = symbol {
        logger.get_trades_by_symbol(sym).await
    } else {
        logger.get_recent_trades(limit).await
    };

    if trades.is_empty() {
        if open_only {
            println!("\n  No open trades.\n");
        } else if let Some(sym) = &symbol {
            println!("\n  No trades for symbol: {}\n", sym);
        }
        return Ok(());
    }

    println!("\n  ── Recent Trades ───────────────────────────────────────────\n");
    println!("  Time                Symbol     Dir   Price   Shares  PnL      Status");
    println!("  ──────────────────  ─────────  ────  ──────  ──────  ───────  ──────");

    for trade in trades {
        let outcome_str = match &trade.outcome {
            ploy::strategy::TradeOutcome::Open => "OPEN",
            ploy::strategy::TradeOutcome::Won => "WON",
            ploy::strategy::TradeOutcome::Lost => "LOST",
            ploy::strategy::TradeOutcome::ExitedEarly { .. } => "EXIT",
            ploy::strategy::TradeOutcome::Cancelled => "CANCEL",
        };

        let pnl_str = match trade.pnl_usd {
            Some(pnl) => format!("${:+.2}", pnl),
            None => "-".to_string(),
        };

        println!(
            "  {}  {:10} {:4}  {:5.1}¢  {:6}  {:>7}  {}",
            trade.timestamp.format("%Y-%m-%d %H:%M"),
            trade.symbol,
            trade.direction,
            trade.entry_price * rust_decimal_macros::dec!(100),
            trade.shares,
            pnl_str,
            outcome_str
        );
    }

    println!();
    Ok(())
}
