//! RL Strategy Integration
//!
//! Implements the Strategy trait using an RL agent for decision making.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Timelike, Utc};
use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::{OrderRequest, OrderSide, OrderType, Quote, Side, TimeInForce};
use crate::error::Result;
use crate::rl::config::RLConfig;
use crate::rl::core::{
    ContinuousAction, DefaultStateEncoder, DiscreteAction, PnLRewardFunction, RawObservation,
    RewardFunction, RewardTransition, StateEncoder,
};
use crate::rl::memory::ReplayBuffer;
use crate::strategy::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, RiskLevel, StrategyAction,
    StrategyStateInfo, Strategy,
};

/// RL-based trading strategy
///
/// Uses reinforcement learning to make trading decisions based on market state.
pub struct RLStrategy {
    /// Strategy ID
    id: String,
    /// RL configuration
    config: RLConfig,
    /// State encoder
    encoder: Arc<DefaultStateEncoder>,
    /// Reward function
    reward_fn: Box<dyn RewardFunction>,
    /// Replay buffer for experience storage
    replay_buffer: Arc<RwLock<ReplayBuffer>>,
    /// Current observation
    current_obs: RawObservation,
    /// Previous observation (for reward calculation)
    prev_obs: Option<RawObservation>,
    /// Current position
    position: Option<PositionInfo>,
    /// Strategy state
    state: StrategyStateInfo,
    /// Token IDs to track (UP, DOWN)
    token_ids: (String, String),
    /// Binance symbol to track
    symbol: String,
    /// Online learning enabled
    online_learning: bool,
    /// Step counter
    step_count: u64,
    /// Last action taken
    last_action: Option<ContinuousAction>,
    /// Exploration rate
    exploration_rate: f32,
}

impl RLStrategy {
    /// Create a new RL strategy
    pub fn new(
        id: String,
        config: RLConfig,
        up_token: String,
        down_token: String,
        symbol: String,
    ) -> Self {
        let state = StrategyStateInfo {
            strategy_id: id.clone(),
            phase: "initializing".to_string(),
            enabled: true,
            ..Default::default()
        };

        Self {
            id,
            config: config.clone(),
            encoder: Arc::new(DefaultStateEncoder::new()),
            reward_fn: Box::new(PnLRewardFunction::new()),
            replay_buffer: Arc::new(RwLock::new(ReplayBuffer::new(
                config.training.buffer_size,
            ))),
            current_obs: RawObservation::new(),
            prev_obs: None,
            position: None,
            state,
            token_ids: (up_token, down_token),
            symbol,
            online_learning: config.training.online_learning,
            step_count: 0,
            last_action: None,
            exploration_rate: config.training.exploration_rate,
        }
    }

    /// Update observation from market data
    fn update_observation(&mut self, update: &MarketUpdate) {
        match update {
            MarketUpdate::BinancePrice { price, .. } => {
                self.current_obs.spot_price = Some(*price);

                // Update price history
                if self.current_obs.price_history.len() >= 15 {
                    self.current_obs.price_history.remove(0);
                }
                self.current_obs.price_history.push(*price);
            }
            MarketUpdate::PolymarketQuote {
                token_id,
                side,
                quote,
                ..
            } => {
                match side {
                    Side::Up if *token_id == self.token_ids.0 => {
                        self.current_obs.up_bid = quote.best_bid;
                        self.current_obs.up_ask = quote.best_ask;
                    }
                    Side::Down if *token_id == self.token_ids.1 => {
                        self.current_obs.down_bid = quote.best_bid;
                        self.current_obs.down_ask = quote.best_ask;
                    }
                    _ => {}
                }

                // Recalculate derived features
                self.current_obs.calculate_spreads();
                self.current_obs.calculate_sum_of_asks();
            }
            _ => {}
        }

        // Update time features
        let now = Utc::now();
        self.current_obs
            .update_time_features(now.hour(), now.weekday().num_days_from_monday());

        // Update position features
        if let Some(pos) = &self.position {
            self.current_obs.has_position = true;
            self.current_obs.position_side = Some(pos.side);
            self.current_obs.position_shares = pos.shares;
            self.current_obs.entry_price = Some(pos.entry_price);
            self.current_obs.unrealized_pnl = Some(pos.unrealized_pnl);
            self.current_obs.position_duration_secs =
                Some((now - pos.opened_at).num_seconds());
        } else {
            self.current_obs.has_position = false;
            self.current_obs.position_side = None;
            self.current_obs.position_shares = 0;
            self.current_obs.entry_price = None;
            self.current_obs.unrealized_pnl = None;
            self.current_obs.position_duration_secs = None;
        }
    }

