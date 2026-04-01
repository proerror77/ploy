//! Data feed implementations.

pub mod database;
mod historical;
mod live;

pub use database::load_from_database;
pub use historical::HistoricalFeed;
pub use live::LiveFeed;
