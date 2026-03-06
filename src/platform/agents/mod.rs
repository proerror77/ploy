//! Domain Agent compatibility implementations
//!
//! These platform agents are retained only for transitional compatibility.
//! New live strategy work should land in the canonical `Strategy` runtime.

pub mod crypto_agent;
pub mod event_edge_agent;
pub mod nba_agent;

#[cfg(feature = "rl")]
pub mod rl_crypto_agent;
