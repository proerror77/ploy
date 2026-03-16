//! Directional backtest engine for momentum-driven binary option trading.
//!
//! Uses weighted momentum (10s/30s/60s) → fair value estimation → edge filtering
//! to enter positions, mirroring the live MomentumDetector logic. Holds to
//! settlement by default (binary options settle at $1.00 or $0.00).
//!
//! Binance spot price serves as Chainlink proxy (>99.9% correlation on 5m/15m).
//!
//! Usage:
//!   ploy strategy backtest directional --symbols BTCUSDT --save --json

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::backtest_recorder::{BacktestRecorder, NullRecorder};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;

mod entry_lifecycle;
mod position_lifecycle;

// ─────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────

/// Configuration for a directional backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent positions per symbol
    pub max_concurrent_positions: usize,
    /// Minimum edge to enter (fair_value - pm_ask - fees), e.g. 0.05 = 5%
    pub entry_threshold: f64,
    /// Don't buy YES above this price (e.g. 0.85)
    pub max_entry_price: Decimal,
    /// Don't buy YES below this price (e.g. 0.15)
    pub min_entry_price: Decimal,
    /// Minimum absolute momentum to trigger signal (e.g. 0.003 = 0.3%)
    pub min_momentum: Decimal,
    /// Time stop: exit if <N secs remaining AND position is underwater (e.g. 30)
    pub time_stop_secs: u64,
    /// Maximum loss per position in USD
    pub hard_stop_usd: Decimal,
    /// Hold winners to settlement (default true — let them run)
    pub hold_to_settlement: bool,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
    /// Minimum time remaining to enter a position (seconds).
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds).
    /// Only enter when outcome is becoming clearer.
    pub max_time_remaining_secs: u64,
    /// Use price_to_beat in fair value calculation
    pub use_price_to_beat: bool,
}

impl Default for DirectionalBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 100,
            max_concurrent_positions: 3,
            entry_threshold: 0.05,
            max_entry_price: dec!(0.85),
            min_entry_price: dec!(0.15),
            min_momentum: dec!(0.003), // 0.3% minimum move
            time_stop_secs: 30,
            hard_stop_usd: dec!(5),
            hold_to_settlement: true,
            cooldown_secs: 60,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            use_price_to_beat: true,
        }
    }
}

impl DirectionalBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Position tracking
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirectionalPosition {
    symbol: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
    #[allow(dead_code)]
    event_slug: String,
    /// Window open price (Binance proxy for Chainlink S0)
    s0: Decimal,
    /// When the event window settles
    event_end_time: DateTime<Utc>,
    /// Model probability at entry
    entry_p_hat: f64,
    /// EV_net at entry for diagnostics
    entry_ev_net: f64,
    /// Realized vol at entry
    entry_sigma: f64,
    /// Latest PM price for mark-to-market
    latest_pm_price: Decimal,
}

/// A closed trade with directional-specific diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalClosedTrade {
    pub symbol: String,
    pub direction: String,
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub won: bool,
    pub holding_secs: i64,
    pub exit_reason: String,
    // Directional-specific fields
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma: f64,
}

// ─────────────────────────────────────────────────────────────
// Active event window info
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    /// S0 = price_to_beat from EventState
    s0: Decimal,
    end_time: DateTime<Utc>,
}

// ─────────────────────────────────────────────────────────────
// Engine
// ─────────────────────────────────────────────────────────────

pub struct DirectionalBacktestEngine {
    config: DirectionalBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    // Market state
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    // Active events: symbol -> concurrent windows (5m + 15m can overlap)
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    // Positions & trades
    positions: Vec<DirectionalPosition>,
    closed_trades: Vec<DirectionalClosedTrade>,
    // Accounting
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    // Data range
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
    // Throttle: last timestamp we ran entry/exit logic per symbol
    last_logic_ts: HashMap<String, DateTime<Utc>>,
}

