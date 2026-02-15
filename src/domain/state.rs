use serde::{Deserialize, Serialize};
use std::fmt;

/// Strategy state machine states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrategyState {
    /// Waiting for a round to start
    Idle,
    /// Round active, watching for dump signal within window
    WatchWindow,
    /// Leg1 order submitted, waiting for fill
    Leg1Pending,
    /// Leg1 filled, watching for Leg2 opportunity
    Leg1Filled,
    /// Leg2 order submitted, waiting for fill
    Leg2Pending,
    /// Both legs filled, cycle complete
    CycleComplete,
    /// Cycle aborted (timeout, risk, or round end)
    Abort,
}

impl StrategyState {
    pub fn as_str(&self) -> &'static str {
        match self {
            StrategyState::Idle => "IDLE",
            StrategyState::WatchWindow => "WATCH_WINDOW",
            StrategyState::Leg1Pending => "LEG1_PENDING",
            StrategyState::Leg1Filled => "LEG1_FILLED",
            StrategyState::Leg2Pending => "LEG2_PENDING",
            StrategyState::CycleComplete => "CYCLE_COMPLETE",
            StrategyState::Abort => "ABORT",
        }
    }

    /// Check if this state can transition to another state
    pub fn can_transition_to(&self, target: StrategyState) -> bool {
        use StrategyState::*;

        match (self, target) {
            // From Idle
            (Idle, WatchWindow) => true,

            // From WatchWindow
            (WatchWindow, Leg1Pending) => true, // Dump detected
            (WatchWindow, Idle) => true,        // Window expired

            // From Leg1Pending
            (Leg1Pending, Leg1Filled) => true, // Order filled
            (Leg1Pending, Abort) => true,      // Timeout/cancel

            // From Leg1Filled
            (Leg1Filled, Leg2Pending) => true, // Leg2 opportunity
            (Leg1Filled, Abort) => true,       // Round ending

            // From Leg2Pending
            (Leg2Pending, CycleComplete) => true, // Order filled
            (Leg2Pending, Abort) => true,         // Timeout/round end

            // From CycleComplete
            (CycleComplete, Idle) => true, // Settlement done

            // From Abort
            (Abort, Idle) => true, // Cleanup done

            // All other transitions are invalid
            _ => false,
        }
    }

    /// Get valid next states from current state
    pub fn valid_transitions(&self) -> Vec<StrategyState> {
        use StrategyState::*;

        match self {
            Idle => vec![WatchWindow],
            WatchWindow => vec![Leg1Pending, Idle],
            Leg1Pending => vec![Leg1Filled, Abort],
            Leg1Filled => vec![Leg2Pending, Abort],
            Leg2Pending => vec![CycleComplete, Abort],
            CycleComplete => vec![Idle],
            Abort => vec![Idle],
        }
    }

    /// Is this state in the middle of an active cycle?
    pub fn is_in_cycle(&self) -> bool {
        matches!(
            self,
            StrategyState::Leg1Pending
                | StrategyState::Leg1Filled
                | StrategyState::Leg2Pending
                | StrategyState::CycleComplete
        )
    }

    /// Does this state imply open exposure / pending execution that should be aborted on round end?
    ///
    /// Note: `CycleComplete` is intentionally excluded. A completed cycle should be
    /// cleaned up, not aborted, even if the round has ended.
    pub fn requires_abort_on_round_end(&self) -> bool {
        matches!(
            self,
            StrategyState::Leg1Pending | StrategyState::Leg1Filled | StrategyState::Leg2Pending
        )
    }

    /// Does this state require immediate attention (pending orders)?
    pub fn has_pending_order(&self) -> bool {
        matches!(
            self,
            StrategyState::Leg1Pending | StrategyState::Leg2Pending
        )
    }

    /// Is this a terminal state for the current cycle?
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            StrategyState::CycleComplete | StrategyState::Abort | StrategyState::Idle
        )
    }
}

