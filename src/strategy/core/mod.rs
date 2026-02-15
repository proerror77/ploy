//! Core strategy components shared across market types
//!
//! This module contains the fundamental abstractions for split arbitrage
//! that work across crypto, sports, and other binary markets.

mod position;
mod price_cache;
mod split_engine;
mod traits;

pub use position::{ArbSide, ArbStats, HedgedPosition, PartialPosition, PositionStatus};
pub use price_cache::PriceCache;
pub use split_engine::{SplitArbConfig, SplitArbEngine};
pub use traits::{BinaryMarket, MarketDiscovery, MarketType};
