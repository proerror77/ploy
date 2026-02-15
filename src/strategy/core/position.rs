//! Position tracking for split arbitrage
//!
//! Tracks both partial (unhedged) and fully hedged positions.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Which side of the binary market
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArbSide {
    /// Yes/Up/Team A
    Yes,
    /// No/Down/Team B
    No,
}

impl ArbSide {
    /// Get the opposite side
    pub fn opposite(&self) -> Self {
        match self {
            ArbSide::Yes => ArbSide::No,
            ArbSide::No => ArbSide::Yes,
        }
    }
}

impl std::fmt::Display for ArbSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArbSide::Yes => write!(f, "YES"),
            ArbSide::No => write!(f, "NO"),
        }
    }
}

/// Status of a partial position
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionStatus {
    /// Waiting for hedge opportunity
    WaitingForHedge,
    /// Hedge order placed
    HedgePending,
    /// Fully hedged, profit locked
    Hedged,
    /// Exited without hedge (stopped out or timed out)
    ExitedUnhedged,
}

/// Tracks a partial position waiting for hedge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialPosition {
    /// Event/market identifier
    pub event_id: String,

    /// Market condition ID
    pub condition_id: String,

    /// Which side we bought first
    pub first_side: ArbSide,

    /// Token ID of first side
    pub first_token_id: String,

    /// Entry price of first side
    pub first_entry_price: Decimal,

    /// Shares bought
    pub shares: u64,

    /// When we entered
    pub entry_time: DateTime<Utc>,

    /// Event end time (for timeout)
    pub event_end_time: DateTime<Utc>,

    /// Token ID of the other side (for hedging)
    pub other_token_id: String,

    /// Current status
    pub status: PositionStatus,

    /// Maximum price we can pay for hedge to hit target profit
    pub max_hedge_price: Decimal,

    /// Human-readable labels for logging
    pub first_side_label: String,
    pub other_side_label: String,
}

/// A fully hedged position with locked profit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HedgedPosition {
    pub event_id: String,
    pub condition_id: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_entry_price: Decimal,
    pub no_entry_price: Decimal,
    pub total_cost: Decimal,
    pub locked_profit: Decimal,
    pub shares: u64,
    pub entry_time: DateTime<Utc>,
    pub hedge_time: DateTime<Utc>,
    pub event_end_time: DateTime<Utc>,
}

impl HedgedPosition {
    /// Calculate profit per share in dollars
    pub fn profit_per_share(&self) -> Decimal {
        self.locked_profit
    }

    /// Calculate total profit in dollars
    pub fn total_profit(&self) -> Decimal {
        self.locked_profit * Decimal::from(self.shares)
    }
}

/// Statistics for arbitrage tracking
#[derive(Debug, Default, Clone)]
pub struct ArbStats {
    pub signals_detected: u64,
    pub first_leg_entries: u64,
    pub hedges_completed: u64,
    pub unhedged_exits: u64,
    pub total_profit: Decimal,
    pub total_loss: Decimal,
}

impl ArbStats {
    /// Net P&L
    pub fn net_pnl(&self) -> Decimal {
        self.total_profit - self.total_loss
    }
}
