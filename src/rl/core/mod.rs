//! Core RL abstractions
//!
//! Fundamental types for state representation, actions, and rewards.

pub mod state;
pub mod action;
pub mod reward;

pub use state::{RawObservation, StateEncoder, DefaultStateEncoder, TOTAL_FEATURES};
pub use action::{DiscreteAction, ContinuousAction, HybridAction, NUM_DISCRETE_ACTIONS, CONTINUOUS_ACTION_DIM};
pub use reward::{RewardSignal, RewardFunction, PnLRewardFunction, RewardTransition};
