//! Event Registry — shared event pool for cross-strategy discovery.
//!
//! Implements the funnel:
//! ```text
//! DISCOVER (broad scan) → RESEARCH (evaluate) → MONITOR (watch) → TRADE (execute)
//!   1000 events          →  50 worth tracking  →  10 monitoring  →  1-2 trades
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt;

// =============================================================================
// EventStatus — state machine
// =============================================================================

/// Lifecycle status of a registered event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Discovered,
    Researched,
    Monitoring,
    Paused,
    Settled,
    Expired,
}

impl EventStatus {
    /// Valid next states from the current status.
    pub fn valid_transitions(self) -> &'static [EventStatus] {
        use EventStatus::*;
        match self {
            Discovered => &[Researched, Monitoring, Paused, Expired],
            Researched => &[Monitoring, Paused, Expired],
            Monitoring => &[Paused, Settled, Expired],
            Paused => &[Monitoring, Expired],
            Settled => &[],
            Expired => &[],
        }
    }

    /// Check whether transitioning to `next` is allowed.
    pub fn can_transition_to(self, next: EventStatus) -> bool {
        self.valid_transitions().contains(&next)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Researched => "researched",
            Self::Monitoring => "monitoring",
            Self::Paused => "paused",
            Self::Settled => "settled",
            Self::Expired => "expired",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "discovered" => Some(Self::Discovered),
            "researched" => Some(Self::Researched),
            "monitoring" => Some(Self::Monitoring),
            "paused" => Some(Self::Paused),
            "settled" => Some(Self::Settled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

impl fmt::Display for EventStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// =============================================================================
// RegisteredEvent — DB row
// =============================================================================

/// A single row from the `event_registry` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredEvent {
    pub id: i32,
    pub event_id: Option<String>,
    pub title: String,
    pub slug: Option<String>,
    pub source: String,
    pub domain: String,
    pub strategy_hint: Option<String>,
    pub status: String,
    pub confidence: Option<f64>,
    pub settlement_rule: Option<String>,
    pub end_time: Option<DateTime<Utc>>,
    pub market_slug: Option<String>,
    pub condition_id: Option<String>,
    pub token_ids: Option<JsonValue>,
    pub outcome_prices: Option<JsonValue>,
    pub metadata: JsonValue,
    pub last_scanned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RegisteredEvent {
    /// Parse the `status` column into a typed enum.
    pub fn parsed_status(&self) -> Option<EventStatus> {
        EventStatus::from_str(&self.status)
    }
}

// =============================================================================
// EventFilter — query builder
// =============================================================================

/// Filter criteria for listing events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    pub status: Option<String>,
    pub domain: Option<String>,
    pub strategy_hint: Option<String>,
    pub source: Option<String>,
    pub limit: Option<i64>,
}

// =============================================================================
// EventUpsertRequest — insert/update payload
// =============================================================================

/// Payload for inserting or updating an event in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventUpsertRequest {
    pub title: String,
    pub source: String,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default = "default_domain")]
    pub domain: String,
    #[serde(default)]
    pub strategy_hint: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub settlement_rule: Option<String>,
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub market_slug: Option<String>,
    #[serde(default)]
    pub condition_id: Option<String>,
    #[serde(default)]
    pub token_ids: Option<JsonValue>,
    #[serde(default)]
    pub outcome_prices: Option<JsonValue>,
    #[serde(default)]
    pub metadata: Option<JsonValue>,
}

fn default_domain() -> String {
    "politics".to_string()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_display_roundtrip() {
        let statuses = [
            EventStatus::Discovered,
            EventStatus::Researched,
            EventStatus::Monitoring,
            EventStatus::Paused,
            EventStatus::Settled,
            EventStatus::Expired,
        ];
        for s in statuses {
            let text = s.as_str();
            let parsed = EventStatus::from_str(text).expect("should parse");
            assert_eq!(s, parsed);
        }
    }

    #[test]
    fn test_valid_transitions() {
        use EventStatus::*;

        // discovered → researched ✓
        assert!(Discovered.can_transition_to(Researched));
        // discovered → monitoring ✓ (skip research)
        assert!(Discovered.can_transition_to(Monitoring));
        // discovered → settled ✗
        assert!(!Discovered.can_transition_to(Settled));

        // monitoring → paused ✓
        assert!(Monitoring.can_transition_to(Paused));
        // monitoring → settled ✓
        assert!(Monitoring.can_transition_to(Settled));

        // paused → monitoring ✓ (resume)
        assert!(Paused.can_transition_to(Monitoring));
        // paused → researched ✗ (can't go backwards)
        assert!(!Paused.can_transition_to(Researched));

        // terminal states have no transitions
        assert!(Settled.valid_transitions().is_empty());
        assert!(Expired.valid_transitions().is_empty());
    }

    #[test]
    fn test_invalid_status_string() {
        assert!(EventStatus::from_str("invalid").is_none());
        assert!(EventStatus::from_str("").is_none());
    }

    #[test]
    fn test_event_filter_default() {
        let f = EventFilter::default();
        assert!(f.status.is_none());
        assert!(f.domain.is_none());
        assert!(f.strategy_hint.is_none());
        assert!(f.source.is_none());
        assert!(f.limit.is_none());
    }
}
