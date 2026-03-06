//! Domain Agent compatibility implementations
//!
//! These platform agents are retained only for transitional compatibility.
//! New live strategy work should land in the canonical `Strategy` runtime.

mod crypto_agent;
mod event_edge_agent;
mod nba_agent;

#[cfg(feature = "rl")]
mod rl_crypto_agent;

pub use crypto_agent::{CryptoAgent, CryptoAgentConfig};
pub use event_edge_agent::EventEdgePlatformAgent;
pub use nba_agent::NbaComebackAgent;

#[cfg(feature = "rl")]
pub use rl_crypto_agent::{RLCryptoAgent, RLCryptoAgentConfig};
