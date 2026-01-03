//! Sports market strategies
//!
//! Specialized strategies for sports betting markets (NBA, NFL, etc.).

mod discovery;
mod runner;

pub use discovery::{SportsMarketDiscovery, SportsLeague};
pub use runner::{run_sports_split_arb, SportsSplitArbConfig};
