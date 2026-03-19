//! Momentum strategy backtest engine.
//!
//! Reuses the live `MomentumDetector.check()` and `SpotPrice` types to ensure
//! the backtest signal logic is identical to production. "Change strategy once,
//! live + backtest automatically agree."
//!
//! Usage:
//!   ploy strategy backtest momentum --symbols BTCUSDT --save --json

use std::collections::HashMap;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::adapters::SpotPrice;
use crate::domain::Side;
use crate::strategy::backtest::BacktestResults;
use crate::strategy::backtest_feed::{MarketFeed, UpdateType};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::momentum::{Direction, MomentumConfig, MomentumDetector};

mod lifecycle;
mod results;

use self::lifecycle::{BacktestClosedTrade, BacktestPosition};
pub use self::results::save_backtest_results;

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
                UpdateType::PmQuote { side, best_ask, .. } => {
                    self.handle_pm_quote(&update.symbol, *side, *best_ask, update.timestamp);
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
                UpdateType::BinanceL2 { .. } => {
                    // Binance L2 features are ignored by momentum backtest.
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
        quote_side: Side,
        best_ask: Option<Decimal>,
        ts: DateTime<Utc>,
    ) {
        // Update latest asks
        let entry = self
            .pm_asks
            .entry(symbol.to_string())
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

        // Update position mark-to-market
        for pos in &mut self.positions {
            if pos.symbol == symbol {
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

}
// Tests live in results.rs submodule
