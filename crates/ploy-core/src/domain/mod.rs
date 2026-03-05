//! Core domain types for the ploy trading system.
//!
//! These are fundamental enums and value types used across the entire codebase.
//! They intentionally have no heavy dependencies (only serde, rust_decimal, chrono).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Market side (binary market: Up / Down)
// ---------------------------------------------------------------------------

/// Side of a binary market (UP or DOWN).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Up,
    Down,
}

impl Side {
    /// Get the opposite side.
    pub fn opposite(&self) -> Self {
        match self {
            Side::Up => Side::Down,
            Side::Down => Side::Up,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Up => "UP",
            Side::Down => "DOWN",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Order side (Buy / Sell)
// ---------------------------------------------------------------------------

/// Order side (buy or sell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "BUY"),
            OrderSide::Sell => write!(f, "SELL"),
        }
    }
}

// ---------------------------------------------------------------------------
// Order type
// ---------------------------------------------------------------------------

/// Order type (limit or market).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType {
    Limit,
    Market,
}

// ---------------------------------------------------------------------------
// Time in force
// ---------------------------------------------------------------------------

/// Time-in-force policy for an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good Till Cancelled
    GTC,
    /// Fill Or Kill
    FOK,
    /// Immediate Or Cancel
    IOC,
}

// ---------------------------------------------------------------------------
// Order status
// ---------------------------------------------------------------------------

/// Order lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderStatus {
    /// Order created but not yet submitted
    Pending,
    /// Order submitted to exchange
    Submitted,
    /// Order partially filled
    PartiallyFilled,
    /// Order fully filled
    Filled,
    /// Order cancelled
    Cancelled,
    /// Order rejected by exchange
    Rejected,
    /// Order expired
    Expired,
    /// Order failed (internal error)
    Failed,
}

impl OrderStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled
        )
    }
}

// ---------------------------------------------------------------------------
// Domain (trading domain)
// ---------------------------------------------------------------------------

/// Trading domain classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Domain {
    /// Sports events (NBA, NFL, etc.)
    Sports,
    /// Cryptocurrency (BTC, ETH, SOL 15-min rounds)
    Crypto,
    /// Political events
    Politics,
    /// Economic indicators
    Economics,
    /// Custom domain
    Custom(u32),
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Domain::Sports => write!(f, "Sports"),
            Domain::Crypto => write!(f, "Crypto"),
            Domain::Politics => write!(f, "Politics"),
            Domain::Economics => write!(f, "Economics"),
            Domain::Custom(id) => write!(f, "Custom({})", id),
        }
    }
}

impl FromStr for Domain {
    type Err = &'static str;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err("domain is empty");
        }

        if let Some(custom) = normalized.strip_prefix("custom:") {
            let id = custom
                .trim()
                .parse::<u32>()
                .map_err(|_| "custom domain id must be a non-negative integer")?;
            return Ok(Domain::Custom(id));
        }

        match normalized.as_str() {
            "crypto" => Ok(Domain::Crypto),
            "sports" => Ok(Domain::Sports),
            "politics" => Ok(Domain::Politics),
            "economics" => Ok(Domain::Economics),
            _ => Err("invalid domain; expected crypto|sports|politics|economics|custom:<id>"),
        }
    }
}

impl<'de> Deserialize<'de> for Domain {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct DomainVisitor;

        impl<'de> de::Visitor<'de> for DomainVisitor {
            type Value = Domain;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a domain string like \"crypto\" or \"custom:42\"")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Domain, E> {
                Domain::from_str(v).map_err(de::Error::custom)
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Domain, A::Error> {
                if let Some(key) = map.next_key::<String>()? {
                    if key.eq_ignore_ascii_case("custom") {
                        let id: u32 = map.next_value()?;
                        return Ok(Domain::Custom(id));
                    }
                    return Domain::from_str(&key).map_err(de::Error::custom);
                }
                Err(de::Error::custom("empty map for Domain"))
            }
        }

        deserializer.deserialize_any(DomainVisitor)
    }
}

