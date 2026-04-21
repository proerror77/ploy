//! Data feed implementations.

#[cfg(feature = "db-feed")]
pub mod database;
mod historical;
mod live;
mod options;
#[cfg(feature = "parquet-feed")]
pub mod parquet;
#[cfg(feature = "parquet-feed")]
pub mod parquet_stream;
mod recorded;

#[cfg(feature = "db-feed")]
pub use database::{load_from_database, load_from_database_with_options};
pub use historical::HistoricalFeed;
pub use live::LiveFeed;
pub use options::HistoricalLoadOptions;
#[cfg(feature = "parquet-feed")]
pub use parquet_stream::StreamingParquetFeed;
pub use recorded::{RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed};
