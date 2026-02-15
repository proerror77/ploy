//! Backtest Environment for RL Training with Historical Data
//!
//! Replays historical tick data for realistic backtesting.

use super::trading::{EnvAction, TradingEnvConfig};
use crate::domain::Tick;
use rust_decimal::prelude::ToPrimitive;

/// Historical tick data for backtesting
#[derive(Debug, Clone)]
pub struct HistoricalData {
    /// UP token ticks
    pub up_ticks: Vec<TickData>,
    /// DOWN token ticks
    pub down_ticks: Vec<TickData>,
    /// Round slug/identifier
    pub round_slug: String,
}

/// Simplified tick data for backtesting
#[derive(Debug, Clone)]
pub struct TickData {
    /// Timestamp in milliseconds
    pub timestamp_ms: i64,
    /// Best bid price
    pub bid: f64,
    /// Best ask price
    pub ask: f64,
}

impl TickData {
    /// Create from domain Tick
    pub fn from_tick(tick: &Tick) -> Self {
        Self {
            timestamp_ms: tick.timestamp.timestamp_millis(),
            bid: tick.best_bid.and_then(|d| d.to_f64()).unwrap_or(0.5),
            ask: tick.best_ask.and_then(|d| d.to_f64()).unwrap_or(0.5),
        }
    }
}

/// Market state at a point in time during backtest
#[derive(Debug, Clone)]
pub struct BacktestMarketState {
    /// Current timestamp
    pub timestamp_ms: i64,
    /// UP token bid
    pub up_bid: f64,
    /// UP token ask
    pub up_ask: f64,
    /// DOWN token bid
    pub down_bid: f64,
    /// DOWN token ask
    pub down_ask: f64,
    /// Sum of asks
    pub sum_of_asks: f64,
    /// Implied spot price (UP ask)
    pub spot_price: f64,
    /// Price history
    pub price_history: Vec<f64>,
}

impl BacktestMarketState {
    /// Get momentum over n steps
    pub fn momentum(&self, n: usize) -> Option<f64> {
        if self.price_history.len() < n + 1 {
            return None;
        }
        let current = self.price_history.last()?;
        let past = self.price_history.get(self.price_history.len() - n - 1)?;
        Some(current - past)
    }
}

/// Position during backtest
#[derive(Debug, Clone, Default)]
struct BacktestPosition {
    is_up: bool,
    shares: u64,
    entry_price: f64,
    entry_time: i64,
}

impl BacktestPosition {
    fn is_empty(&self) -> bool {
        self.shares == 0
    }

    fn current_value(&self, state: &BacktestMarketState) -> f64 {
        if self.shares == 0 {
            return 0.0;
        }
        let price = if self.is_up {
            state.up_bid
        } else {
            state.down_bid
        };
        self.shares as f64 * price
    }

    fn unrealized_pnl(&self, state: &BacktestMarketState) -> f64 {
        if self.shares == 0 {
            return 0.0;
        }
        let price = if self.is_up {
            state.up_bid
        } else {
            state.down_bid
        };
        self.shares as f64 * (price - self.entry_price)
    }
}

/// Step result from backtest environment
#[derive(Debug, Clone)]
pub struct BacktestStepResult {
    pub observation: Vec<f32>,
    pub reward: f32,
    pub done: bool,
    pub info: BacktestInfo,
}

/// Additional backtest info
#[derive(Debug, Clone, Default)]
pub struct BacktestInfo {
    pub timestamp_ms: i64,
    pub capital: f64,
    pub position_value: f64,
    pub total_pnl: f64,
    pub num_trades: usize,
    pub win_rate: f64,
}

/// Backtest environment that replays historical data
pub struct BacktestEnvironment {
    /// Historical data
    data: HistoricalData,
    /// Current tick index
    tick_idx: usize,
    /// Merged timeline of ticks
    timeline: Vec<(i64, bool, TickData)>, // (timestamp, is_up, tick)
    /// Current market state
    state: BacktestMarketState,
    /// Current position
    position: BacktestPosition,
    /// Capital
    capital: f64,
    initial_capital: f64,
    /// Statistics
    episode_pnl: f64,
    num_trades: usize,
    winning_trades: usize,
    /// Configuration
    config: TradingEnvConfig,
}

