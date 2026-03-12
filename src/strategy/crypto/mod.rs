//! Crypto market strategies
//!
//! Specialized strategies for crypto UP/DOWN markets (BTC, ETH, SOL).

mod discovery;
mod runner;
mod series_registry;

pub use discovery::CryptoMarketDiscovery;
pub use runner::{run_crypto_split_arb, CryptoSplitArbConfig};
pub use series_registry::{
    all_updown_series_ids, horizon_for_series, known_binance_symbols, series_ids_for_symbol,
    series_info, symbol_and_window_for_series, CryptoSeriesInfo,
};
