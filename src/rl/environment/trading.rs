//! Trading Environment for RL Training
//!
//! Provides a gym-like interface with step/reset for RL agent training.

use super::market::{MarketConfig, MarketState, SimulatedMarket};
use crate::rl::core::action::DiscreteAction;

/// Trading environment configuration
#[derive(Debug, Clone)]
pub struct TradingEnvConfig {
    /// Market simulation config
    pub market: MarketConfig,
    /// Initial capital
    pub initial_capital: f64,
    /// Maximum position size (shares)
    pub max_position: u64,
    /// Transaction cost (percentage)
    pub transaction_cost: f64,
    /// Maximum steps per episode
    pub max_steps: usize,
    /// Take profit threshold
    pub take_profit: f64,
    /// Stop loss threshold
    pub stop_loss: f64,
}

impl Default for TradingEnvConfig {
    fn default() -> Self {
        Self {
            market: MarketConfig::default(),
            initial_capital: 1000.0,
            max_position: 100,
            transaction_cost: 0.001,
            max_steps: 1000,
            take_profit: 0.05,
            stop_loss: 0.03,
        }
    }
}

/// Action that can be taken in the environment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvAction {
    /// Do nothing
    Hold,
    /// Buy UP token
    BuyUp,
    /// Buy DOWN token
    BuyDown,
    /// Sell current position
    Sell,
}

impl From<DiscreteAction> for EnvAction {
    fn from(action: DiscreteAction) -> Self {
        match action {
            DiscreteAction::Hold => EnvAction::Hold,
            DiscreteAction::BuyUp => EnvAction::BuyUp,
            DiscreteAction::BuyDown => EnvAction::BuyDown,
            DiscreteAction::SellPosition => EnvAction::Sell,
            DiscreteAction::EnterHedge => EnvAction::Hold, // Not supported in sim
        }
    }
}

impl From<usize> for EnvAction {
    fn from(action: usize) -> Self {
        match action {
            0 => EnvAction::Hold,
            1 => EnvAction::BuyUp,
            2 => EnvAction::BuyDown,
            3 => EnvAction::Sell,
            _ => EnvAction::Hold,
        }
    }
}

/// Position in the simulated environment
#[derive(Debug, Clone, Default)]
struct Position {
    /// True if holding UP, false if holding DOWN
    is_up: bool,
    /// Number of shares held
    shares: u64,
    /// Entry price
    entry_price: f64,
    /// Steps held
    duration: usize,
}

impl Position {
    fn is_empty(&self) -> bool {
        self.shares == 0
    }

    fn current_value(&self, market: &MarketState) -> f64 {
        if self.shares == 0 {
            return 0.0;
        }
        let current_price = if self.is_up {
            market.up_bid
        } else {
            market.down_bid
        };
        self.shares as f64 * current_price
    }

    fn unrealized_pnl(&self, market: &MarketState) -> f64 {
        if self.shares == 0 {
            return 0.0;
        }
        let current_price = if self.is_up {
            market.up_bid
        } else {
            market.down_bid
        };
        self.shares as f64 * (current_price - self.entry_price)
    }

    fn unrealized_pnl_pct(&self, market: &MarketState) -> f64 {
        if self.shares == 0 || self.entry_price == 0.0 {
            return 0.0;
        }
        let current_price = if self.is_up {
            market.up_bid
        } else {
            market.down_bid
        };
        (current_price - self.entry_price) / self.entry_price
    }
}

/// Result of taking a step in the environment
#[derive(Debug, Clone)]
pub struct StepResult {
    /// New observation after action
    pub observation: Vec<f32>,
    /// Reward signal
    pub reward: f32,
    /// Whether episode is done
    pub done: bool,
    /// Whether episode was truncated (max steps)
    pub truncated: bool,
    /// Additional info
    pub info: StepInfo,
}

/// Additional step information
#[derive(Debug, Clone, Default)]
pub struct StepInfo {
    /// Current capital
    pub capital: f64,
    /// Current position value
    pub position_value: f64,
    /// Total PnL this episode
    pub total_pnl: f64,
    /// Number of trades executed
    pub num_trades: usize,
    /// Win rate (if any trades)
    pub win_rate: f64,
}

/// Trading environment for RL training
pub struct TradingEnvironment {
    config: TradingEnvConfig,
    market: SimulatedMarket,
    position: Position,
    capital: f64,
    initial_capital: f64,
    step_count: usize,
    episode_pnl: f64,
    num_trades: usize,
    winning_trades: usize,
    last_action: EnvAction,
}

impl TradingEnvironment {
    /// Create a new trading environment
    pub fn new(config: TradingEnvConfig) -> Self {
        let market = SimulatedMarket::new(config.market.clone());
        let initial_capital = config.initial_capital;

        Self {
            config,
            market,
            position: Position::default(),
            capital: initial_capital,
            initial_capital,
            step_count: 0,
            episode_pnl: 0.0,
            num_trades: 0,
            winning_trades: 0,
            last_action: EnvAction::Hold,
        }
    }

