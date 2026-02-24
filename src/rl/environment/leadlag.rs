//! Lead-Lag Trading Environment for RL Training
//!
//! Uses Binance LOB features to predict Polymarket price movements.
//! Position management: $1 per trade, $50 max total position.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Lead-Lag environment configuration
#[derive(Debug, Clone)]
pub struct LeadLagConfig {
    /// Amount per trade in USD
    pub trade_size_usd: Decimal,
    /// Maximum total position in USD
    pub max_position_usd: Decimal,
    /// Transaction cost (Polymarket fee ~0.5%)
    pub transaction_cost: Decimal,
    /// Maximum steps per episode
    pub max_steps: usize,
    /// Reward scaling factor
    pub reward_scale: f32,
    /// Penalty for holding too long without action
    pub hold_penalty: f32,
    /// Bonus for profitable trades
    pub profit_bonus: f32,
}

impl Default for LeadLagConfig {
    fn default() -> Self {
        Self {
            trade_size_usd: dec!(1.0),
            max_position_usd: dec!(50.0),
            transaction_cost: dec!(0.005), // 0.5%
            max_steps: 10000,
            reward_scale: 100.0,
            hold_penalty: -0.001,
            profit_bonus: 0.1,
        }
    }
}

/// Actions available in the lead-lag environment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadLagAction {
    /// Do nothing
    Hold,
    /// Buy YES token ($1)
    BuyYes,
    /// Buy NO token ($1)
    BuyNo,
    /// Close all YES positions
    CloseYes,
    /// Close all NO positions
    CloseNo,
}

impl From<usize> for LeadLagAction {
    fn from(action: usize) -> Self {
        match action {
            0 => LeadLagAction::Hold,
            1 => LeadLagAction::BuyYes,
            2 => LeadLagAction::BuyNo,
            3 => LeadLagAction::CloseYes,
            4 => LeadLagAction::CloseNo,
            _ => LeadLagAction::Hold,
        }
    }
}

impl LeadLagAction {
    pub fn num_actions() -> usize {
        5
    }
}

/// LOB-based observation for the RL agent
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LobObservation {
    // Binance LOB features
    pub bn_mid_price: f32,
    pub bn_obi_5: f32,
    pub bn_obi_10: f32,
    pub bn_spread_bps: f32,
    pub bn_bid_volume: f32,
    pub bn_ask_volume: f32,

    // Momentum features
    pub momentum_1s: f32,
    pub momentum_5s: f32,
    pub momentum_10s: f32,

    // Polymarket prices
    pub pm_yes_price: f32,
    pub pm_no_price: f32,

    // Position state
    pub yes_position_usd: f32,
    pub no_position_usd: f32,
    pub yes_avg_entry: f32,
    pub no_avg_entry: f32,
    pub total_exposure_pct: f32, // 0-1, exposure / max_position

    // Unrealized PnL
    pub yes_unrealized_pnl: f32,
    pub no_unrealized_pnl: f32,

    // Historical signals (last 10 OBI readings)
    pub obi_history: [f32; 10],
}

impl LobObservation {
    pub const FEATURE_DIM: usize = 30; // Total features

    /// Convert to feature vector for neural network
    pub fn to_features(&self) -> Vec<f32> {
        let mut features = Vec::with_capacity(Self::FEATURE_DIM);

        // LOB features (6)
        features.push(self.bn_mid_price / 100000.0); // Normalize BTC price
        features.push(self.bn_obi_5);
        features.push(self.bn_obi_10);
        features.push(self.bn_spread_bps / 100.0); // Normalize spread
        features.push(self.bn_bid_volume.ln().max(0.0) / 10.0); // Log volume
        features.push(self.bn_ask_volume.ln().max(0.0) / 10.0);

        // Momentum features (3)
        features.push(self.momentum_1s * 100.0); // Scale small percentages
        features.push(self.momentum_5s * 100.0);
        features.push(self.momentum_10s * 100.0);

        // Polymarket prices (2)
        features.push(self.pm_yes_price);
        features.push(self.pm_no_price);

        // Position state (5)
        features.push(self.yes_position_usd / 50.0); // Normalize by max
        features.push(self.no_position_usd / 50.0);
        features.push(self.yes_avg_entry);
        features.push(self.no_avg_entry);
        features.push(self.total_exposure_pct);

        // Unrealized PnL (2)
        features.push(self.yes_unrealized_pnl);
        features.push(self.no_unrealized_pnl);

        // OBI history (10)
        for obi in &self.obi_history {
            features.push(*obi);
        }

        // Derived features (2)
        features.push(self.bn_obi_5 - self.bn_obi_10); // OBI momentum
        features.push(self.pm_yes_price + self.pm_no_price - 1.0); // Market efficiency

        features
    }
}

