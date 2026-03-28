//! Historical data feed for backtesting.
//!
//! Replays pre-loaded market updates in timestamp order.
//! Deterministic: same data → same sequence every run.

use std::collections::VecDeque;

use async_trait::async_trait;

use crate::traits::{Feed, MarketUpdate};

/// Replay feed from pre-loaded market updates.
///
/// Updates are consumed in order via `pop_front`. The feed is exhausted
/// when all updates have been consumed (returns `None`).
pub struct HistoricalFeed {
    updates: VecDeque<MarketUpdate>,
}

impl HistoricalFeed {
    /// Create a feed from a pre-sorted vector of market updates.
    ///
    /// Caller is responsible for sorting by timestamp before constructing.
    pub fn new(updates: Vec<MarketUpdate>) -> Self {
        Self {
            updates: VecDeque::from(updates),
        }
    }

    /// Number of remaining updates.
    pub fn remaining(&self) -> usize {
        self.updates.len()
    }
}

#[async_trait]
impl Feed for HistoricalFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        self.updates.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn exhausts_all_updates() {
        let updates = vec![
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: Utc::now(),
            },
            MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100100),
                ts: Utc::now(),
            },
        ];
        let mut feed = HistoricalFeed::new(updates);

        assert!(feed.next().await.is_some());
        assert!(feed.next().await.is_some());
        assert!(feed.next().await.is_none());
        assert_eq!(feed.remaining(), 0);
    }
}
