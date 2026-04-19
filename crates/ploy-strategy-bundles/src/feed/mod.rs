//! Data feed implementations.

pub mod database;
mod historical;
mod live;
#[cfg(feature = "parquet-feed")]
pub mod parquet;
pub mod parquet_stream;
mod recorded;

pub use database::{HistoricalLoadOptions, load_from_database, load_from_database_with_options};
pub use historical::HistoricalFeed;
pub use live::LiveFeed;
pub use parquet_stream::StreamingParquetFeed;
pub use recorded::{RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed};
