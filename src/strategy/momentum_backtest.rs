//! Momentum strategy backtest engine.
//!
//! Reuses the live `MomentumDetector.check()` and `SpotPrice` types to ensure
//! the backtest signal logic is identical to production. "Change strategy once,
//! live + backtest automatically agree."
//!
//! Usage:
//!   ploy strategy backtest momentum --symbols BTCUSDT --save --json

use std::collections::HashMap;
use std::fmt;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info};

use crate::adapters::SpotPrice;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::momentum::{Direction, MomentumConfig, MomentumDetector, MomentumSignal};

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a momentum backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumBacktestConfig {
    pub momentum_config: MomentumConfig,
    pub symbols: Vec<String>,
    pub initial_capital: Decimal,
    pub max_concurrent_positions: usize,
    pub cooldown_secs: u64,
}

impl MomentumBacktestConfig {
    pub fn default_with_symbols(symbols: Vec<String>, initial_capital: Decimal) -> Self {
        let mut momentum_config = MomentumConfig::default();
        momentum_config.symbols = symbols.clone();
        Self {
            momentum_config,
            symbols,
            initial_capital,
            max_concurrent_positions: 5,
            cooldown_secs: 30,
        }
    }

    /// SHA-256 hash of the serialized config for deduplication.
    pub fn config_hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let json = serde_json::to_string(self).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        json.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

// ─────────────────────────────────────────────────────────────
// Position tracking
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BacktestPosition {
    symbol: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    event_slug: Option<String>,
    /// Latest PM ask for this direction (for exit tracking)
    latest_pm_price: Decimal,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct MomentumBacktestEngine {
    config: MomentumBacktestConfig,
    detector: MomentumDetector,
    execution_sim: ExecutionSimulator,
    spot_prices: HashMap<String, SpotPrice>,
    /// Latest PM asks per symbol: (up_ask, down_ask)
    pm_asks: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    positions: Vec<BacktestPosition>,
    closed_trades: Vec<BacktestClosedTrade>,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BacktestClosedTrade {
    symbol: String,
    direction: String,
    entry_time: DateTime<Utc>,
    exit_time: DateTime<Utc>,
    entry_price: Decimal,
    exit_price: Decimal,
    shares: u64,
    pnl: Decimal,
    won: bool,
    holding_secs: i64,
}

impl MomentumBacktestEngine {
    pub fn new(config: MomentumBacktestConfig) -> Self {
        let detector = MomentumDetector::new(config.momentum_config.clone());
        let execution_sim = ExecutionSimulator::new();
        let equity = config.initial_capital;

        Self {
            config,
            detector,
            execution_sim,
            spot_prices: HashMap::new(),
            pm_asks: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
        }
    }

    pub fn config(&self) -> &MomentumBacktestConfig {
        &self.config
    }

    /// Main backtest loop — consumes the feed and returns results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            // Track data range
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote { up_ask, down_ask } => {
                    self.handle_pm_quote(&update.symbol, *up_ask, *down_ask, update.timestamp);
                }
                UpdateType::EventState {
                    outcome: Some(won), ..
                } => {
                    self.resolve_positions(&update.symbol, *won, update.timestamp);
                }
                UpdateType::EventState { .. } => {
                    // Metadata update — could be used for time-remaining filtering
                }
            }
        }

        self.build_results()
    }

    // ─── Event handlers ──────────────────────────────────────

    fn handle_spot_trade(
        &mut self,
        symbol: &str,
        price: Decimal,
        quantity: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update SpotPrice (same struct as live — maintains rolling history)
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks
        let entry = self
            .pm_asks
            .entry(symbol.to_string())
            .or_insert((None, None));
        if up_ask.is_some() {
            entry.0 = up_ask;
        }
        if down_ask.is_some() {
            entry.1 = down_ask;
        }

        // Update position mark-to-market
        for pos in &mut self.positions {
            if pos.symbol == symbol {
                match pos.direction {
                    Direction::Up => {
                        if let Some(ask) = up_ask {
                            pos.latest_pm_price = ask;
                        }
                    }
                    Direction::Down => {
                        if let Some(ask) = down_ask {
                            pos.latest_pm_price = ask;
                        }
                    }
                }
            }
        }

        // Check for new entry signal via MomentumDetector.check()
        // (This is the EXACT same method used in live trading!)
        if let Some(spot) = self.spot_prices.get(symbol) {
            let (ua, da) = self.pm_asks.get(symbol).copied().unwrap_or((None, None));
            if let Some(signal) = self.detector.check(symbol, spot, ua, da) {
                self.try_entry(&signal, ts);
            }
        }

        // Check exits for existing positions
        self.check_exits(ts);

        // Record equity curve
        self.record_equity(ts);
    }

