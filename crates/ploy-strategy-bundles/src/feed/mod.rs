//! Data feed implementations.

pub mod database;
mod historical;
mod live;
mod recorded;

pub use database::load_from_database;
pub use historical::HistoricalFeed;
pub use live::LiveFeed;
pub use recorded::{RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed};
