//! Action Space
//!
//! Defines discrete and continuous action spaces for RL agents.

use serde::{Deserialize, Serialize};

/// Number of discrete actions
pub const NUM_DISCRETE_ACTIONS: usize = 5;

/// Dimension of continuous action space
pub const CONTINUOUS_ACTION_DIM: usize = 5;

/// Discrete action space for DQN-style agents
///
/// Simple, interpretable actions that map directly to trading decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum DiscreteAction {
    /// Do nothing, maintain current state
    Hold = 0,
    /// Buy UP tokens
    BuyUp = 1,
    /// Buy DOWN tokens
    BuyDown = 2,
    /// Exit current position (sell all)
    SellPosition = 3,
    /// Enter hedge position (buy both sides for split-arb)
    EnterHedge = 4,
}

impl DiscreteAction {
    /// Convert from action index
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Hold),
            1 => Some(Self::BuyUp),
            2 => Some(Self::BuyDown),
            3 => Some(Self::SellPosition),
            4 => Some(Self::EnterHedge),
            _ => None,
        }
    }

    /// Convert to action index
    pub fn to_index(self) -> usize {
        self as usize
    }

    /// Get all possible actions
    pub fn all() -> &'static [DiscreteAction] {
        &[
            Self::Hold,
            Self::BuyUp,
            Self::BuyDown,
            Self::SellPosition,
            Self::EnterHedge,
        ]
    }

    /// Check if this is a buy action
    pub fn is_buy(&self) -> bool {
        matches!(self, Self::BuyUp | Self::BuyDown | Self::EnterHedge)
    }

    /// Check if this is a sell action
    pub fn is_sell(&self) -> bool {
        matches!(self, Self::SellPosition)
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            Self::Hold => "Hold current position",
            Self::BuyUp => "Buy UP tokens",
            Self::BuyDown => "Buy DOWN tokens",
            Self::SellPosition => "Sell/exit position",
            Self::EnterHedge => "Enter hedge (split-arb)",
        }
    }
}

impl Default for DiscreteAction {
    fn default() -> Self {
        Self::Hold
    }
}

/// Continuous action space for PPO-style agents
///
/// Provides fine-grained control over trading parameters.
/// All values are in range [-1, 1] or [0, 1] and must be
/// scaled to actual trading parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContinuousAction {
    /// Position size delta: -1 (full sell) to +1 (full buy)
    /// Scaled by max position size to get actual shares
    pub position_delta: f32,

    /// Side preference: -1 (DOWN) to +1 (UP)
    /// 0 means no preference (use market signals)
    pub side_preference: f32,

    /// Urgency: 0 (patient, use limit orders) to 1 (aggressive, use market orders)
    /// Controls order type and price aggressiveness
    pub urgency: f32,

    /// Take profit adjustment: -1 (tighter) to +1 (looser)
    /// Modifies the base take profit threshold
    pub tp_adjustment: f32,

    /// Stop loss adjustment: -1 (tighter) to +1 (looser)
    /// Modifies the base stop loss threshold
    pub sl_adjustment: f32,
}

impl Default for ContinuousAction {
    fn default() -> Self {
        Self {
            position_delta: 0.0,
            side_preference: 0.0,
            urgency: 0.5,
            tp_adjustment: 0.0,
            sl_adjustment: 0.0,
        }
    }
}

impl ContinuousAction {
    /// Create a new continuous action
    pub fn new(
        position_delta: f32,
        side_preference: f32,
        urgency: f32,
        tp_adjustment: f32,
        sl_adjustment: f32,
    ) -> Self {
        Self {
            position_delta: position_delta.clamp(-1.0, 1.0),
            side_preference: side_preference.clamp(-1.0, 1.0),
            urgency: urgency.clamp(0.0, 1.0),
            tp_adjustment: tp_adjustment.clamp(-1.0, 1.0),
            sl_adjustment: sl_adjustment.clamp(-1.0, 1.0),
        }
    }

    /// Create from a raw tensor output (Vec of f32)
    pub fn from_tensor(values: &[f32]) -> Self {
        assert!(values.len() >= CONTINUOUS_ACTION_DIM,
            "Expected {} values, got {}", CONTINUOUS_ACTION_DIM, values.len());

        Self::new(
            values[0],
            values[1],
            values[2],
            values[3],
            values[4],
        )
    }