    /// Reset the environment for a new episode
    pub fn reset(&mut self) -> Vec<f32> {
        self.market.reset();
        self.position = Position::default();
        self.capital = self.initial_capital;
        self.step_count = 0;
        self.episode_pnl = 0.0;
        self.num_trades = 0;
        self.winning_trades = 0;
        self.last_action = EnvAction::Hold;

        self.get_observation()
    }

    /// Take a step in the environment
    pub fn step(&mut self, action: EnvAction) -> StepResult {
        self.step_count += 1;
        self.last_action = action;

        // Execute action
        let trade_pnl = self.execute_action(action);

        // Update position duration
        if !self.position.is_empty() {
            self.position.duration += 1;
        }

        // Step the market
        self.market.step();

        // Calculate reward
        let reward = self.calculate_reward(trade_pnl);

        // Check termination conditions
        let (done, truncated) = self.check_done();

        // Get new observation
        let observation = self.get_observation();

        // Build info
        let info = StepInfo {
            capital: self.capital,
            position_value: self.position.current_value(self.market.state()),
            total_pnl: self.episode_pnl,
            num_trades: self.num_trades,
            win_rate: if self.num_trades > 0 {
                self.winning_trades as f64 / self.num_trades as f64
            } else {
                0.0
            },
        };

        StepResult {
            observation,
            reward,
            done,
            truncated,
            info,
        }
    }

