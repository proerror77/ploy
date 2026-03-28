//! Live data feed backed by a tokio broadcast channel.
//!
//! Used for both dry-run and live trading. The feed blocks on
//! `recv()` until the next market update arrives from the WebSocket
//! adapters.

use async_trait::async_trait;
use tokio::sync::broadcast;
use tracing::warn;

use crate::traits::{Feed, MarketUpdate};

/// Live feed that consumes market updates from a broadcast channel.
///
/// Multiple strategies can subscribe to the same broadcast sender.
/// Lagged messages are skipped with a warning.
pub struct LiveFeed {
    rx: broadcast::Receiver<MarketUpdate>,
}

impl LiveFeed {
    /// Create a live feed from a broadcast receiver.
    pub fn new(rx: broadcast::Receiver<MarketUpdate>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl Feed for LiveFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        loop {
            match self.rx.recv().await {
                Ok(update) => return Some(update),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "LiveFeed lagged, skipping messages");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