/// Position tracking for the environment
#[derive(Debug, Clone, Default)]
pub struct Position {
    /// USD value in YES tokens
    pub yes_usd: Decimal,
    /// YES shares held
    pub yes_shares: Decimal,
    /// Average entry price for YES
    pub yes_avg_entry: Decimal,
    /// USD value in NO tokens
    pub no_usd: Decimal,
    /// NO shares held
    pub no_shares: Decimal,
    /// Average entry price for NO
    pub no_avg_entry: Decimal,
    /// Realized PnL
    pub realized_pnl: Decimal,
    /// Number of trades executed
    pub num_trades: u32,
    /// Number of winning trades
    pub winning_trades: u32,
}

impl Position {
    pub fn total_exposure(&self) -> Decimal {
        self.yes_usd + self.no_usd
    }

    pub fn can_buy(&self, amount: Decimal, max_position: Decimal) -> bool {
        self.total_exposure() + amount <= max_position
    }

    pub fn unrealized_pnl(&self, yes_price: Decimal, no_price: Decimal) -> Decimal {
        let yes_pnl = if self.yes_shares > Decimal::ZERO {
            self.yes_shares * yes_price - self.yes_usd
        } else {
            Decimal::ZERO
        };

        let no_pnl = if self.no_shares > Decimal::ZERO {
            self.no_shares * no_price - self.no_usd
        } else {
            Decimal::ZERO
        };

        yes_pnl + no_pnl
    }
}

/// Step result from the environment
#[derive(Debug, Clone)]
pub struct LeadLagStepResult {
    pub observation: Vec<f32>,
    pub reward: f32,
    pub done: bool,
    pub truncated: bool,
    pub info: LeadLagInfo,
}

/// Additional info from step
#[derive(Debug, Clone, Default)]
pub struct LeadLagInfo {
    pub step: usize,
    pub total_pnl: f32,
    pub realized_pnl: f32,
    pub unrealized_pnl: f32,
    pub num_trades: u32,
    pub win_rate: f32,
    pub yes_position: f32,
    pub no_position: f32,
    pub action_taken: Option<LeadLagAction>,
    pub action_valid: bool,
}

/// Lead-Lag Trading Environment
pub struct LeadLagEnvironment {
    config: LeadLagConfig,
    position: Position,
    step_count: usize,
    current_obs: LobObservation,
    obi_history: VecDeque<f32>,
    data: Vec<LobDataPoint>,
    data_index: usize,
    episode_reward: f32,
}

/// Single data point for training
#[derive(Debug, Clone)]
pub struct LobDataPoint {
    pub timestamp_ms: i64,
    pub bn_mid_price: Decimal,
    pub bn_obi_5: Decimal,
    pub bn_obi_10: Decimal,
    pub bn_spread_bps: Decimal,
    pub bn_bid_volume: Decimal,
    pub bn_ask_volume: Decimal,
    pub momentum_1s: Decimal,
    pub momentum_5s: Decimal,
    pub pm_yes_price: Decimal,
    pub pm_no_price: Decimal,
}

impl LeadLagEnvironment {
    /// Create a new environment with historical data
    pub fn new(config: LeadLagConfig, data: Vec<LobDataPoint>) -> Self {
        Self {
            config,
            position: Position::default(),
            step_count: 0,
            current_obs: LobObservation::default(),
            obi_history: VecDeque::with_capacity(10),
            data,
            data_index: 0,
            episode_reward: 0.0,
        }
    }

    /// Reset the environment for a new episode
    pub fn reset(&mut self) -> Vec<f32> {
        self.position = Position::default();
        self.step_count = 0;
        self.obi_history.clear();
        self.data_index = 0;
        self.episode_reward = 0.0;

        // Initialize OBI history
        for _ in 0..10 {
            self.obi_history.push_back(0.0);
        }

        // Load first observation
        self.update_observation();
        self.current_obs.to_features()
    }