    /// Select action using the current policy
    ///
    /// For now, this uses a simple rule-based policy as placeholder.
    /// In production, this would query the neural network.
    fn select_action(&mut self) -> ContinuousAction {
        // Get encoded state
        let state_vec = self.encoder.encode(&self.current_obs);

        // Placeholder: rule-based action selection
        // In production, this queries the PPO actor network
        let action = self.rule_based_action();

        // Apply exploration noise
        let action = if rand::random::<f32>() < self.exploration_rate {
            // Random action for exploration
            ContinuousAction::new(
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>() * 2.0 - 1.0,
                rand::random::<f32>(),
                0.0,
                0.0,
            )
        } else {
            action
        };

        self.last_action = Some(action);
        action
    }

    /// Simple rule-based action as baseline
    fn rule_based_action(&self) -> ContinuousAction {
        // Check for arbitrage opportunity
        if let Some(sum) = self.current_obs.sum_of_asks {
            let sum_f32: f32 = sum.to_string().parse().unwrap_or(1.0);

            // Good arb opportunity: sum < 0.96
            if sum_f32 < 0.96 && !self.current_obs.has_position {
                // Choose side based on spread
                let side_pref = match (self.current_obs.spread_up, self.current_obs.spread_down) {
                    (Some(up), Some(down)) if up < down => 0.5, // Prefer UP
                    (Some(up), Some(down)) if down < up => -0.5, // Prefer DOWN
                    _ => 0.0, // No preference
                };

                return ContinuousAction::new(
                    0.7,  // Buy signal
                    side_pref,
                    0.5,  // Medium urgency
                    0.0,
                    0.0,
                );
            }

            // Exit opportunity: sum > 1.0 with position
            if sum_f32 > 1.0 && self.current_obs.has_position {
                return ContinuousAction::new(
                    -0.8, // Sell signal
                    0.0,
                    0.7, // Urgent exit
                    0.0,
                    0.0,
                );
            }
        }

        // Default: hold
        ContinuousAction::default()
    }

    /// Convert RL action to strategy actions
    fn action_to_orders(&self, action: ContinuousAction) -> Vec<StrategyAction> {
        let discrete = action.to_discrete();
        let mut actions = Vec::new();

        match discrete {
            DiscreteAction::Hold => {
                // No action
            }
            DiscreteAction::BuyUp => {
                if let Some(ask) = self.current_obs.up_ask {
                    let shares = self.calculate_position_size(&action);
                    let order = self.create_order(&self.token_ids.0, Side::Up, shares, ask, &action);
                    actions.push(StrategyAction::SubmitOrder {
                        client_order_id: format!("rl_buy_up_{}", self.step_count),
                        order,
                        priority: if action.is_aggressive() { 10 } else { 5 },
                    });
                }
            }
            DiscreteAction::BuyDown => {
                if let Some(ask) = self.current_obs.down_ask {
                    let shares = self.calculate_position_size(&action);
                    let order = self.create_order(&self.token_ids.1, Side::Down, shares, ask, &action);
                    actions.push(StrategyAction::SubmitOrder {
                        client_order_id: format!("rl_buy_down_{}", self.step_count),
                        order,
                        priority: if action.is_aggressive() { 10 } else { 5 },
                    });
                }
            }
            DiscreteAction::SellPosition => {
                if let Some(pos) = &self.position {
                    let bid = if pos.side == Side::Up {
                        self.current_obs.up_bid
                    } else {
                        self.current_obs.down_bid
                    };

                    if let Some(bid) = bid {
                        let order = OrderRequest {
                            client_order_id: Uuid::new_v4().to_string(),
                            token_id: pos.token_id.clone(),
                            market_side: pos.side,
                            order_side: OrderSide::Sell,
                            shares: pos.shares,
                            limit_price: bid,
                            order_type: if action.is_aggressive() {
                                OrderType::Market
                            } else {
                                OrderType::Limit
                            },
                            time_in_force: TimeInForce::GTC,
                        };

                        actions.push(StrategyAction::SubmitOrder {
                            client_order_id: format!("rl_sell_{}", self.step_count),
                            order,
                            priority: 10, // High priority for exits
                        });
                    }
                }
            }
            DiscreteAction::EnterHedge => {
                // Enter both sides for split-arb
                // This is a simplified version
                debug!("Hedge action requested but not implemented in simple mode");
            }
        }

        actions
    }