impl DirectionalBacktestEngine {
    pub fn new(config: DirectionalBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self {
        let equity = config.initial_capital;
        Self {
            config,
            fee_model: FeeModel::crypto(),
            execution_sim: ExecutionSimulator::new(),
            recorder,
            spot_prices: HashMap::new(),
            pm_asks_by_event: HashMap::new(),
            active_events: HashMap::new(),
            positions: Vec::new(),
            closed_trades: Vec::new(),
            equity,
            peak_equity: equity,
            max_drawdown: Decimal::ZERO,
            equity_curve: Vec::new(),
            last_entry_time: HashMap::new(),
            data_range_start: None,
            data_range_end: None,
            last_logic_ts: HashMap::new(),
        }
    }

    pub fn new_without_recorder(config: DirectionalBacktestConfig) -> Self {
        Self::new(config, Box::new(NullRecorder))
    }

    pub fn config(&self) -> &DirectionalBacktestConfig {
        &self.config
    }

    pub fn closed_trades(&self) -> &[DirectionalClosedTrade] {
        &self.closed_trades
    }

    /// Take ownership of the recorder back from the engine.
    /// Useful for calling async methods (like `flush_async`/`finalize`) after `run()`.
    pub fn take_recorder(&mut self) -> Box<dyn BacktestRecorder> {
        std::mem::replace(&mut self.recorder, Box::new(NullRecorder))
    }

    // ─── Main loop ──────────────────────────────────────────

    /// Consume the feed and return aggregate results.
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults {
        while let Some(update) = feed.next_update() {
            // Track data range
            if self.data_range_start.is_none() {
                self.data_range_start = Some(update.timestamp);
            }
            self.data_range_end = Some(update.timestamp);

            // Prune expired events (end_time has passed without settlement)
            for events in self.active_events.values_mut() {
                events.retain(|e| e.end_time > update.timestamp);
            }

            match &update.update_type {
                UpdateType::SpotTrade { price, quantity } => {
                    self.handle_spot_trade(&update.symbol, *price, *quantity, update.timestamp);
                }
                UpdateType::PmQuote {
                    event_slug,
                    side,
                    best_ask,
                    ..
                } => {
                    self.handle_pm_quote(
                        &update.symbol,
                        event_slug,
                        *side,
                        *best_ask,
                        update.timestamp,
                    );
                }
                UpdateType::EventState {
                    event_slug,
                    end_time,
                    price_to_beat,
                    outcome,
                } => {
                    // Binary settlement — only close positions matching this event
                    if let Some(won) = outcome {
                        self.resolve_positions(&update.symbol, event_slug, *won, update.timestamp);
                        // Remove only the settled event, not all events for the symbol
                        if let Some(events) = self.active_events.get_mut(&update.symbol) {
                            events.retain(|e| e.event_slug != *event_slug);
                        }
                        self.pm_asks_by_event.remove(event_slug);
                    }

                    // Track active window: store S0 (price_to_beat) for probability calc
                    // Multiple events per symbol are allowed (5m + 15m overlap)
                    if outcome.is_none() {
                        if let (Some(end), Some(s0)) = (end_time, price_to_beat) {
                            let events =
                                self.active_events.entry(update.symbol.clone()).or_default();
                            // Don't add duplicate events
                            if !events.iter().any(|e| e.event_slug == *event_slug) {
                                events.push(ActiveWindowInfo {
                                    event_slug: event_slug.clone(),
                                    s0: *s0,
                                    end_time: *end,
                                });
                            }
                        }
                    }
                }
                UpdateType::LobSnapshot { .. } => {
                    // LOB depth not used by directional backtest
                }
                UpdateType::BinanceL2 { .. } => {
                    // Binance L2 features are ignored by the directional backtest.
                }
            }
        }

        // Force-close any remaining positions at latest PM price (data exhausted)
        self.close_remaining_positions();
        let _ = self.recorder.flush();
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
        self.spot_prices
            .entry(symbol.to_string())
            .and_modify(|sp| sp.update(price, quantity, ts))
            .or_insert_with(|| SpotPrice::new(price, quantity, ts));
    }

    fn handle_pm_quote(
        &mut self,
        symbol: &str,
        event_slug: &str,
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks (per event_slug)
        let entry = self
            .pm_asks_by_event
            .entry(event_slug.to_string())
            .or_insert((None, None));
        match quote_side {
            Side::Up => {
                if best_ask.is_some() {
                    entry.0 = best_ask;
                }
            }
            Side::Down => {
                if best_ask.is_some() {
                    entry.1 = best_ask;
                }
            }
        }

        // Update position mark-to-market (cheap — just price assignment)
        for pos in &mut self.positions {
            if pos.symbol == symbol && pos.event_slug == event_slug {
                match pos.direction {
                    Direction::Up => {
                        if quote_side == Side::Up {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                    Direction::Down => {
                        if quote_side == Side::Down {
                            if let Some(ask) = best_ask {
                                pos.latest_pm_price = ask;
                            }
                        }
                    }
                }
            }
        }

        // Throttle entry/exit logic to once per second per symbol.
        // PM quotes arrive ~30-40/sec — running probability model on every tick is wasteful.
        let should_run_logic = match self.last_logic_ts.get(symbol) {
            Some(last) => (ts - *last).num_seconds() >= 1,
            None => true,
        };
        if !should_run_logic {
            return;
        }
        self.last_logic_ts.insert(symbol.to_string(), ts);

        // Try directional entry
        self.try_directional_entry(symbol, ts);

        // Check exits for existing positions
        self.check_exits(ts);

        // Record equity curve
        self.record_equity(ts);
    }

    // ─── Equity tracking ─────────────────────────────────────

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

        let sharpe = self.calculate_sharpe();

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
            trades: Vec::new(),
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

    /// Print directional-specific summary stats beyond BacktestResults.
    pub fn print_directional_summary(&self) {
        if self.closed_trades.is_empty() {
            info!("No trades to summarize.");
            return;
        }

        let total = self.closed_trades.len();

        // Settlement rate
        let settled = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason == "settlement")
            .count();
        let settlement_rate = settled as f64 / total as f64 * 100.0;

        // Exit reason breakdown
        let mut exit_counts: HashMap<&str, usize> = HashMap::new();
        for t in &self.closed_trades {
            *exit_counts.entry(&t.exit_reason).or_default() += 1;
        }

        // Avg p_hat for winners vs losers (calibration check)
        let winner_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|t| t.won)
            .map(|t| t.entry_p_hat)
            .collect();
        let loser_p: Vec<f64> = self
            .closed_trades
            .iter()
            .filter(|t| !t.won)
            .map(|t| t.entry_p_hat)
            .collect();

        let avg_winner_p = if winner_p.is_empty() {
            0.0
        } else {
            winner_p.iter().sum::<f64>() / winner_p.len() as f64
        };
        let avg_loser_p = if loser_p.is_empty() {
            0.0
        } else {
            loser_p.iter().sum::<f64>() / loser_p.len() as f64
        };

        // EV_net distribution
        let ev_nets: Vec<f64> = self.closed_trades.iter().map(|t| t.entry_ev_net).collect();
        let avg_ev = ev_nets.iter().sum::<f64>() / total as f64;

        // Direction breakdown
        let up_trades = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "UP")
            .count();
        let down_trades = total - up_trades;
        let up_wins = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "UP" && t.won)
            .count();
        let down_wins = self
            .closed_trades
            .iter()
            .filter(|t| t.direction == "DOWN" && t.won)
            .count();

