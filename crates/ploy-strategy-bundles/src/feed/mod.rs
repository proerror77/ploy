//! Data feed implementations.

mod historical;
mod live;

pub use historical::HistoricalFeed;
pub use live::LiveFeed;