impl BacktestEnvironment {
    /// Create new backtest environment from historical data
    pub fn new(data: HistoricalData, config: TradingEnvConfig) -> Self {
        // Merge UP and DOWN ticks into timeline
        let mut timeline: Vec<(i64, bool, TickData)> = Vec::new();

        for tick in &data.up_ticks {
            timeline.push((tick.timestamp_ms, true, tick.clone()));
        }
        for tick in &data.down_ticks {
            timeline.push((tick.timestamp_ms, false, tick.clone()));
        }

        // Sort by timestamp
        timeline.sort_by_key(|(ts, _, _)| *ts);

        let initial_capital = config.initial_capital;

        let mut env = Self {
            data,
            tick_idx: 0,
            timeline,
            state: BacktestMarketState {
                timestamp_ms: 0,
                up_bid: 0.5,
                up_ask: 0.5,
                down_bid: 0.5,
                down_ask: 0.5,
                sum_of_asks: 1.0,
                spot_price: 0.5,
                price_history: vec![0.5],
            },
            position: BacktestPosition::default(),
            capital: initial_capital,
            initial_capital,
            episode_pnl: 0.0,
            num_trades: 0,
            winning_trades: 0,
            config,
        };

        env.initialize_state();
        env
    }

    /// Initialize market state from first ticks
    fn initialize_state(&mut self) {
        if self.timeline.is_empty() {
            return;
        }

        // Find initial UP and DOWN prices
        let mut up_found = false;
        let mut down_found = false;

        for (ts, is_up, tick) in &self.timeline {
            if *is_up && !up_found {
                self.state.up_bid = tick.bid;
                self.state.up_ask = tick.ask;
                up_found = true;
            } else if !is_up && !down_found {
                self.state.down_bid = tick.bid;
                self.state.down_ask = tick.ask;
                down_found = true;
            }

            if up_found && down_found {
                self.state.timestamp_ms = *ts;
                break;
            }
        }

        self.update_derived_state();
    }

    fn update_derived_state(&mut self) {
        self.state.sum_of_asks = self.state.up_ask + self.state.down_ask;
        self.state.spot_price = self.state.up_ask;

        // Update price history
        self.state.price_history.push(self.state.spot_price);
        if self.state.price_history.len() > 60 {
            self.state.price_history.remove(0);
        }
    }

    /// Reset environment
    pub fn reset(&mut self) -> Vec<f32> {
        self.tick_idx = 0;
        self.position = BacktestPosition::default();
        self.capital = self.initial_capital;
        self.episode_pnl = 0.0;
        self.num_trades = 0;
        self.winning_trades = 0;
        self.state.price_history = vec![0.5];

        self.initialize_state();
        self.get_observation()
    }

    /// Step environment with action
    pub fn step(&mut self, action: EnvAction) -> BacktestStepResult {
        // Execute action
        let trade_pnl = self.execute_action(action);

        // Advance to next tick
        self.advance_tick();

        // Calculate reward
        let reward = self.calculate_reward(trade_pnl);

        // Check if done
        let done = self.tick_idx >= self.timeline.len();

        let info = BacktestInfo {
            timestamp_ms: self.state.timestamp_ms,
            capital: self.capital,
            position_value: self.position.current_value(&self.state),
            total_pnl: self.episode_pnl,
            num_trades: self.num_trades,
            win_rate: if self.num_trades > 0 {
                self.winning_trades as f64 / self.num_trades as f64
            } else {
                0.0
            },
        };

        BacktestStepResult {
            observation: self.get_observation(),
            reward,
            done,
            info,
        }
    }

    fn advance_tick(&mut self) {
        if self.tick_idx >= self.timeline.len() {
            return;
        }

        let (ts, is_up, tick) = &self.timeline[self.tick_idx];

        if *is_up {
            self.state.up_bid = tick.bid;
            self.state.up_ask = tick.ask;
        } else {
            self.state.down_bid = tick.bid;
            self.state.down_ask = tick.ask;
        }

        self.state.timestamp_ms = *ts;
        self.update_derived_state();
        self.tick_idx += 1;
    }