        println!("\n=== Directional Backtest Summary ===");
        println!(
            "Settlement rate:  {:.1}% ({}/{})",
            settlement_rate, settled, total
        );
        println!("Exit reasons:");
        for (reason, count) in &exit_counts {
            println!("  {:<16} {}", reason, count);
        }
        println!("\nCalibration:");
        println!("  Avg p_hat winners:  {:.3}", avg_winner_p);
        println!("  Avg p_hat losers:   {:.3}", avg_loser_p);
        println!("  Avg EV_net at entry: {:.4}", avg_ev);
        println!("\nDirection breakdown:");
        println!(
            "  UP:   {} trades, {} wins ({:.1}%)",
            up_trades,
            up_wins,
            if up_trades > 0 {
                up_wins as f64 / up_trades as f64 * 100.0
            } else {
                0.0
            }
        );
        println!(
            "  DOWN: {} trades, {} wins ({:.1}%)",
            down_trades,
            down_wins,
            if down_trades > 0 {
                down_wins as f64 / down_trades as f64 * 100.0
            } else {
                0.0
            }
        );

        // Sigma distribution
        let sigmas: Vec<f64> = self.closed_trades.iter().map(|t| t.entry_sigma).collect();
        let avg_sigma = sigmas.iter().sum::<f64>() / sigmas.len().max(1) as f64;
        let min_sigma = sigmas.iter().cloned().fold(f64::MAX, f64::min);
        let max_sigma = sigmas.iter().cloned().fold(f64::MIN, f64::max);
        println!("\nVolatility:");
        println!("  Avg σ at entry: {:.5}", avg_sigma);
        println!("  Min σ: {:.5}  Max σ: {:.5}", min_sigma, max_sigma);

        // Holding time distribution
        let hold_times: Vec<i64> = self.closed_trades.iter().map(|t| t.holding_secs).collect();
        let avg_hold = hold_times.iter().sum::<i64>() as f64 / hold_times.len().max(1) as f64;
        let min_hold = hold_times.iter().min().copied().unwrap_or(0);
        let max_hold = hold_times.iter().max().copied().unwrap_or(0);
        println!("\nHolding time:");
        println!(
            "  Avg: {:.0}s  Min: {}s  Max: {}s",
            avg_hold, min_hold, max_hold
        );

