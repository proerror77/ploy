//! Abstraction over external data sources that drive EventEdge probability estimates.
//!
//! The `EventDataSource` trait decouples the core scan logic from the specific
//! data provider (Arena text leaderboard, future alternatives, etc.).

use crate::error::Result;
use crate::strategy::event_models::arena_text::{fetch_arena_text_snapshot, ArenaTextSnapshot};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::HashMap;

/// A point-in-time snapshot from an external event data source.
#[derive(Debug, Clone)]
pub struct EventSnapshot {
    pub last_updated: Option<NaiveDate>,
    pub fetched_at: DateTime<Utc>,
    /// Best score per organization (used for probability estimation).
    pub scores: HashMap<String, i32>,
    /// The raw Arena snapshot (if the source is Arena-based).
    pub arena: Option<ArenaTextSnapshot>,
}

/// Trait for fetching external event data that drives probability estimates.
#[async_trait]
pub trait EventDataSource: Send + Sync {
    async fn fetch_snapshot(&self) -> Result<EventSnapshot>;

    fn has_changed(&self, snapshot: &EventSnapshot, last_seen: &Option<NaiveDate>) -> bool {
        match (snapshot.last_updated, last_seen) {
            (Some(current), Some(prev)) => current != *prev,
            (Some(_), None) => true,
            (None, _) => true,
        }
    }
}

/// Arena text leaderboard data source (wraps `fetch_arena_text_snapshot`).
pub struct ArenaTextSource {
    pub softmax_temp: f64,
}

impl Default for ArenaTextSource {
    fn default() -> Self {
        Self { softmax_temp: 20.0 }
    }
}

#[async_trait]
impl EventDataSource for ArenaTextSource {
    async fn fetch_snapshot(&self) -> Result<EventSnapshot> {
        let arena = fetch_arena_text_snapshot().await?;
        let scores = arena.best_score_by_org();
        Ok(EventSnapshot {
            last_updated: arena.last_updated,
            fetched_at: arena.fetched_at,
            scores,
            arena: Some(arena),
        })
    }
}