    fn execute_action(&mut self, action: EnvAction) -> f64 {
        match action {
            EnvAction::Hold => 0.0,

            EnvAction::BuyUp => {
                if !self.position.is_empty() {
                    return 0.0;
                }

                let price = self.state.up_ask;
                let max_shares = (self.capital / price) as u64;
                let shares = max_shares.min(self.config.max_position);

                if shares == 0 {
                    return 0.0;
                }

                let cost = shares as f64 * price;
                let fee = cost * self.config.transaction_cost;

                self.capital -= cost + fee;
                self.position = BacktestPosition {
                    is_up: true,
                    shares,
                    entry_price: price,
                    entry_time: self.state.timestamp_ms,
                };

                -fee
            }

            EnvAction::BuyDown => {
                if !self.position.is_empty() {
                    return 0.0;
                }

                let price = self.state.down_ask;
                let max_shares = (self.capital / price) as u64;
                let shares = max_shares.min(self.config.max_position);

                if shares == 0 {
                    return 0.0;
                }

                let cost = shares as f64 * price;
                let fee = cost * self.config.transaction_cost;

                self.capital -= cost + fee;
                self.position = BacktestPosition {
                    is_up: false,
                    shares,
                    entry_price: price,
                    entry_time: self.state.timestamp_ms,
                };

                -fee
            }

            EnvAction::Sell => {
                if self.position.is_empty() {
                    return 0.0;
                }

                let price = if self.position.is_up {
                    self.state.up_bid
                } else {
                    self.state.down_bid
                };

                let proceeds = self.position.shares as f64 * price;
                let fee = proceeds * self.config.transaction_cost;
                let pnl =
                    proceeds - fee - (self.position.shares as f64 * self.position.entry_price);

                self.capital += proceeds - fee;
                self.episode_pnl += pnl;
                self.num_trades += 1;

                if pnl > 0.0 {
                    self.winning_trades += 1;
                }

                self.position = BacktestPosition::default();
                pnl
            }
        }
    }

    fn calculate_reward(&self, trade_pnl: f64) -> f32 {
        let mut reward = trade_pnl as f32;

        // Unrealized PnL component
        if !self.position.is_empty() {
            let unrealized = self.position.unrealized_pnl(&self.state);
            reward += unrealized as f32 * 0.1;
        }

        // Timing bonus for good entries
        if self.state.sum_of_asks < 0.96 {
            reward += 0.01;
        }

        reward
    }

    fn get_observation(&self) -> Vec<f32> {
        let mut obs = Vec::with_capacity(42);

        // Price features (20)
        obs.push(self.state.spot_price as f32);

        // Price history
        let history = &self.state.price_history;
        for i in 0..15 {
            if i < history.len() {
                obs.push(history[history.len() - 1 - i] as f32);
            } else {
                obs.push(self.state.spot_price as f32);
            }
        }

        // Momentum
        obs.push(self.state.momentum(1).unwrap_or(0.0) as f32);
        obs.push(self.state.momentum(5).unwrap_or(0.0) as f32);
        obs.push(self.state.momentum(15).unwrap_or(0.0) as f32);
        obs.push(self.state.momentum(30).unwrap_or(0.0) as f32);

        // Quote features (8)
        obs.push(self.state.up_bid as f32);
        obs.push(self.state.up_ask as f32);
        obs.push(self.state.down_bid as f32);
        obs.push(self.state.down_ask as f32);
        obs.push((self.state.up_ask - self.state.up_bid) as f32);
        obs.push((self.state.down_ask - self.state.down_bid) as f32);
        obs.push(self.state.sum_of_asks as f32);
        obs.push(1.0); // Liquidity placeholder

        // Position features (6)
        obs.push(if self.position.is_empty() { 0.0 } else { 1.0 });
        obs.push(if self.position.is_up { 1.0 } else { -1.0 });
        obs.push(self.position.shares as f32 / self.config.max_position as f32);
        obs.push(self.position.entry_price as f32);
        obs.push(self.position.unrealized_pnl(&self.state) as f32);
        let duration = if self.position.entry_time > 0 {
            (self.state.timestamp_ms - self.position.entry_time) as f32 / 60000.0
        } else {
            0.0
        };
        obs.push(duration);

        // Risk features (4)
        let total_value = self.capital + self.position.current_value(&self.state);
        let exposure = self.position.current_value(&self.state) / total_value;
        obs.push(exposure as f32);
        obs.push((total_value / self.initial_capital - 1.0) as f32);
        obs.push(self.episode_pnl as f32 / self.initial_capital as f32);
        obs.push(0.0);

        // Time features (4)
        obs.push(0.0);
        obs.push(0.0);
        obs.push(0.0);
        obs.push(0.0);

        obs
    }

