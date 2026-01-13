//! NBA Swing Strategy State Machine
//!
//! Manages the lifecycle of a swing trading position:
//! WATCH → ARMED → ENTERED → MANAGING → EXITED → (back to WATCH)
//!                    ↓
//!                  HALT (emergency stop)
//!
//! Each state has specific allowed actions and transitions.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Strategy state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrategyState {
    /// Watching markets, no position
    /// Allowed: detect signals
    Watch,
    
    /// Signal detected, ready to enter
    /// Allowed: submit entry order
    Armed,
    
    /// Entry order submitted, waiting for fill
    /// Allowed: wait for fill, cancel if signal lost
    Entering,
    
    /// Position opened, actively managing
    /// Allowed: monitor exit conditions, partial exits
    Managing,
    
    /// Exit order submitted, waiting for fill
    /// Allowed: wait for fill
    Exiting,
    
    /// Position closed, analyzing results
    /// Allowed: record PnL, reset to Watch
    Exited,
    
    /// Emergency halt (data issues, risk limits, manual stop)
    /// Allowed: close positions, investigate issues
    Halt,
}

impl fmt::Display for StrategyState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Watch => write!(f, "WATCH"),
            Self::Armed => write!(f, "ARMED"),
            Self::Entering => write!(f, "ENTERING"),
            Self::Managing => write!(f, "MANAGING"),
            Self::Exiting => write!(f, "EXITING"),
            Self::Exited => write!(f, "EXITED"),
            Self::Halt => write!(f, "HALT"),
        }
    }
}

/// State transition events
#[derive(Debug, Clone)]
pub enum StateEvent {
    // Watch → Armed
    SignalDetected,
    
    // Armed → Entering
    EntryOrderSubmitted,
    
    // Armed → Watch (signal lost before entry)
    SignalLost,
    
    // Entering → Managing
    EntryFilled,
    
    // Entering → Watch (entry failed/cancelled)
    EntryCancelled,
    
    // Managing → Exiting
    ExitSignal,
    
    // Exiting → Exited
    ExitFilled,
    
    // Exited → Watch (ready for next trade)
    Reset,
    
    // Any → Halt (emergency)
    EmergencyHalt(String),
    
    // Halt → Watch (resume after fixing issues)
    Resume,
}

/// State machine
pub struct StateMachine {
    current_state: StrategyState,
    halt_reasons: Vec<String>,
    transition_history: Vec<StateTransition>,
}