    /// Calculate position size based on action
    fn calculate_position_size(&self, action: &ContinuousAction) -> u64 {
        let base_size = 100; // Base position size
        let size_multiplier = action.position_size_pct();
        (base_size as f32 * size_multiplier).max(1.0) as u64
    }

    /// Create an order request
    fn create_order(
        &self,
        token_id: &str,
        market_side: Side,
        shares: u64,
        price: Decimal,
        action: &ContinuousAction,
    ) -> OrderRequest {
        OrderRequest {
            client_order_id: Uuid::new_v4().to_string(),
            token_id: token_id.to_string(),
            market_side,
            order_side: OrderSide::Buy,
            shares,
            limit_price: price,
            order_type: if action.is_aggressive() {
                OrderType::Market
            } else {
                OrderType::Limit
            },
            time_in_force: TimeInForce::GTC,
        }
    }

    /// Compute reward transition from state change
    fn compute_reward_transition(&self) -> RewardTransition {
        let mut transition = RewardTransition::default();

        // Calculate unrealized PnL delta
        if let (Some(prev), Some(curr)) = (
            self.prev_obs.as_ref().and_then(|o| o.unrealized_pnl),
            self.current_obs.unrealized_pnl,
        ) {
            transition.unrealized_pnl_delta = Some(curr - prev);
        }

        // Set sum of asks at entry
        if self.current_obs.has_position && !self.prev_obs.as_ref().map(|o| o.has_position).unwrap_or(false) {
            transition.sum_of_asks_at_entry = self.current_obs.sum_of_asks;
        }

        // Risk exposure from config
        transition.risk_exposure = self.current_obs.exposure_pct.to_string().parse().unwrap_or(0.0);

        transition
    }

    /// Decay exploration rate
    fn decay_exploration(&mut self) {
        self.exploration_rate = (self.exploration_rate * self.config.training.exploration_decay)
            .max(self.config.training.exploration_min);
    }
}

