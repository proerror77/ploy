//! Strategy Integration
//!
//! Connects RL agents to the trading strategy system.

pub mod live_runtime;
pub mod rl_strategy;

pub use live_runtime::{RLCryptoRuntime, RLCryptoRuntimeConfig};
pub use rl_strategy::RLStrategy;