impl Domain {
    pub fn parse_optional(raw: Option<&str>, default: Domain) -> std::result::Result<Self, String> {
        match raw {
            None => Ok(default),
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Ok(default)
                } else {
                    Self::from_str(trimmed).map_err(|e| e.to_string())
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Timeframe
// ---------------------------------------------------------------------------

/// Timeframe for deployment / intent routing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timeframe {
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    Other(String),
}

impl Timeframe {
    pub fn as_str(&self) -> &str {
        match self {
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::Other(v) => v.as_str(),
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Strategy state
// ---------------------------------------------------------------------------

/// Strategy state machine states.
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

    /// Check if this state can transition to another state.
    pub fn can_transition_to(&self, target: StrategyState) -> bool {
        use StrategyState::*;
        matches!(
            (self, target),
            (Idle, WatchWindow)
                | (WatchWindow, Leg1Pending)
                | (WatchWindow, Idle)
                | (Leg1Pending, Leg1Filled)
                | (Leg1Pending, Abort)
                | (Leg1Filled, Leg2Pending)
                | (Leg1Filled, Abort)
                | (Leg2Pending, CycleComplete)
                | (Leg2Pending, Abort)
                | (CycleComplete, Idle)
                | (Abort, Idle)
        )
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

    /// Does this state imply open exposure that should be aborted on round end?
    pub fn requires_abort_on_round_end(&self) -> bool {
        matches!(
            self,
            StrategyState::Leg1Pending | StrategyState::Leg1Filled | StrategyState::Leg2Pending
        )
    }

    /// Does this state have a pending order?
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

// ---------------------------------------------------------------------------
// Risk state
// ---------------------------------------------------------------------------

/// Risk state for circuit breakers.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Up.opposite(), Side::Down);
        assert_eq!(Side::Down.opposite(), Side::Up);
    }

    #[test]
    fn test_side_display() {
        assert_eq!(Side::Up.to_string(), "UP");
        assert_eq!(Side::Down.to_string(), "DOWN");
    }

    #[test]
    fn test_order_status_terminal() {
        assert!(OrderStatus::Filled.is_terminal());
        assert!(OrderStatus::Cancelled.is_terminal());
        assert!(!OrderStatus::Pending.is_terminal());
        assert!(!OrderStatus::Submitted.is_terminal());
    }

    #[test]
    fn test_order_status_active() {
        assert!(OrderStatus::Pending.is_active());
        assert!(OrderStatus::Submitted.is_active());
        assert!(OrderStatus::PartiallyFilled.is_active());
        assert!(!OrderStatus::Filled.is_active());
    }

    #[test]
    fn test_domain_from_str() {
        assert_eq!(Domain::from_str("crypto").unwrap(), Domain::Crypto);
        assert_eq!(Domain::from_str("Sports").unwrap(), Domain::Sports);
        assert_eq!(Domain::from_str("POLITICS").unwrap(), Domain::Politics);
        assert_eq!(Domain::from_str("custom:42").unwrap(), Domain::Custom(42));
        assert!(Domain::from_str("invalid").is_err());
    }

    #[test]
    fn test_domain_display() {
        assert_eq!(Domain::Crypto.to_string(), "Crypto");
        assert_eq!(Domain::Custom(7).to_string(), "Custom(7)");
    }

    #[test]
    fn test_strategy_state_transitions() {
        use StrategyState::*;
        assert!(Idle.can_transition_to(WatchWindow));
        assert!(WatchWindow.can_transition_to(Leg1Pending));
        assert!(!Idle.can_transition_to(Leg1Filled));
        assert!(!WatchWindow.can_transition_to(Leg2Pending));
    }

    #[test]
    fn test_strategy_state_from_str() {
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
    fn test_risk_state() {
        assert!(RiskState::Normal.can_trade());
        assert!(RiskState::Elevated.can_trade());
        assert!(!RiskState::Halted.can_trade());
        assert!(RiskState::Normal.can_open_new_cycle());
        assert!(!RiskState::Halted.can_open_new_cycle());
    }

    #[test]
    fn test_timeframe_as_str() {
        assert_eq!(Timeframe::M5.as_str(), "5m");
        assert_eq!(Timeframe::M15.as_str(), "15m");
        assert_eq!(Timeframe::Other("1h".into()).as_str(), "1h");
    }
}