    fn try_entry(&mut self, signal: &MomentumSignal, ts: DateTime<Utc>) {
        // Cooldown check
        if let Some(last) = self.last_entry_time.get(&signal.symbol) {
            let elapsed = (ts - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                return;
            }
        }

        // Max positions check
        if self.positions.len() >= self.config.max_concurrent_positions {
            return;
        }

        // Don't enter if we already hold the same symbol+direction
        let already_holding = self.positions.iter().any(|p| {
            p.symbol == signal.symbol
                && std::mem::discriminant(&p.direction) == std::mem::discriminant(&signal.direction)
        });
        if already_holding {
            return;
        }

        // Simulate execution
        let sim_result = self.execution_sim.simulate_buy(
            signal.pm_price,
            ts,
            self.config.momentum_config.shares_per_trade,
            10_000, // market depth assumption
        );

        let cost = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        if cost > self.equity {
            debug!(
                "Skipping entry: insufficient equity ({} < {})",
                self.equity, cost
            );
            return;
        }

        self.equity -= cost;

        self.positions.push(BacktestPosition {
            symbol: signal.symbol.clone(),
            direction: signal.direction.clone(),
            entry_price: sim_result.fill_price,
            entry_time: ts,
            shares: sim_result.filled_shares,
            event_slug: None,
            latest_pm_price: signal.pm_price,
        });

        self.last_entry_time.insert(signal.symbol.clone(), ts);
    }

    fn check_exits(&mut self, ts: DateTime<Utc>) {
        // Exit conditions: price moved against us, or time-based stop
        let mut to_close = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            let holding_secs = (ts - pos.entry_time).num_seconds();

            // Max holding time: 15 minutes (typical event duration)
            if holding_secs > 900 {
                to_close.push((i, pos.latest_pm_price, "timeout"));
                continue;
            }

            // Stop-loss: PM price increased 30% from entry (getting more expensive = bad)
            if pos.latest_pm_price > pos.entry_price * dec!(1.30) {
                to_close.push((i, pos.latest_pm_price, "stop_loss"));
            }
        }

