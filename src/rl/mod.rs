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

pub mod algorithms;
pub mod config;
pub mod core;
pub mod environment;
pub mod integration;
pub mod memory;
pub mod networks;
pub mod training;

// Config exports
pub use config::{PPOConfig, RLConfig, RewardConfig, TrainingConfig};

// Core exports
pub use core::{
    ContinuousAction, DefaultStateEncoder, DiscreteAction, HybridAction, PnLRewardFunction,
    RawObservation, RewardFunction, RewardSignal, RewardTransition, StateEncoder,
    CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS, TOTAL_FEATURES,
};

// Memory exports
pub use memory::ReplayBuffer;

// Integration exports
pub use integration::RLStrategy;

// Environment exports
pub use environment::{
    EnvAction, MarketConfig, SimulatedMarket, StepResult, TradingEnvConfig, TradingEnvironment,
};