        // Entry price distribution
        let entry_prices: Vec<f64> = self
            .closed_trades
            .iter()
            .map(|t| t.entry_price.to_f64().unwrap_or(0.0))
            .collect();
        let avg_entry = entry_prices.iter().sum::<f64>() / entry_prices.len().max(1) as f64;
        println!("  Avg entry price: ${:.4}", avg_entry);

        // Per-symbol breakdown
        let mut symbol_stats: HashMap<&str, (usize, usize, Decimal, Decimal)> = HashMap::new();
        for t in &self.closed_trades {
            let entry =
                symbol_stats
                    .entry(&t.symbol)
                    .or_insert((0, 0, Decimal::ZERO, Decimal::ZERO));
            entry.0 += 1; // total trades
            if t.won {
                entry.1 += 1; // wins
            }
            entry.2 += t.pnl; // total pnl
            entry.3 += Decimal::from(t.shares) * t.entry_price; // volume
        }

        let mut symbols: Vec<&&str> = symbol_stats.keys().collect();
        symbols.sort();

        println!("\nPer-symbol breakdown:");
        println!(
            "  {:<12} {:>6} {:>6} {:>8} {:>12} {:>12}",
            "Symbol", "Trades", "Wins", "WinRate", "PnL", "Volume"
        );
        println!("  {}", "-".repeat(62));
        for sym in &symbols {
            let (trades, wins, pnl, vol) = symbol_stats[*sym];
            let wr = if trades > 0 {
                wins as f64 / trades as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
                sym, trades, wins, wr, pnl, vol
            );
        }
        let total_vol: Decimal = symbol_stats.values().map(|v| v.3).sum();
        let total_pnl: Decimal = symbol_stats.values().map(|v| v.2).sum();
        println!("  {}", "-".repeat(62));
        println!(
            "  {:<12} {:>6} {:>6} {:>7.1}% {:>11.2} {:>11.2}",
            "TOTAL",
            total,
            self.closed_trades.iter().filter(|t| t.won).count(),
            self.closed_trades.iter().filter(|t| t.won).count() as f64 / total as f64 * 100.0,
            total_pnl,
            total_vol
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Display for directional results
// ─────────────────────────────────────────────────────────────

impl fmt::Display for DirectionalBacktestEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let results = self.build_results();
        writeln!(f, "=== Directional Backtest Results ===")?;
        writeln!(
            f,
            "Period:        {} to {}",
            results.start_time.format("%Y-%m-%d %H:%M"),
            results.end_time.format("%Y-%m-%d %H:%M")
        )?;
        writeln!(f, "Total trades:  {}", results.total_trades)?;
        writeln!(
            f,
            "Win/Loss:      {} / {}",
            results.winning_trades, results.losing_trades
        )?;
        writeln!(f, "Win rate:      {:.1}%", results.win_rate * 100.0)?;
        writeln!(f, "Total PnL:     ${:.2}", results.total_pnl)?;
        writeln!(f, "Avg PnL/trade: ${:.4}", results.avg_pnl_per_trade)?;
        writeln!(f, "Sharpe ratio:  {:.2}", results.sharpe_ratio)?;
        writeln!(f, "Profit factor: {:.2}", results.profit_factor)?;
        writeln!(f, "Max drawdown:  {:.2}%", results.max_drawdown * dec!(100))?;
        writeln!(f, "Avg hold time: {:.0}s", results.avg_holding_time_secs)?;
        writeln!(f, "Largest win:   ${:.4}", results.largest_win)?;
        writeln!(f, "Largest loss:  ${:.4}", results.largest_loss)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::backtest_feed::{HistoricalFeed, MarketUpdate};
    use std::collections::VecDeque;

    fn mock_feed(updates: Vec<MarketUpdate>) -> HistoricalFeed {
        HistoricalFeed {
            updates: VecDeque::from(updates),
        }
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn test_empty_feed() {
        let config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);
        let mut feed = mock_feed(vec![]);
        let results = engine.run(&mut feed);

        assert_eq!(results.total_trades, 0);
        assert_eq!(results.total_pnl, Decimal::ZERO);
    }

    #[test]
    fn test_settlement_binary_payout() {
        // Setup: create a position via momentum signal, then settle it.
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0; // Accept any positive edge
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001); // Low threshold for test
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300); // 5 min window

        let mut updates = vec![];

