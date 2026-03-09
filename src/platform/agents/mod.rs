//! Domain Agent Implementations
//!
//! Remaining platform agents are compatibility/runtime adapters only.

#[cfg(feature = "rl")]
mod rl_crypto_agent;

#[cfg(feature = "rl")]
pub use rl_crypto_agent::{RLCryptoAgent, RLCryptoAgentConfig};
