//! Data feed implementations.

#[cfg(feature = "database")]
pub mod database;
mod historical;
mod live;

#[cfg(feature = "database")]
pub use database::load_from_database;
pub use historical::HistoricalFeed;
pub use live::LiveFeed;