    /// Execute trading action
    fn execute_action(&mut self, action: EnvAction) -> f64 {
        let market_state = self.market.state();

        match action {
            EnvAction::Hold => 0.0,

            EnvAction::BuyUp => {
                if !self.position.is_empty() {
                    return 0.0; // Already have position
                }

                let price = market_state.up_ask;
                let max_shares = (self.capital / price) as u64;
                let shares = max_shares.min(self.config.max_position);

                if shares == 0 {
                    return 0.0;
                }

                let cost = shares as f64 * price;
                let fee = cost * self.config.transaction_cost;

                self.capital -= cost + fee;
                self.position = Position {
                    is_up: true,
                    shares,
                    entry_price: price,
                    duration: 0,
                };

                -fee // Cost of transaction
            }

            EnvAction::BuyDown => {
                if !self.position.is_empty() {
                    return 0.0; // Already have position
                }

                let price = market_state.down_ask;
                let max_shares = (self.capital / price) as u64;
                let shares = max_shares.min(self.config.max_position);

                if shares == 0 {
                    return 0.0;
                }

                let cost = shares as f64 * price;
                let fee = cost * self.config.transaction_cost;

                self.capital -= cost + fee;
                self.position = Position {
                    is_up: false,
                    shares,
                    entry_price: price,
                    duration: 0,
                };

                -fee
            }

            EnvAction::Sell => {
                if self.position.is_empty() {
                    return 0.0; // No position to sell
                }

                let price = if self.position.is_up {
                    market_state.up_bid
                } else {
                    market_state.down_bid
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

                self.position = Position::default();

                pnl
            }
        }
    }

    /// Calculate reward for the current step
    fn calculate_reward(&self, trade_pnl: f64) -> f32 {
        let market_state = self.market.state();

        // Base reward: realized PnL
        let mut reward = trade_pnl as f32;

        // Unrealized PnL component (scaled down)
        if !self.position.is_empty() {
            let unrealized = self.position.unrealized_pnl(market_state);
            reward += unrealized as f32 * 0.1;
        }

        // Timing bonus: reward for entering when sum_of_asks < 0.96
        if self.last_action == EnvAction::BuyUp || self.last_action == EnvAction::BuyDown {
            if market_state.sum_of_asks < 0.96 {
                reward += 0.01;
            }
        }

        // Holding penalty (small)
        if !self.position.is_empty() && self.position.duration > 50 {
            reward -= 0.001;
        }

        reward
    }

    /// Check if episode is done
    fn check_done(&self) -> (bool, bool) {
        // Truncated by max steps
        if self.step_count >= self.config.max_steps {
            return (true, true);
        }

        // Done by bankruptcy
        let total_value = self.capital + self.position.current_value(self.market.state());
        if total_value < self.initial_capital * 0.1 {
            return (true, false);
        }

        // Check stop loss / take profit if in position
        if !self.position.is_empty() {
            let pnl_pct = self.position.unrealized_pnl_pct(self.market.state());

            // Stop loss
            if pnl_pct < -self.config.stop_loss {
                return (true, false);
            }
        }

        (false, false)
    }

    /// Get current observation as feature vector
    pub fn get_observation(&self) -> Vec<f32> {
        let market_state = self.market.state();

        let mut obs = Vec::with_capacity(42);

        // Price features (20)
        obs.push(market_state.spot_price as f32);

        // Price history (last 15, padded)
        let history = &market_state.price_history;
        for i in 0..15 {
            if i < history.len() {
                obs.push(history[history.len() - 1 - i] as f32);
            } else {
                obs.push(market_state.spot_price as f32);
            }
        }

        // Momentum features
        obs.push(market_state.momentum(1).unwrap_or(0.0) as f32);
        obs.push(market_state.momentum(5).unwrap_or(0.0) as f32);
        obs.push(market_state.momentum(15).unwrap_or(0.0) as f32);
        obs.push(market_state.momentum(30).unwrap_or(0.0) as f32);

        // Quote features (8)
        obs.push(market_state.up_bid as f32);
        obs.push(market_state.up_ask as f32);
        obs.push(market_state.down_bid as f32);
        obs.push(market_state.down_ask as f32);
        obs.push(market_state.up_spread() as f32);
        obs.push(market_state.down_spread() as f32);
        obs.push(market_state.sum_of_asks as f32);
        obs.push(1.0); // Liquidity placeholder

        // Position features (6)
        obs.push(if self.position.is_empty() { 0.0 } else { 1.0 });
        obs.push(if self.position.is_up { 1.0 } else { -1.0 });
        obs.push(self.position.shares as f32 / self.config.max_position as f32);
        obs.push(self.position.entry_price as f32);
        obs.push(self.position.unrealized_pnl(market_state) as f32);
        obs.push(self.position.duration as f32 / 100.0);

        // Risk features (4)
        let total_value = self.capital + self.position.current_value(market_state);
        let exposure = self.position.current_value(market_state) / total_value;
        obs.push(exposure as f32);
        obs.push((total_value / self.initial_capital - 1.0) as f32); // Return
        obs.push(self.episode_pnl as f32 / self.initial_capital as f32);
        obs.push(0.0); // Consecutive failures placeholder

        // Time features (4) - cyclical encoding
        let progress = self.step_count as f32 / self.config.max_steps as f32;
        obs.push((progress * std::f32::consts::TAU).sin());
        obs.push((progress * std::f32::consts::TAU).cos());
        obs.push(0.0); // Day encoding placeholder
        obs.push(0.0); // Day encoding placeholder

        obs
    }

    /// Get observation dimension
    pub fn observation_dim(&self) -> usize {
        42
    }

    /// Get action dimension
    pub fn action_dim(&self) -> usize {
        4
    }

    /// Get current capital
    pub fn capital(&self) -> f64 {
        self.capital
    }

    /// Get episode PnL
    pub fn episode_pnl(&self) -> f64 {
        self.episode_pnl
    }

    /// Get step count
    pub fn step_count(&self) -> usize {
        self.step_count
    }

    /// Get number of trades
    pub fn num_trades(&self) -> usize {
        self.num_trades
    }

    /// Get win rate
    pub fn win_rate(&self) -> f64 {
        if self.num_trades > 0 {
            self.winning_trades as f64 / self.num_trades as f64
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_creation() {
        let config = TradingEnvConfig::default();
        let env = TradingEnvironment::new(config);

        assert_eq!(env.observation_dim(), 42);
        assert_eq!(env.action_dim(), 4);
    }

    #[test]
    fn test_env_reset() {
        let config = TradingEnvConfig::default();
        let mut env = TradingEnvironment::new(config);

        let obs = env.reset();
        assert_eq!(obs.len(), 42);
        assert_eq!(env.step_count(), 0);
        assert_eq!(env.episode_pnl(), 0.0);
    }

    #[test]
    fn test_env_step_hold() {
        let config = TradingEnvConfig::default();
        let mut env = TradingEnvironment::new(config);

        env.reset();
        let result = env.step(EnvAction::Hold);

        assert!(!result.done);
        assert_eq!(result.observation.len(), 42);
        assert_eq!(env.step_count(), 1);
    }

    #[test]
    fn test_env_buy_sell_cycle() {
        let config = TradingEnvConfig::default();
        let mut env = TradingEnvironment::new(config);

        env.reset();

        // Buy UP
        let result = env.step(EnvAction::BuyUp);
        assert!(!result.done);
        assert!(env.capital() < 1000.0); // Spent money

        // Hold a few steps
        for _ in 0..5 {
            env.step(EnvAction::Hold);
        }

        // Sell
        let result = env.step(EnvAction::Sell);
        assert!(!result.done);
        assert_eq!(env.num_trades(), 1);
    }

    #[test]
    fn test_env_episode_completion() {
        let config = TradingEnvConfig {
            max_steps: 10,
            ..Default::default()
        };
        let mut env = TradingEnvironment::new(config);

        env.reset();

        let mut done = false;
        for _ in 0..15 {
            let result = env.step(EnvAction::Hold);
            if result.done {
                done = true;
                assert!(result.truncated); // Should be truncated by max_steps
                break;
            }
        }

        assert!(done);
    }

    #[test]
    fn test_action_conversion() {
        assert_eq!(EnvAction::from(0), EnvAction::Hold);
        assert_eq!(EnvAction::from(1), EnvAction::BuyUp);
        assert_eq!(EnvAction::from(2), EnvAction::BuyDown);
        assert_eq!(EnvAction::from(3), EnvAction::Sell);
        assert_eq!(EnvAction::from(99), EnvAction::Hold); // Invalid defaults to Hold
    }
}
