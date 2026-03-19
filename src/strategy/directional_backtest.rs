//! Directional backtest engine for momentum-driven binary option trading.
//!
//! Uses weighted momentum (10s/30s/60s) -> fair value estimation -> edge filtering
//! to enter positions, mirroring the live MomentumDetector logic. Holds to
//! settlement by default (binary options settle at $1.00 or $0.00).
//!
//! Binance spot price serves as Chainlink proxy (>99.9% correlation on 5m/15m).
//!
//! Usage:
//!   ploy strategy backtest directional --symbols BTCUSDT --save --json

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::adapters::SpotPrice;
use crate::strategy::backtest_recorder::{BacktestRecorder, NullRecorder};
use crate::strategy::execution_sim::ExecutionSimulator;
use crate::strategy::fee_model::FeeModel;
use crate::strategy::momentum::Direction;

mod entry_lifecycle;
mod position_lifecycle;
mod reporting;
mod runtime_flow;

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
    /// Hold winners to settlement (default true - let them run)
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
            min_momentum: dec!(0.003),
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
    pub entry_p_hat: f64,
    pub entry_ev_net: f64,
    pub s0: Decimal,
    pub entry_sigma: f64,
}

#[derive(Debug, Clone)]
struct ActiveWindowInfo {
    event_slug: String,
    /// S0 = price_to_beat from EventState
    s0: Decimal,
    end_time: DateTime<Utc>,
}

pub struct DirectionalBacktestEngine {
    config: DirectionalBacktestConfig,
    fee_model: FeeModel,
    execution_sim: ExecutionSimulator,
    recorder: Box<dyn BacktestRecorder>,
    spot_prices: HashMap<String, SpotPrice>,
    pm_asks_by_event: HashMap<String, (Option<Decimal>, Option<Decimal>)>,
    active_events: HashMap<String, Vec<ActiveWindowInfo>>,
    positions: Vec<DirectionalPosition>,
    closed_trades: Vec<DirectionalClosedTrade>,
    equity: Decimal,
    peak_equity: Decimal,
    max_drawdown: Decimal,
    equity_curve: Vec<(DateTime<Utc>, Decimal)>,
    last_entry_time: HashMap<String, DateTime<Utc>>,
    data_range_start: Option<DateTime<Utc>>,
    data_range_end: Option<DateTime<Utc>>,
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
}