        // Close in reverse order to preserve indices
        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price, _reason) in to_close {
            self.close_position(idx, exit_price, ts);
        }
    }

    fn resolve_positions(&mut self, symbol: &str, up_won: bool, ts: DateTime<Utc>) {
        // Settlement: positions that picked the winning side get $1.00 per share,
        // losing side gets $0.00.
        let mut to_close = Vec::new();

        for (i, pos) in self.positions.iter().enumerate() {
            if pos.symbol == symbol {
                let exit_price = match (&pos.direction, up_won) {
                    (Direction::Up, true) | (Direction::Down, false) => Decimal::ONE,
                    _ => Decimal::ZERO,
                };
                to_close.push((i, exit_price));
            }
        }

        to_close.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, exit_price) in to_close {
            self.close_position(idx, exit_price, ts);
        }
    }

    fn close_position(&mut self, idx: usize, exit_price: Decimal, ts: DateTime<Utc>) {
        let pos = self.positions.remove(idx);

        // Simulate sell
        let sim_result = self
            .execution_sim
            .simulate_sell(exit_price, ts, pos.shares, 10_000);

        let proceeds = Decimal::from(sim_result.filled_shares) * sim_result.fill_price;
        self.equity += proceeds;

        let pnl = proceeds - Decimal::from(pos.shares) * pos.entry_price;
        let holding_secs = (ts - pos.entry_time).num_seconds();

        self.closed_trades.push(BacktestClosedTrade {
            symbol: pos.symbol,
            direction: format!("{}", pos.direction),
            entry_time: pos.entry_time,
            exit_time: ts,
            entry_price: pos.entry_price,
            exit_price: sim_result.fill_price,
            shares: pos.shares,
            pnl,
            won: pnl > Decimal::ZERO,
            holding_secs,
        });
    }

    fn record_equity(&mut self, ts: DateTime<Utc>) {
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let drawdown = if self.peak_equity > Decimal::ZERO {
            (self.peak_equity - self.equity) / self.peak_equity
        } else {
            Decimal::ZERO
        };
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        // Sample equity curve (max 1 point per second to avoid bloat)
        let should_record = self
            .equity_curve
            .last()
            .map(|(last_ts, _)| (ts - *last_ts).num_seconds() >= 1)
            .unwrap_or(true);
        if should_record {
            self.equity_curve.push((ts, self.equity));
        }
    }

    // ─── Results ─────────────────────────────────────────────

    fn build_results(&self) -> BacktestResults {
        let total = self.closed_trades.len() as u64;
        let winning = self.closed_trades.iter().filter(|t| t.won).count() as u64;
        let losing = total - winning;
        let total_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();

        let win_rate = if total > 0 {
            winning as f64 / total as f64
        } else {
            0.0
        };

        let avg_pnl = if total > 0 {
            total_pnl / Decimal::from(total)
        } else {
            Decimal::ZERO
        };

        let wins: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.pnl)
            .collect();
        let losses: Vec<Decimal> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.pnl)
            .collect();

        let avg_win = if wins.is_empty() {
            Decimal::ZERO
        } else {
            wins.iter().sum::<Decimal>() / Decimal::from(wins.len() as u64)
        };
        let avg_loss = if losses.is_empty() {
            Decimal::ZERO
        } else {
            losses.iter().sum::<Decimal>() / Decimal::from(losses.len() as u64)
        };

        let largest_win = wins.iter().max().copied().unwrap_or(Decimal::ZERO);
        let largest_loss = losses.iter().min().copied().unwrap_or(Decimal::ZERO);

        let total_wins: Decimal = wins.iter().sum();
        let total_losses_abs: Decimal = losses.iter().map(|l| l.abs()).sum();
        let profit_factor = if total_losses_abs > Decimal::ZERO {
            (total_wins / total_losses_abs).to_f64().unwrap_or(0.0)
        } else if total_wins > Decimal::ZERO {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_holding = if total > 0 {
            self.closed_trades
                .iter()
                .map(|t| t.holding_secs as f64)
                .sum::<f64>()
                / total as f64
        } else {
            0.0
        };

        // Simplified Sharpe: mean(trade_pnl) / std(trade_pnl) * sqrt(252)
        let sharpe = self.calculate_sharpe();

        // Total volume
        let total_volume: Decimal = self
            .closed_trades
            .iter()
            .map(|t| Decimal::from(t.shares) * t.entry_price)
            .sum();

        let start_time = self.data_range_start.unwrap_or(Utc::now());
        let end_time = self.data_range_end.unwrap_or(Utc::now());

        BacktestResults {
            start_time,
            end_time,
            total_trades: total,
            winning_trades: winning,
            losing_trades: losing,
            win_rate,
            total_pnl,
            total_volume,
            avg_pnl_per_trade: avg_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: sharpe,
            profit_factor,
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            avg_holding_time_secs: avg_holding,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(), // full BacktestTrade list omitted for momentum
            equity_curve: self.equity_curve.clone(),
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        if self.closed_trades.len() < 2 {
            return 0.0;
        }

        let pnls: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|t| t.pnl.to_f64().unwrap_or(0.0))
            .collect();

        let n = pnls.len() as f64;
        let mean = pnls.iter().sum::<f64>() / n;
        let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std_dev = variance.sqrt();

        if std_dev < 1e-10 {
            return 0.0;
        }

        // Annualize: assume ~24 trades/day for 15-min markets
        let trades_per_year: f64 = 24.0 * 365.0;
        (mean / std_dev) * trades_per_year.sqrt()
    }
}

// ─────────────────────────────────────────────────────────────
// Display for BacktestResults
// ─────────────────────────────────────────────────────────────

impl fmt::Display for BacktestResults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== Momentum Backtest Results ===")?;
        writeln!(
            f,
            "Period:        {} to {}",
            self.start_time.format("%Y-%m-%d %H:%M"),
            self.end_time.format("%Y-%m-%d %H:%M")
        )?;
        writeln!(f, "Total trades:  {}", self.total_trades)?;
        writeln!(
            f,
            "Win/Loss:      {} / {}",
            self.winning_trades, self.losing_trades
        )?;
        writeln!(f, "Win rate:      {:.1}%", self.win_rate * 100.0)?;
        writeln!(f, "Total PnL:     ${:.2}", self.total_pnl)?;
        writeln!(f, "Avg PnL/trade: ${:.4}", self.avg_pnl_per_trade)?;
        writeln!(f, "Sharpe ratio:  {:.2}", self.sharpe_ratio)?;
        writeln!(f, "Profit factor: {:.2}", self.profit_factor)?;
        writeln!(f, "Max drawdown:  {:.2}%", self.max_drawdown * dec!(100))?;
        writeln!(f, "Avg hold time: {:.0}s", self.avg_holding_time_secs)?;
        writeln!(f, "Largest win:   ${:.4}", self.largest_win)?;
        writeln!(f, "Largest loss:  ${:.4}", self.largest_loss)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
