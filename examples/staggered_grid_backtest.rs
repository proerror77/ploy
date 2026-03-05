use std::str::FromStr;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ploy::adapters::PostgresStore;
use ploy::strategy::backtest_feed::{HistoricalFeed, MarketFeed, MarketUpdate};
use ploy::strategy::backtest_recorder::NullRecorder;
use ploy::strategy::staggered_arb_backtest::{
    StaggeredArbBacktestConfig, StaggeredArbBacktestEngine,
};
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Summary {
    max_initial_sum: String,
    start_time: chrono::DateTime<Utc>,
    end_time: chrono::DateTime<Utc>,
    total_trades: u64,
    winning_trades: u64,
    losing_trades: u64,
    win_rate: f64,
    total_pnl: String,
    avg_pnl_per_trade: String,
    max_drawdown: String,
    sharpe_ratio: f64,
    profit_factor: f64,
    avg_holding_time_secs: f64,
}

#[derive(Clone)]
struct SharedFeed {
    updates: Arc<Vec<MarketUpdate>>,
    idx: usize,
}

impl SharedFeed {
    fn new(updates: Arc<Vec<MarketUpdate>>) -> Self {
        Self { updates, idx: 0 }
    }
}

impl MarketFeed for SharedFeed {
    fn next_update(&mut self) -> Option<MarketUpdate> {
        let update = self.updates.get(self.idx).cloned();
        if update.is_some() {
            self.idx += 1;
        }
        update
    }
}

fn parse_arg(args: &[String], key: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == key)
        .map(|w| w[1].clone())
}

fn run_one(
    updates: Arc<Vec<MarketUpdate>>,
    symbols: Vec<String>,
    initial_capital: Decimal,
    max_initial_sum: Decimal,
) -> Summary {
    let mut config = StaggeredArbBacktestConfig::with_symbols(symbols);
    config.initial_capital = initial_capital;
    config.max_initial_sum = max_initial_sum;

    let mut engine = StaggeredArbBacktestEngine::new(config, Box::new(NullRecorder));
    let mut feed = SharedFeed::new(updates);
    let results = engine.run(&mut feed);

    Summary {
        max_initial_sum: max_initial_sum.to_string(),
        start_time: results.start_time,
        end_time: results.end_time,
        total_trades: results.total_trades,
        winning_trades: results.winning_trades,
        losing_trades: results.losing_trades,
        win_rate: results.win_rate,
        total_pnl: results.total_pnl.to_string(),
        avg_pnl_per_trade: results.avg_pnl_per_trade.to_string(),
        max_drawdown: results.max_drawdown.to_string(),
        sharpe_ratio: results.sharpe_ratio,
        profit_factor: results.profit_factor,
        avg_holding_time_secs: results.avg_holding_time_secs,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let from_raw = parse_arg(&args, "--from")
        .context("missing --from (ISO8601, e.g. 2026-03-01T00:00:00Z)")?;
    let to_raw = parse_arg(&args, "--to")
        .context("missing --to (ISO8601, e.g. 2026-03-04T00:00:00Z)")?;
    let max_initial_sums_raw = parse_arg(&args, "--max-initial-sums")
        .unwrap_or_else(|| "0.80,0.85,0.90".to_string());
    let symbols_raw =
        parse_arg(&args, "--symbols").unwrap_or_else(|| "BTCUSDT,ETHUSDT,SOLUSDT".to_string());
    let capital_raw = parse_arg(&args, "--capital").unwrap_or_else(|| "10000".to_string());

    let from_dt = DateTime::parse_from_rfc3339(&from_raw)
        .with_context(|| format!("invalid --from: {}", from_raw))?
        .with_timezone(&Utc);
    let to_dt = DateTime::parse_from_rfc3339(&to_raw)
        .with_context(|| format!("invalid --to: {}", to_raw))?
        .with_timezone(&Utc);

    let sums: Vec<Decimal> = max_initial_sums_raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Decimal::from_str(s).with_context(|| format!("invalid threshold: {}", s)))
        .collect::<Result<Vec<_>>>()?;
    if sums.is_empty() {
        anyhow::bail!("--max-initial-sums cannot be empty");
    }

    let initial_capital = Decimal::from_str(&capital_raw)
        .with_context(|| format!("invalid --capital: {}", capital_raw))?;

    let symbols: Vec<String> = symbols_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string());
    let store = PostgresStore::new(&db_url, 5).await?;
    let mut feed = HistoricalFeed::from_database(
        store.pool(),
        &symbols,
        Some(from_dt),
        Some(to_dt),
    )
    .await?;

    let mut updates = Vec::with_capacity(feed.len());
    while let Some(u) = feed.next_update() {
        updates.push(u);
    }
    let shared_updates = Arc::new(updates);

    let handles: Vec<_> = sums
        .iter()
        .copied()
        .map(|sum| {
            let updates = Arc::clone(&shared_updates);
            let symbols = symbols.clone();
            thread::spawn(move || run_one(updates, symbols, initial_capital, sum))
        })
        .collect();

    let mut summaries: Vec<Summary> = handles
        .into_iter()
        .map(|h| h.join().map_err(|_| anyhow::anyhow!("worker thread panicked")))
        .collect::<Result<Vec<_>>>()?;
    summaries.sort_by(|a, b| a.max_initial_sum.cmp(&b.max_initial_sum));

    println!("{}", serde_json::to_string(&summaries)?);
    Ok(())
}
