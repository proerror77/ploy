//! Freshness tracking trait for data sources.
//!
//! Data adapters accept an optional `FreshnessTracker` to report per-symbol
//! update timestamps. The main app implements this trait with its
//! `DataPlaneFreshness` type.

use std::fmt::Debug;

/// Identifies the upstream data source for freshness tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSource {
    BinanceSpot,
    BinanceKline,
    BinanceLob,
}

/// Trait for recording data freshness from adapters.
///
/// Implementors track per-(source, symbol) update timestamps and connection state.
pub trait FreshnessTracker: Send + Sync + Debug {
    /// Record that a data update was received for the given source and symbol.
    fn record_update(&self, source: DataSource, symbol: &str);

    /// Record a source connection state change.
    fn set_source_connected(&self, source: DataSource, connected: bool);

    /// Set the subscription count for a source.
    fn set_subscription_count(&self, source: DataSource, count: u64);
}