    /// Take a step in the environment
    pub fn step(&mut self, action: LeadLagAction) -> LeadLagStepResult {
        self.step_count += 1;
        self.data_index += 1;

        let mut reward = 0.0f32;
        let mut action_valid = true;

        // Get current prices
        let yes_price = if self.data_index < self.data.len() {
            self.data[self.data_index].pm_yes_price
        } else {
            Decimal::ZERO
        };
        let no_price = if self.data_index < self.data.len() {
            self.data[self.data_index].pm_no_price
        } else {
            Decimal::ZERO
        };

        // Execute action
        match action {
            LeadLagAction::Hold => {
                reward += self.config.hold_penalty;
            }
            LeadLagAction::BuyYes => {
                if self
                    .position
                    .can_buy(self.config.trade_size_usd, self.config.max_position_usd)
                    && yes_price > Decimal::ZERO
                {
                    let shares = self.config.trade_size_usd / yes_price;
                    let cost =
                        self.config.trade_size_usd * (Decimal::ONE + self.config.transaction_cost);

                    // Update average entry
                    let total_shares = self.position.yes_shares + shares;
                    if total_shares > Decimal::ZERO {
                        self.position.yes_avg_entry =
                            (self.position.yes_usd + cost) / total_shares * yes_price / cost;
                    }

                    self.position.yes_shares += shares;
                    self.position.yes_usd += cost;
                    self.position.num_trades += 1;
                } else {
                    action_valid = false;
                    reward -= 0.01; // Penalty for invalid action
                }
            }
            LeadLagAction::BuyNo => {
                if self
                    .position
                    .can_buy(self.config.trade_size_usd, self.config.max_position_usd)
                    && no_price > Decimal::ZERO
                {
                    let shares = self.config.trade_size_usd / no_price;
                    let cost =
                        self.config.trade_size_usd * (Decimal::ONE + self.config.transaction_cost);

                    let total_shares = self.position.no_shares + shares;
                    if total_shares > Decimal::ZERO {
                        self.position.no_avg_entry =
                            (self.position.no_usd + cost) / total_shares * no_price / cost;
                    }

                    self.position.no_shares += shares;
                    self.position.no_usd += cost;
                    self.position.num_trades += 1;
                } else {
                    action_valid = false;
                    reward -= 0.01;
                }
            }
            LeadLagAction::CloseYes => {
                if self.position.yes_shares > Decimal::ZERO && yes_price > Decimal::ZERO {
                    let proceeds = self.position.yes_shares
                        * yes_price
                        * (Decimal::ONE - self.config.transaction_cost);
                    let pnl = proceeds - self.position.yes_usd;

                    self.position.realized_pnl += pnl;
                    if pnl > Decimal::ZERO {
                        self.position.winning_trades += 1;
                        reward += self.config.profit_bonus;
                    }

                    // Add PnL to reward
                    reward += decimal_to_f32(pnl) * self.config.reward_scale;

                    // Reset YES position
                    self.position.yes_shares = Decimal::ZERO;
                    self.position.yes_usd = Decimal::ZERO;
                    self.position.yes_avg_entry = Decimal::ZERO;
                } else {
                    action_valid = false;
                    reward -= 0.01;
                }
            }
            LeadLagAction::CloseNo => {
                if self.position.no_shares > Decimal::ZERO && no_price > Decimal::ZERO {
                    let proceeds = self.position.no_shares
                        * no_price
                        * (Decimal::ONE - self.config.transaction_cost);
                    let pnl = proceeds - self.position.no_usd;

                    self.position.realized_pnl += pnl;
                    if pnl > Decimal::ZERO {
                        self.position.winning_trades += 1;
                        reward += self.config.profit_bonus;
                    }

                    reward += decimal_to_f32(pnl) * self.config.reward_scale;

                    self.position.no_shares = Decimal::ZERO;
                    self.position.no_usd = Decimal::ZERO;
                    self.position.no_avg_entry = Decimal::ZERO;
                } else {
                    action_valid = false;
                    reward -= 0.01;
                }
            }
        }

        // Update observation
        self.update_observation();

        // Calculate unrealized PnL for shaping reward
        let unrealized = self.position.unrealized_pnl(yes_price, no_price);
        reward += decimal_to_f32(unrealized) * 0.01; // Small shaping reward

        self.episode_reward += reward;

        // Check if done
        let done = self.data_index >= self.data.len() - 1;
        let truncated = self.step_count >= self.config.max_steps;

        // Build info
        let info = LeadLagInfo {
            step: self.step_count,
            total_pnl: decimal_to_f32(self.position.realized_pnl + unrealized),
            realized_pnl: decimal_to_f32(self.position.realized_pnl),
            unrealized_pnl: decimal_to_f32(unrealized),
            num_trades: self.position.num_trades,
            win_rate: if self.position.num_trades > 0 {
                self.position.winning_trades as f32 / self.position.num_trades as f32
            } else {
                0.0
            },
            yes_position: decimal_to_f32(self.position.yes_usd),
            no_position: decimal_to_f32(self.position.no_usd),
            action_taken: Some(action),
            action_valid,
        };

        LeadLagStepResult {
            observation: self.current_obs.to_features(),
            reward,
            done: done || truncated,
            truncated,
            info,
        }
    }