#[async_trait]
impl Strategy for RLStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "RL Strategy"
    }

    fn description(&self) -> &str {
        "Reinforcement learning based trading strategy using PPO"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![
            DataFeed::PolymarketQuotes {
                tokens: vec![self.token_ids.0.clone(), self.token_ids.1.clone()],
            },
            DataFeed::BinanceSpot {
                symbols: vec![self.symbol.clone()],
            },
            DataFeed::Tick { interval_ms: 1000 },
        ]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        // Store previous observation for reward calculation
        self.prev_obs = Some(self.current_obs.clone());

        // Update current observation
        self.update_observation(update);

        // Increment step counter
        self.step_count += 1;

        // Select action using policy
        let action = self.select_action();

        // Convert to strategy actions
        let actions = self.action_to_orders(action);

        // Store experience for training (if online learning)
        if self.online_learning {
            let reward_transition = self.compute_reward_transition();
            let reward_signal = self.reward_fn.compute(&reward_transition);

            // Store transition in replay buffer
            // (This would be a full transition once we have next state)
            debug!(
                "Step {}: reward={:.4}, exploration={:.4}",
                self.step_count, reward_signal.total, self.exploration_rate
            );
        }

        // Update state
        self.state.phase = if self.position.is_some() {
            "in_position".to_string()
        } else {
            "watching".to_string()
        };
        self.state.last_update = Utc::now();

        Ok(actions)
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        // Update position based on fills
        if update.filled_qty > 0 {
            if let Some(price) = update.avg_fill_price {
                // Determine if this is opening or closing a position
                // Simplified: check if we have a position
                if self.position.is_some() {
                    // Assume it's a close
                    let realized_pnl = self.position.as_ref().map(|p| {
                        (price - p.entry_price) * Decimal::from(p.shares)
                    });

                    if let Some(pnl) = realized_pnl {
                        self.state.realized_pnl_today += pnl;
                    }

                    self.position = None;
                    info!("Position closed: PnL = {:?}", realized_pnl);
                } else {
                    // Opening a new position
                    // We need to know which side this is
                    // For now, assume the order_id tells us
                    let side = if update.client_order_id.as_ref().map(|s| s.contains("up")).unwrap_or(false) {
                        Side::Up
                    } else {
                        Side::Down
                    };

                    let token_id = if side == Side::Up {
                        self.token_ids.0.clone()
                    } else {
                        self.token_ids.1.clone()
                    };

                    self.position = Some(PositionInfo::new(
                        token_id,
                        side,
                        update.filled_qty,
                        price,
                        self.id.clone(),
                    ));

                    info!("Position opened: {:?} @ {}", side, price);
                }
            }
        }

        // Decay exploration after each order
        self.decay_exploration();

        Ok(vec![])
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        // Periodic updates (e.g., time-based actions)
        self.current_obs.update_time_features(
            now.hour(),
            now.weekday().num_days_from_monday(),
        );

        // Update position price if we have one
        if let Some(pos) = &mut self.position {
            let current_price = if pos.side == Side::Up {
                self.current_obs.up_bid
            } else {
                self.current_obs.down_bid
            };

            if let Some(price) = current_price {
                pos.update_price(price);
                self.state.unrealized_pnl = pos.unrealized_pnl;
            }
        }

        Ok(vec![])
    }

    fn state(&self) -> StrategyStateInfo {
        self.state.clone()
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.position.iter().cloned().collect()
    }

    fn is_active(&self) -> bool {
        self.position.is_some()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        info!("RL Strategy shutting down");

        // Close any open positions
        let mut actions = Vec::new();

        if let Some(pos) = &self.position {
            let bid = if pos.side == Side::Up {
                self.current_obs.up_bid
            } else {
                self.current_obs.down_bid
            };

            if let Some(bid) = bid {
                actions.push(StrategyAction::SubmitOrder {
                    client_order_id: format!("rl_shutdown_{}", self.step_count),
                    order: OrderRequest {
                        client_order_id: Uuid::new_v4().to_string(),
                        token_id: pos.token_id.clone(),
                        market_side: pos.side,
                        order_side: OrderSide::Sell,
                        shares: pos.shares,
                        limit_price: bid,
                        order_type: OrderType::Market,
                        time_in_force: TimeInForce::IOC,
                    },
                    priority: 100, // Highest priority
                });
            }
        }

        self.state.enabled = false;
        self.state.phase = "shutdown".to_string();

        Ok(actions)
    }

    fn reset(&mut self) {
        self.current_obs = RawObservation::new();
        self.prev_obs = None;
        self.position = None;
        self.step_count = 0;
        self.last_action = None;
        self.exploration_rate = self.config.training.exploration_rate;
        self.state = StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "initializing".to_string(),
            enabled: true,
            ..Default::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rl_strategy_creation() {
        let config = RLConfig::default();
        let strategy = RLStrategy::new(
            "test_rl".to_string(),
            config,
            "up_token".to_string(),
            "down_token".to_string(),
            "BTCUSDT".to_string(),
        );

        assert_eq!(strategy.id(), "test_rl");
        assert!(!strategy.is_active());
    }

    #[test]
    fn test_rule_based_action() {
        let config = RLConfig::default();
        let mut strategy = RLStrategy::new(
            "test_rl".to_string(),
            config,
            "up_token".to_string(),
            "down_token".to_string(),
            "BTCUSDT".to_string(),
        );

        // Without sum of asks, should hold
        let action = strategy.rule_based_action();
        assert_eq!(action.position_delta, 0.0);
    }
}