    /// Get number of remaining ticks
    pub fn remaining_ticks(&self) -> usize {
        self.timeline.len().saturating_sub(self.tick_idx)
    }

    /// Get total ticks
    pub fn total_ticks(&self) -> usize {
        self.timeline.len()
    }

    /// Get round info
    pub fn round_slug(&self) -> &str {
        &self.data.round_slug
    }

    /// Get final stats
    pub fn final_stats(&self) -> BacktestInfo {
        BacktestInfo {
            timestamp_ms: self.state.timestamp_ms,
            capital: self.capital,
            position_value: self.position.current_value(&self.state),
            total_pnl: self.episode_pnl,
            num_trades: self.num_trades,
            win_rate: if self.num_trades > 0 {
                self.winning_trades as f64 / self.num_trades as f64
            } else {
                0.0
            },
        }
    }
}

/// Generate sample historical data for testing (simulates realistic market)
pub fn generate_sample_data(duration_mins: u64, volatility: f64) -> HistoricalData {
    use rand::Rng;

    let mut rng = rand::thread_rng();
    let mut up_ticks = Vec::new();
    let mut down_ticks = Vec::new();

    let num_ticks = duration_mins * 60; // One tick per second
    let start_ts = chrono::Utc::now().timestamp_millis() - (duration_mins as i64 * 60 * 1000);

    let mut up_price = 0.50;
    let mut down_price = 0.50;

    for i in 0..num_ticks {
        let ts = start_ts + (i as i64 * 1000);

        // Random walk with mean reversion
        let up_change = rng.gen_range(-volatility..volatility) + 0.001 * (0.5 - up_price);
        up_price = (up_price + up_change).clamp(0.10, 0.90);

        // DOWN should roughly mirror UP
        down_price = (1.0 - up_price + rng.gen_range(-0.02..0.02)).clamp(0.10, 0.90);

        let spread = 0.01 + rng.gen_range(0.0..0.02);

        up_ticks.push(TickData {
            timestamp_ms: ts,
            bid: (up_price - spread / 2.0).max(0.01),
            ask: (up_price + spread / 2.0).min(0.99),
        });

        down_ticks.push(TickData {
            timestamp_ms: ts + 100, // Slight offset
            bid: (down_price - spread / 2.0).max(0.01),
            ask: (down_price + spread / 2.0).min(0.99),
        });
    }

    HistoricalData {
        up_ticks,
        down_ticks,
        round_slug: format!("sample-{}", duration_mins),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_data_generation() {
        let data = generate_sample_data(5, 0.02);
        assert!(!data.up_ticks.is_empty());
        assert!(!data.down_ticks.is_empty());
    }

    #[test]
    fn test_backtest_env_creation() {
        let data = generate_sample_data(5, 0.02);
        let config = TradingEnvConfig::default();
        let env = BacktestEnvironment::new(data, config);

        assert!(env.total_ticks() > 0);
    }

    #[test]
    fn test_backtest_step() {
        let data = generate_sample_data(5, 0.02);
        let config = TradingEnvConfig::default();
        let mut env = BacktestEnvironment::new(data, config);

        let obs = env.reset();
        assert_eq!(obs.len(), 42);

        let result = env.step(EnvAction::Hold);
        assert!(!result.done);
    }

    #[test]
    fn test_backtest_trading() {
        let data = generate_sample_data(5, 0.02);
        let config = TradingEnvConfig::default();
        let mut env = BacktestEnvironment::new(data, config);

        env.reset();

        // Buy UP
        let result = env.step(EnvAction::BuyUp);
        assert!(!result.done);

        // Hold
        for _ in 0..10 {
            env.step(EnvAction::Hold);
        }

        // Sell
        let result = env.step(EnvAction::Sell);
        assert_eq!(env.final_stats().num_trades, 1);
    }
}
