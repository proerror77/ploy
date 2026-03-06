use chrono::{DateTime, Utc};
use ploy_backtest::{
    strategies::{build_momentum_results, MomentumClosedTrade},
    BacktestResults, ExecutionSimulator, MarketFeed, UpdateType,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::debug;

use crate::adapters::SpotPrice;
use crate::strategy::momentum::{Direction, MomentumDetector, MomentumSignal};

use super::MomentumBacktestConfig;

#[derive(Debug, Clone)]
struct BacktestPosition {
    symbol: String,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    shares: u64,
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
    closed_trades: Vec<MomentumClosedTrade>,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
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
                UpdateType::LobSnapshot { .. } => {
                    // LOB depth not used by momentum backtest
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

        self.closed_trades.push(MomentumClosedTrade {
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
        build_momentum_results(
            &self.closed_trades,
            &self.equity_curve,
            self.max_drawdown,
            self.data_range_start,
            self.data_range_end,
        )
    }
}