    /// Convert to tensor representation
    pub fn to_tensor(&self) -> [f32; CONTINUOUS_ACTION_DIM] {
        [
            self.position_delta,
            self.side_preference,
            self.urgency,
            self.tp_adjustment,
            self.sl_adjustment,
        ]
    }

    /// Get the implied discrete action from continuous values
    ///
    /// Useful for interpreting continuous actions in discrete terms.
    pub fn to_discrete(&self) -> DiscreteAction {
        // Strong sell signal
        if self.position_delta < -0.5 {
            return DiscreteAction::SellPosition;
        }

        // Strong buy signal
        if self.position_delta > 0.5 {
            if self.side_preference > 0.3 {
                return DiscreteAction::BuyUp;
            } else if self.side_preference < -0.3 {
                return DiscreteAction::BuyDown;
            } else {
                // No strong preference, might be a hedge
                return DiscreteAction::EnterHedge;
            }
        }

        // Weak signal, hold
        DiscreteAction::Hold
    }

    /// Check if this represents an aggressive action
    pub fn is_aggressive(&self) -> bool {
        self.urgency > 0.7
    }

    /// Get position size as percentage of max
    pub fn position_size_pct(&self) -> f32 {
        self.position_delta.abs()
    }
}

/// Hybrid action combining discrete choice with continuous parameters
///
/// Useful for structured exploration or when some actions
/// need continuous parameters (like sizing).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HybridAction {
    /// The discrete action to take
    pub action: DiscreteAction,
    /// Position size as fraction of max (0.0 to 1.0)
    pub size: f32,
    /// Price aggressiveness (0.0 = passive, 1.0 = aggressive)
    pub aggressiveness: f32,
}

impl Default for HybridAction {
    fn default() -> Self {
        Self {
            action: DiscreteAction::Hold,
            size: 0.0,
            aggressiveness: 0.5,
        }
    }
}

impl HybridAction {
    /// Create a new hybrid action
    pub fn new(action: DiscreteAction, size: f32, aggressiveness: f32) -> Self {
        Self {
            action,
            size: size.clamp(0.0, 1.0),
            aggressiveness: aggressiveness.clamp(0.0, 1.0),
        }
    }

    /// Create a hold action
    pub fn hold() -> Self {
        Self::default()
    }

    /// Create a buy action
    pub fn buy(side_is_up: bool, size: f32, aggressiveness: f32) -> Self {
        Self::new(
            if side_is_up { DiscreteAction::BuyUp } else { DiscreteAction::BuyDown },
            size,
            aggressiveness,
        )
    }

    /// Create a sell action
    pub fn sell(aggressiveness: f32) -> Self {
        Self::new(DiscreteAction::SellPosition, 1.0, aggressiveness)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discrete_action_roundtrip() {
        for action in DiscreteAction::all() {
            let index = action.to_index();
            let recovered = DiscreteAction::from_index(index).unwrap();
            assert_eq!(*action, recovered);
        }
    }

    #[test]
    fn test_continuous_action_clamping() {
        let action = ContinuousAction::new(2.0, -2.0, 1.5, 0.5, -0.5);
        assert_eq!(action.position_delta, 1.0);
        assert_eq!(action.side_preference, -1.0);
        assert_eq!(action.urgency, 1.0);
    }

    #[test]
    fn test_continuous_to_discrete() {
        // Strong buy UP
        let action = ContinuousAction::new(0.8, 0.7, 0.5, 0.0, 0.0);
        assert_eq!(action.to_discrete(), DiscreteAction::BuyUp);

        // Strong sell
        let action = ContinuousAction::new(-0.8, 0.0, 0.5, 0.0, 0.0);
        assert_eq!(action.to_discrete(), DiscreteAction::SellPosition);

        // Weak signal -> hold
        let action = ContinuousAction::new(0.2, 0.1, 0.5, 0.0, 0.0);
        assert_eq!(action.to_discrete(), DiscreteAction::Hold);
    }

    #[test]
    fn test_tensor_roundtrip() {
        let action = ContinuousAction::new(0.5, -0.3, 0.8, 0.1, -0.2);
        let tensor = action.to_tensor();
        let recovered = ContinuousAction::from_tensor(&tensor);
        assert_eq!(action, recovered);
    }
}