/// State transition record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: StrategyState,
    pub to: StrategyState,
    pub event: String,
    pub timestamp: i64,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            current_state: StrategyState::Watch,
            halt_reasons: vec![],
            transition_history: vec![],
        }
    }
    
    /// Attempt state transition
    pub fn transition(&mut self, event: StateEvent) -> Result<StrategyState, String> {
        let from = self.current_state;
        
        let to = match (&self.current_state, &event) {
            // Watch → Armed
            (StrategyState::Watch, StateEvent::SignalDetected) => StrategyState::Armed,
            
            // Armed → Entering
            (StrategyState::Armed, StateEvent::EntryOrderSubmitted) => StrategyState::Entering,
            
            // Armed → Watch (signal lost)
            (StrategyState::Armed, StateEvent::SignalLost) => StrategyState::Watch,
            
            // Entering → Managing
            (StrategyState::Entering, StateEvent::EntryFilled) => StrategyState::Managing,
            
            // Entering → Watch (entry failed)
            (StrategyState::Entering, StateEvent::EntryCancelled) => StrategyState::Watch,
            
            // Managing → Exiting
            (StrategyState::Managing, StateEvent::ExitSignal) => StrategyState::Exiting,
            
            // Exiting → Exited
            (StrategyState::Exiting, StateEvent::ExitFilled) => StrategyState::Exited,
            
            // Exited → Watch
            (StrategyState::Exited, StateEvent::Reset) => StrategyState::Watch,
            
            // Any → Halt
            (_, StateEvent::EmergencyHalt(reason)) => {
                self.halt_reasons.push(reason.to_string());
                StrategyState::Halt
            },
            
            // Halt → Watch (resume)
            (StrategyState::Halt, StateEvent::Resume) => {
                self.halt_reasons.clear();
                StrategyState::Watch
            },
            
            // Invalid transition
            _ => {
                return Err(format!(
                    "Invalid transition: {} -> {:?}",
                    self.current_state,
                    event
                ));
            }
        };
        
        // Record transition
        self.transition_history.push(StateTransition {
            from,
            to,
            event: format!("{:?}", event),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
        
        self.current_state = to;
        Ok(to)
    }
    
    /// Get current state
    pub fn state(&self) -> StrategyState {
        self.current_state
    }
    
    /// Check if in specific state
    pub fn is_watch(&self) -> bool {
        self.current_state == StrategyState::Watch
    }
    
    pub fn is_armed(&self) -> bool {
        self.current_state == StrategyState::Armed
    }
    
    pub fn is_entering(&self) -> bool {
        self.current_state == StrategyState::Entering
    }
    
    pub fn is_managing(&self) -> bool {
        self.current_state == StrategyState::Managing
    }
    
    pub fn is_exiting(&self) -> bool {
        self.current_state == StrategyState::Exiting
    }
    
    pub fn is_exited(&self) -> bool {
        self.current_state == StrategyState::Exited
    }
    
    pub fn is_halt(&self) -> bool {
        self.current_state == StrategyState::Halt
    }
    
    /// Check if can enter position
    pub fn can_enter(&self) -> bool {
        matches!(self.current_state, StrategyState::Armed)
    }
    
    /// Check if has active position
    pub fn has_position(&self) -> bool {
        matches!(
            self.current_state,
            StrategyState::Entering | StrategyState::Managing | StrategyState::Exiting
        )
    }
    
    /// Get halt reasons
    pub fn halt_reasons(&self) -> &[String] {
        &self.halt_reasons
    }
    
    /// Get transition history
    pub fn history(&self) -> &[StateTransition] {
        &self.transition_history
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_normal_flow() {
        let mut sm = StateMachine::new();
        
        // Watch → Armed
        assert_eq!(sm.state(), StrategyState::Watch);
        sm.transition(StateEvent::SignalDetected).unwrap();
        assert_eq!(sm.state(), StrategyState::Armed);
        
        // Armed → Entering
        sm.transition(StateEvent::EntryOrderSubmitted).unwrap();
        assert_eq!(sm.state(), StrategyState::Entering);
        
        // Entering → Managing
        sm.transition(StateEvent::EntryFilled).unwrap();
        assert_eq!(sm.state(), StrategyState::Managing);
        assert!(sm.has_position());
        
        // Managing → Exiting
        sm.transition(StateEvent::ExitSignal).unwrap();
        assert_eq!(sm.state(), StrategyState::Exiting);
        
        // Exiting → Exited
        sm.transition(StateEvent::ExitFilled).unwrap();
        assert_eq!(sm.state(), StrategyState::Exited);
        assert!(!sm.has_position());
        
        // Exited → Watch
        sm.transition(StateEvent::Reset).unwrap();
        assert_eq!(sm.state(), StrategyState::Watch);
    }
    
    #[test]
    fn test_signal_lost() {
        let mut sm = StateMachine::new();
        
        sm.transition(StateEvent::SignalDetected).unwrap();
        assert_eq!(sm.state(), StrategyState::Armed);
        
        // Signal lost before entry
        sm.transition(StateEvent::SignalLost).unwrap();
        assert_eq!(sm.state(), StrategyState::Watch);
    }
    
    #[test]
    fn test_entry_cancelled() {
        let mut sm = StateMachine::new();
        
        sm.transition(StateEvent::SignalDetected).unwrap();
        sm.transition(StateEvent::EntryOrderSubmitted).unwrap();
        assert_eq!(sm.state(), StrategyState::Entering);
        
        // Entry cancelled
        sm.transition(StateEvent::EntryCancelled).unwrap();
        assert_eq!(sm.state(), StrategyState::Watch);
    }
    
    #[test]
    fn test_emergency_halt() {
        let mut sm = StateMachine::new();
        
        sm.transition(StateEvent::SignalDetected).unwrap();
        sm.transition(StateEvent::EntryOrderSubmitted).unwrap();
        sm.transition(StateEvent::EntryFilled).unwrap();
        assert_eq!(sm.state(), StrategyState::Managing);
        
        // Emergency halt from any state
        sm.transition(StateEvent::EmergencyHalt("Data latency too high".to_string())).unwrap();
        assert_eq!(sm.state(), StrategyState::Halt);
        assert!(sm.is_halt());
        assert_eq!(sm.halt_reasons().len(), 1);
        
        // Resume
        sm.transition(StateEvent::Resume).unwrap();
        assert_eq!(sm.state(), StrategyState::Watch);
        assert_eq!(sm.halt_reasons().len(), 0);
    }
    
    #[test]
    fn test_invalid_transition() {
        let mut sm = StateMachine::new();
        
        // Can't go directly from Watch to Managing
        let result = sm.transition(StateEvent::ExitSignal);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_transition_history() {
        let mut sm = StateMachine::new();
        
        sm.transition(StateEvent::SignalDetected).unwrap();
        sm.transition(StateEvent::EntryOrderSubmitted).unwrap();
        sm.transition(StateEvent::EntryFilled).unwrap();
        
        let history = sm.history();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].from, StrategyState::Watch);
        assert_eq!(history[0].to, StrategyState::Armed);
        assert_eq!(history[2].to, StrategyState::Managing);
    }
}
