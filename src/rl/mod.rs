//! Reinforcement Learning Module
//!
//! Provides RL-based trading strategies using the Burn framework.
//!
//! # Features
//!
//! - **State Representation**: Market observations encoded as tensors
//! - **Action Space**: Discrete (Hold/Buy/Sell) and continuous (sizing, thresholds)
//! - **Algorithms**: PPO (Proximal Policy Optimization)
//! - **Online Learning**: Adapt during live trading
//!
//! # Usage
//!
//! Enable the `rl` feature in Cargo.toml:
//! ```toml
//! ploy = { features = ["rl"] }
//! ```

pub mod config;
pub mod core;
pub mod networks;
pub mod algorithms;
pub mod memory;
pub mod training;
pub mod integration;
pub mod environment;

// Config exports
pub use config::{RLConfig, PPOConfig, TrainingConfig, RewardConfig};

// Core exports
pub use core::{
    RawObservation, StateEncoder, DefaultStateEncoder, TOTAL_FEATURES,
    DiscreteAction, ContinuousAction, HybridAction, NUM_DISCRETE_ACTIONS, CONTINUOUS_ACTION_DIM,
    RewardSignal, RewardFunction, PnLRewardFunction, RewardTransition,
};

// Memory exports
pub use memory::ReplayBuffer;

// Integration exports
pub use integration::RLStrategy;

// Environment exports
pub use environment::{TradingEnvironment, TradingEnvConfig, SimulatedMarket, MarketConfig, StepResult, EnvAction};