impl fmt::Display for StrategyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl TryFrom<&str> for StrategyState {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s.to_uppercase().as_str() {
            "IDLE" => Ok(StrategyState::Idle),
            "WATCH_WINDOW" => Ok(StrategyState::WatchWindow),
            "LEG1_PENDING" => Ok(StrategyState::Leg1Pending),
            "LEG1_FILLED" => Ok(StrategyState::Leg1Filled),
            "LEG2_PENDING" => Ok(StrategyState::Leg2Pending),
            "CYCLE_COMPLETE" => Ok(StrategyState::CycleComplete),
            "ABORT" => Ok(StrategyState::Abort),
            _ => Err(format!("Unknown state: {}", s)),
        }
    }
}

/// State transition event (for logging/debugging)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: StrategyState,
    pub to: StrategyState,
    pub reason: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl StateTransition {
    pub fn new(from: StrategyState, to: StrategyState, reason: impl Into<String>) -> Self {
        Self {
            from,
            to,
            reason: reason.into(),
            timestamp: chrono::Utc::now(),
        }
    }
}

/// Risk state for circuit breakers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskState {
    /// Normal operation
    Normal,
    /// Elevated risk, reduced position sizes
    Elevated,
    /// Trading halted due to risk limits
    Halted,
}

impl RiskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskState::Normal => "NORMAL",
            RiskState::Elevated => "ELEVATED",
            RiskState::Halted => "HALTED",
        }
    }

    pub fn can_open_new_cycle(&self) -> bool {
        matches!(self, RiskState::Normal | RiskState::Elevated)
    }

    pub fn can_trade(&self) -> bool {
        !matches!(self, RiskState::Halted)
    }
}

impl fmt::Display for RiskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        use StrategyState::*;

        // Valid transitions
        assert!(Idle.can_transition_to(WatchWindow));
        assert!(WatchWindow.can_transition_to(Leg1Pending));
        assert!(WatchWindow.can_transition_to(Idle));
        assert!(Leg1Pending.can_transition_to(Leg1Filled));
        assert!(Leg1Filled.can_transition_to(Leg2Pending));
        assert!(Leg2Pending.can_transition_to(CycleComplete));
        assert!(CycleComplete.can_transition_to(Idle));
        assert!(Abort.can_transition_to(Idle));

        // Invalid transitions
        assert!(!Idle.can_transition_to(Leg1Filled));
        assert!(!WatchWindow.can_transition_to(Leg2Pending));
        assert!(!Leg1Filled.can_transition_to(WatchWindow));
    }

    #[test]
    fn test_state_from_str() {
        assert_eq!(
            StrategyState::try_from("IDLE").unwrap(),
            StrategyState::Idle
        );
        assert_eq!(
            StrategyState::try_from("leg1_filled").unwrap(),
            StrategyState::Leg1Filled
        );
        assert!(StrategyState::try_from("INVALID").is_err());
    }

    #[test]
    fn test_is_in_cycle() {
        assert!(!StrategyState::Idle.is_in_cycle());
        assert!(!StrategyState::WatchWindow.is_in_cycle());
        assert!(StrategyState::Leg1Pending.is_in_cycle());
        assert!(StrategyState::Leg1Filled.is_in_cycle());
        assert!(StrategyState::Leg2Pending.is_in_cycle());
        assert!(StrategyState::CycleComplete.is_in_cycle());
        assert!(!StrategyState::Abort.is_in_cycle());
    }

    #[test]
    fn test_requires_abort_on_round_end() {
        assert!(!StrategyState::Idle.requires_abort_on_round_end());
        assert!(!StrategyState::WatchWindow.requires_abort_on_round_end());
        assert!(StrategyState::Leg1Pending.requires_abort_on_round_end());
        assert!(StrategyState::Leg1Filled.requires_abort_on_round_end());
        assert!(StrategyState::Leg2Pending.requires_abort_on_round_end());
        assert!(!StrategyState::CycleComplete.requires_abort_on_round_end());
        assert!(!StrategyState::Abort.requires_abort_on_round_end());
    }
}
