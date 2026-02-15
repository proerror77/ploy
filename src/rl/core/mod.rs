//! Core RL abstractions
//!
//! Fundamental types for state representation, actions, and rewards.

pub mod action;
pub mod reward;
pub mod state;

pub use action::{
    ContinuousAction, DiscreteAction, HybridAction, CONTINUOUS_ACTION_DIM, NUM_DISCRETE_ACTIONS,
};
pub use reward::{PnLRewardFunction, RewardFunction, RewardSignal, RewardTransition};
pub use state::{DefaultStateEncoder, RawObservation, StateEncoder, TOTAL_FEATURES};
