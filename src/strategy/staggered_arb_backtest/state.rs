use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::strategy::momentum::Direction;

/// Position lifecycle:
///   Idle → Leg1Filled → Settled (via merge or single-leg settlement)
///                     → Aborted (timeout / stop_loss / time_safety)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArbPositionState {
    Leg1Filled,
    Settled,
    Aborted,
}

impl fmt::Display for ArbPositionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Leg1Filled => write!(f, "Leg1Filled"),
            Self::Settled => write!(f, "Settled"),
            Self::Aborted => write!(f, "Aborted"),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StaggeredArbPosition {
    pub(super) symbol: String,
    pub(super) event_slug: String,
    /// Direction of Leg1 (the side we bought first)
    pub(super) leg1_direction: Direction,
    pub(super) leg1_price: Decimal,
    pub(super) leg1_shares: u64,
    pub(super) leg1_time: DateTime<Utc>,
    pub(super) leg1_fee: Decimal,
    /// Deadline for Leg2 fill
    pub(super) wait_deadline: DateTime<Utc>,
    /// Window open price (S0)
    pub(super) s0: Decimal,
    /// Event end time
    pub(super) event_end_time: DateTime<Utc>,
    /// Window duration in seconds
    pub(super) window_duration_secs: i64,
    /// Model probability at Leg1 entry
    pub(super) entry_p_hat: f64,
    /// Realized vol at entry
    pub(super) entry_sigma: f64,
    /// Best sum seen during monitoring (for diagnostics)
    pub(super) best_sum_seen: Decimal,
    /// Initial sum at entry (up_ask + down_ask)
    pub(super) initial_sum: Decimal,
    /// Current state
    pub(super) state: ArbPositionState,
    // Leg2 (filled after monitoring)
    pub(super) leg2_direction: Option<Direction>,
    pub(super) leg2_price: Option<Decimal>,
    pub(super) leg2_shares: Option<u64>,
    pub(super) leg2_time: Option<DateTime<Utc>>,
    pub(super) leg2_fee: Option<Decimal>,
    // Resolution
    pub(super) exit_reason: Option<String>,
    pub(super) pnl: Option<Decimal>,
}

#[derive(Debug, Clone)]
pub(super) struct ActiveWindowInfo {
    pub(super) event_slug: String,
    pub(super) s0: Decimal,
    pub(super) end_time: DateTime<Utc>,
    /// Window duration in seconds (300 = 5m, 900 = 15m)
    pub(super) window_duration_secs: i64,
}
