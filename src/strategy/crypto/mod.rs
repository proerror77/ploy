//! Crypto market strategies
//!
//! Specialized strategies for crypto UP/DOWN markets (BTC, ETH, SOL).

mod discovery;
mod runner;

pub use discovery::CryptoMarketDiscovery;
pub use runner::{run_crypto_split_arb, CryptoSplitArbConfig};