// Result persistence (Step 9)
// ─────────────────────────────────────────────────────────────

/// Save backtest results to `strategy_evaluations` + `backtest_runs` tables.
pub async fn save_backtest_results(
    pool: &PgPool,
    config: &MomentumBacktestConfig,
    results: &BacktestResults,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    // Determine evaluation status from Sharpe ratio
    let status = if results.sharpe_ratio > 1.0 {
        "PASS"
    } else if results.sharpe_ratio > 0.5 {
        "WARN"
    } else {
        "FAIL"
    };

    // 1. Insert strategy_evaluations row
    let eval_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO strategy_evaluations (
            evaluated_at, strategy_id, domain, stage, status,
            score, pnl_usd, win_rate, sharpe,
            max_drawdown_pct, evidence_kind
        )
        VALUES (NOW(), 'momentum', 'crypto', 'BACKTEST', $1,
                $2, $3, $4, $5, $6, 'backtest_run')
        RETURNING id
        "#,
    )
    .bind(status)
    .bind(results.sharpe_ratio)
    .bind(results.total_pnl)
    .bind(Decimal::from_f64(results.win_rate).unwrap_or(Decimal::ZERO))
    .bind(Decimal::from_f64(results.sharpe_ratio).unwrap_or(Decimal::ZERO))
    .bind(results.max_drawdown)
    .fetch_one(&mut *tx)
    .await?;

    // 2. Insert backtest_runs row with full detail
    let config_json = serde_json::to_value(config)?;
    let equity_json = serde_json::to_value(&results.equity_curve)?;

    sqlx::query(
        r#"
        INSERT INTO backtest_runs (
            evaluation_id, strategy_id, config_hash, config_json,
            started_at, completed_at,
            data_range_start, data_range_end,
            total_trades, winning_trades, losing_trades, win_rate,
            total_pnl, sharpe_ratio, max_drawdown_pct,
            profit_factor, avg_trade_pnl, avg_holding_secs,
            equity_curve
        )
        VALUES (
            $1, 'momentum', $2, $3,
            NOW(), NOW(),
            $4, $5,
            $6, $7, $8, $9,
            $10, $11, $12,
            $13, $14, $15,
            $16
        )
        "#,
    )
    .bind(eval_id)
    .bind(&config.config_hash())
    .bind(&config_json)
    .bind(results.start_time)
    .bind(results.end_time)
    .bind(results.total_trades as i32)
    .bind(results.winning_trades as i32)
    .bind(results.losing_trades as i32)
    .bind(Decimal::from_f64(results.win_rate).unwrap_or(Decimal::ZERO))
    .bind(results.total_pnl)
    .bind(Decimal::from_f64(results.sharpe_ratio).unwrap_or(Decimal::ZERO))
    .bind(results.max_drawdown)
    .bind(Decimal::from_f64(results.profit_factor).unwrap_or(Decimal::ZERO))
    .bind(results.avg_pnl_per_trade)
    .bind(results.avg_holding_time_secs as i64)
    .bind(&equity_json)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    info!(
        "Saved backtest: evaluation #{}, status={}, sharpe={:.2}, pnl=${:.2}",
        eval_id, status, results.sharpe_ratio, results.total_pnl
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use std::collections::VecDeque;

    /// Build a simple mock feed for testing
    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    #[test]
    fn test_engine_empty_feed() {
        let config =
            MomentumBacktestConfig::default_with_symbols(vec!["BTCUSDT".into()], dec!(10000));
        let mut engine = MomentumBacktestEngine::new(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_sharpe_calculation() {
        let config =
            MomentumBacktestConfig::default_with_symbols(vec!["BTCUSDT".into()], dec!(10000));
        let engine = MomentumBacktestEngine::new(config);
        // With no trades, sharpe should be 0
        assert_eq!(engine.calculate_sharpe(), 0.0);
    }
}