    /// Update current observation from data
    fn update_observation(&mut self) {
        if self.data_index >= self.data.len() {
            return;
        }

        let dp = &self.data[self.data_index];

        // Update OBI history
        let obi_5 = decimal_to_f32(dp.bn_obi_5);
        self.obi_history.push_back(obi_5);
        if self.obi_history.len() > 10 {
            self.obi_history.pop_front();
        }

        // Get current prices for position tracking
        let yes_price = dp.pm_yes_price;
        let no_price = dp.pm_no_price;

        self.current_obs = LobObservation {
            bn_mid_price: decimal_to_f32(dp.bn_mid_price),
            bn_obi_5: obi_5,
            bn_obi_10: decimal_to_f32(dp.bn_obi_10),
            bn_spread_bps: decimal_to_f32(dp.bn_spread_bps),
            bn_bid_volume: decimal_to_f32(dp.bn_bid_volume),
            bn_ask_volume: decimal_to_f32(dp.bn_ask_volume),
            momentum_1s: decimal_to_f32(dp.momentum_1s),
            momentum_5s: decimal_to_f32(dp.momentum_5s),
            momentum_10s: 0.0, // Calculate if needed
            pm_yes_price: decimal_to_f32(yes_price),
            pm_no_price: decimal_to_f32(no_price),
            yes_position_usd: decimal_to_f32(self.position.yes_usd),
            no_position_usd: decimal_to_f32(self.position.no_usd),
            yes_avg_entry: decimal_to_f32(self.position.yes_avg_entry),
            no_avg_entry: decimal_to_f32(self.position.no_avg_entry),
            total_exposure_pct: decimal_to_f32(
                self.position.total_exposure() / self.config.max_position_usd,
            ),
            yes_unrealized_pnl: if self.position.yes_shares > Decimal::ZERO {
                decimal_to_f32(self.position.yes_shares * yes_price - self.position.yes_usd)
            } else {
                0.0
            },
            no_unrealized_pnl: if self.position.no_shares > Decimal::ZERO {
                decimal_to_f32(self.position.no_shares * no_price - self.position.no_usd)
            } else {
                0.0
            },
            obi_history: self.obi_history_array(),
        };
    }

    fn obi_history_array(&self) -> [f32; 10] {
        let mut arr = [0.0f32; 10];
        for (i, &val) in self.obi_history.iter().enumerate() {
            if i < 10 {
                arr[i] = val;
            }
        }
        arr
    }

    /// Get the number of actions
    pub fn num_actions(&self) -> usize {
        LeadLagAction::num_actions()
    }

    /// Get the observation dimension
    pub fn observation_dim(&self) -> usize {
        LobObservation::FEATURE_DIM
    }
}

/// Convert Decimal to f32
fn decimal_to_f32(d: Decimal) -> f32 {
    d.to_string().parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_conversion() {
        assert_eq!(LeadLagAction::from(0), LeadLagAction::Hold);
        assert_eq!(LeadLagAction::from(1), LeadLagAction::BuyYes);
        assert_eq!(LeadLagAction::from(2), LeadLagAction::BuyNo);
        assert_eq!(LeadLagAction::from(3), LeadLagAction::CloseYes);
        assert_eq!(LeadLagAction::from(4), LeadLagAction::CloseNo);
    }

    #[test]
    fn test_position_exposure() {
        let pos = Position {
            yes_usd: dec!(10),
            no_usd: dec!(15),
            ..Default::default()
        };
        assert_eq!(pos.total_exposure(), dec!(25));
        assert!(pos.can_buy(dec!(1), dec!(50)));
        assert!(!pos.can_buy(dec!(30), dec!(50)));
    }

    #[test]
    fn test_observation_features() {
        let obs = LobObservation::default();
        let features = obs.to_features();
        assert_eq!(features.len(), LobObservation::FEATURE_DIM);
    }
}