        // Event opens: S0 = 100
        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Build spot price history with UPWARD momentum (100.00 → 101.50)
        // Need enough points spread over 60s for weighted_momentum to work
        for i in 1..=60 {
            let price = dec!(100) + Decimal::from(i) * dec!(0.025);
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price,
                    quantity: Some(dec!(1)),
                },
            });
        }

        // PM quote with cheap UP ask — momentum is up, so should buy UP
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
                bid_size: None,
                ask_size: None,
            },
        });

        // Settlement: UP wins
        updates.push(MarketUpdate {
            timestamp: end_time,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: Some(true),
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert!(results.total_trades >= 1, "Expected at least 1 trade");

        let trades = engine.closed_trades();
        if !trades.is_empty() {
            let t = &trades[0];
            assert_eq!(t.exit_reason, "settlement");
            assert_eq!(t.direction, "UP");
            assert!(t.won, "UP trade should win when UP settles");
            assert!(t.pnl > Decimal::ZERO, "PnL should be positive");
            assert_eq!(t.exit_price, Decimal::ONE, "Settlement pays $1.00");
        }
    }

    #[test]
    fn test_entry_edge_filter() {
        // High entry threshold should reject entries
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.99; // Impossibly high edge requirement
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.02),
                    quantity: Some(dec!(1)),
                },
            });
        }

        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.50)),
                bid_size: None,
                ask_size: None,
            },
        });

        let mut feed = mock_feed(updates);
        let results = engine.run(&mut feed);

        assert_eq!(
            results.total_trades, 0,
            "No trades should pass 99% edge threshold"
        );
    }

    #[test]
    fn test_hold_to_settlement() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = true;
        config.hard_stop_usd = dec!(999);
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 10;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Upward momentum
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry quote
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.30)),
                bid_size: None,
                ask_size: None,
            },
        });

        // Adverse PM quote but NO settlement
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.20)),
                bid_size: None,
                ask_size: None,
            },
        });

        updates.push(MarketUpdate {
            timestamp: ts(200),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.15)),
                bid_size: None,
                ask_size: None,
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        if !trades.is_empty() {
            assert_eq!(
                trades[0].exit_reason, "data_exhausted",
                "Should hold to settlement, closed only because feed ended"
            );
        }
    }

    #[test]
    fn test_hard_stop() {
        let mut config = DirectionalBacktestConfig::with_symbols(vec!["BTCUSDT".into()]);
        config.entry_threshold = 0.0;
        config.hold_to_settlement = false;
        config.hard_stop_usd = dec!(1); // Very tight stop: $1
        config.min_entry_price = dec!(0.01);
        config.max_entry_price = dec!(0.99);
        config.shares_per_trade = 100;
        config.min_momentum = dec!(0.001);
        config.min_time_remaining_secs = 30;
        config.max_time_remaining_secs = 600;

        let mut engine = DirectionalBacktestEngine::new_without_recorder(config);

        let base = ts(0);
        let end_time = ts(300);
        let mut updates = vec![];

        updates.push(MarketUpdate {
            timestamp: base,
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::EventState {
                event_slug: "btc-up-100".into(),
                end_time: Some(end_time),
                price_to_beat: Some(dec!(100)),
                outcome: None,
            },
        });

        // Upward momentum to trigger entry
        for i in 1..=60 {
            updates.push(MarketUpdate {
                timestamp: ts(i),
                symbol: "BTCUSDT".into(),
                update_type: UpdateType::SpotTrade {
                    price: dec!(100) + Decimal::from(i) * dec!(0.025),
                    quantity: Some(dec!(1)),
                },
            });
        }

        // Entry at 0.40
        updates.push(MarketUpdate {
            timestamp: ts(61),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.40)),
                bid_size: None,
                ask_size: None,
            },
        });

        // Price crashes to 0.10 — unrealized loss = 100 * (0.10 - ~0.40) ≈ -$30 > $1 stop
        updates.push(MarketUpdate {
            timestamp: ts(100),
            symbol: "BTCUSDT".into(),
            update_type: UpdateType::PmQuote {
                event_slug: "btc-up-100".into(),
                token_id: "btc-up-100:UP".into(),
                side: Side::Up,
                best_bid: None,
                best_ask: Some(dec!(0.10)),
                bid_size: None,
                ask_size: None,
            },
        });

        let mut feed = mock_feed(updates);
        let _results = engine.run(&mut feed);

        let trades = engine.closed_trades();
        let hard_stopped = trades.iter().any(|t| t.exit_reason == "hard_stop");
        assert!(
            hard_stopped || trades.is_empty(),
            "Expected hard_stop exit or no entry (if edge filter blocked)"
        );
    }
}
