//! Temporal leg straddle coordinator
//!
//! Manages a state machine for temporal straddle positions:
//! Leg1Active → WaitingLeg2Trigger → Leg2Active → Complete
//!
//! The straddle allows profiting from price reversals by entering
//! opposing legs at different times on binary markets.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::config::StraddleConfig;

/// State of a single straddle position
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StraddleState {
    /// Leg1 has been entered (e.g., bought UP at 0.45)
    Leg1Active,
    /// Waiting for price to move to trigger Leg2 entry
    WaitingLeg2Trigger,
    /// Both legs active
    Leg2Active,
    /// Both legs resolved (either both settled or manually closed)
    Complete,
    /// Expired — Leg2 trigger never arrived within max_wait
    Expired,
}

impl std::fmt::Display for StraddleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StraddleState::Leg1Active => write!(f, "Leg1Active"),
            StraddleState::WaitingLeg2Trigger => write!(f, "WaitingLeg2"),
            StraddleState::Leg2Active => write!(f, "Leg2Active"),
            StraddleState::Complete => write!(f, "Complete"),
            StraddleState::Expired => write!(f, "Expired"),
        }
    }
}

/// Tracked straddle instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveStraddle {
    pub id: String,
    pub symbol: String,
    pub state: StraddleState,
    /// Leg1 entry price
    pub leg1_cost: Decimal,
    /// Leg1 entry side (e.g., "UP")
    pub leg1_side: String,
    /// When Leg1 was entered
    pub leg1_entered_at: DateTime<Utc>,
    /// Leg2 target side (opposite of Leg1)
    pub leg2_side: String,
    /// Leg2 entry price (if filled)
    pub leg2_cost: Option<Decimal>,
    /// When Leg2 was entered (if filled)
    pub leg2_entered_at: Option<DateTime<Utc>>,
    /// BTC spot price at Leg1 entry
    pub leg1_spot_price: Decimal,
}

impl ActiveStraddle {
    /// Combined cost of both legs (only meaningful when Leg2Active)
    pub fn combined_cost(&self) -> Option<Decimal> {
        self.leg2_cost.map(|l2| self.leg1_cost + l2)
    }

    /// Whether the straddle guarantees profit (combined cost < 1.00)
    pub fn is_profitable(&self) -> Option<bool> {
        self.combined_cost().map(|c| c < Decimal::ONE)
    }
}

/// Signal emitted by the straddle manager
#[derive(Debug, Clone)]
pub enum StraddleSignal {
    /// Enter Leg2 — tells crypto agent to switch mode and target
    EnterLeg2 {
        straddle_id: String,
        symbol: String,
        side: String,
        max_price: Decimal,
    },
    /// Cancel/expire a straddle — no Leg2 trigger arrived
    Expire {
        straddle_id: String,
        reason: String,
    },
}

/// Manages active straddle state machines
pub struct StraddleManager {
    config: StraddleConfig,
    active: HashMap<String, ActiveStraddle>,
    next_id: u64,
}

impl StraddleManager {
    pub fn new(config: StraddleConfig) -> Self {
        Self {
            config,
            active: HashMap::new(),
            next_id: 1,
        }
    }

    /// Whether straddle mode is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Register a new Leg1 entry
    pub fn register_leg1(
        &mut self,
        symbol: String,
        side: String,
        cost: Decimal,
        spot_price: Decimal,
    ) -> String {
        let id = format!("straddle-{}", self.next_id);
        self.next_id += 1;

        let leg2_side = if side == "UP" {
            "DOWN".to_string()
        } else {
            "UP".to_string()
        };

        let straddle = ActiveStraddle {
            id: id.clone(),
            symbol,
            state: StraddleState::Leg1Active,
            leg1_cost: cost,
            leg1_side: side,
            leg1_entered_at: Utc::now(),
            leg2_side,
            leg2_cost: None,
            leg2_entered_at: None,
            leg1_spot_price: spot_price,
        };

        info!(
            id = %straddle.id,
            symbol = %straddle.symbol,
            side = %straddle.leg1_side,
            cost = %straddle.leg1_cost,
            "straddle: Leg1 registered"
        );

        self.active.insert(id.clone(), straddle);
        id
    }

    /// Tick the straddle manager: check triggers, expire timeouts.
    /// Returns signals for the agent to act on.
    pub fn tick(&mut self, current_spot: Decimal) -> Vec<StraddleSignal> {
        if !self.config.enabled {
            return Vec::new();
        }

        let now = Utc::now();
        let mut signals = Vec::new();
        let mut completed = Vec::new();

        for (id, straddle) in self.active.iter_mut() {
            match straddle.state {
                StraddleState::Leg1Active => {
                    // Transition to WaitingLeg2Trigger immediately after fill confirmation
                    straddle.state = StraddleState::WaitingLeg2Trigger;
                    debug!(id = %id, "straddle: moved to WaitingLeg2Trigger");
                }
                StraddleState::WaitingLeg2Trigger => {
                    // Check timeout
                    let elapsed = (now - straddle.leg1_entered_at).num_seconds() as u64;
                    if elapsed > self.config.leg2_max_wait_secs {
                        straddle.state = StraddleState::Expired;
                        signals.push(StraddleSignal::Expire {
                            straddle_id: id.clone(),
                            reason: format!(
                                "Leg2 trigger timeout after {}s",
                                elapsed
                            ),
                        });
                        completed.push(id.clone());
                        continue;
                    }

                    // Check price move trigger
                    let entry_spot = straddle.leg1_spot_price;
                    if !entry_spot.is_zero() {
                        let move_pct = ((current_spot - entry_spot) / entry_spot)
                            .abs()
                            .to_string()
                            .parse::<f64>()
                            .unwrap_or(0.0)
                            * 100.0;

                        if move_pct >= self.config.leg2_trigger_move_pct {
                            // Calculate max price for Leg2 to ensure profitability
                            let max_combined =
                                Decimal::from_f64_retain(self.config.max_combined_cost)
                                    .unwrap_or(Decimal::new(97, 2));
                            let max_leg2_price = max_combined - straddle.leg1_cost;

                            if max_leg2_price > Decimal::ZERO {
                                info!(
                                    id = %id,
                                    move_pct = format!("{:.2}", move_pct),
                                    max_leg2 = %max_leg2_price,
                                    "straddle: Leg2 trigger fired"
                                );
                                signals.push(StraddleSignal::EnterLeg2 {
                                    straddle_id: id.clone(),
                                    symbol: straddle.symbol.clone(),
                                    side: straddle.leg2_side.clone(),
                                    max_price: max_leg2_price,
                                });
                                straddle.state = StraddleState::Leg2Active;
                            } else {
                                warn!(
                                    id = %id,
                                    leg1_cost = %straddle.leg1_cost,
                                    "straddle: Leg2 skipped — no profit margin"
                                );
                                straddle.state = StraddleState::Expired;
                                completed.push(id.clone());
                            }
                        }
                    }
                }
                StraddleState::Complete | StraddleState::Expired => {
                    completed.push(id.clone());
                }
                StraddleState::Leg2Active => {
                    // Leg2 is active — await settlement (managed externally)
                }
            }
        }

        // Clean up completed/expired straddles
        for id in completed {
            self.active.remove(&id);
        }

        signals
    }

    /// Mark Leg2 as entered
    pub fn confirm_leg2(&mut self, straddle_id: &str, cost: Decimal) {
        if let Some(straddle) = self.active.get_mut(straddle_id) {
            straddle.leg2_cost = Some(cost);
            straddle.leg2_entered_at = Some(Utc::now());
            straddle.state = StraddleState::Leg2Active;

            if let Some(combined) = straddle.combined_cost() {
                info!(
                    id = %straddle_id,
                    combined_cost = %combined,
                    profitable = combined < Decimal::ONE,
                    "straddle: Leg2 confirmed"
                );
            }
        }
    }

    /// Mark a straddle as complete (both legs settled)
    pub fn mark_complete(&mut self, straddle_id: &str) {
        if let Some(straddle) = self.active.get_mut(straddle_id) {
            straddle.state = StraddleState::Complete;
        }
    }

    /// Get all active straddles
    pub fn active_straddles(&self) -> Vec<&ActiveStraddle> {
        self.active
            .values()
            .filter(|s| !matches!(s.state, StraddleState::Complete | StraddleState::Expired))
            .collect()
    }

    /// Build governance metadata for active straddle state
    pub fn governance_metadata(&self) -> HashMap<String, String> {
        let mut meta = HashMap::new();
        let active: Vec<_> = self.active_straddles();

        meta.insert(
            "openclaw.straddle.active_count".to_string(),
            active.len().to_string(),
        );

        if let Some(latest) = active.last() {
            meta.insert(
                "openclaw.straddle.target_symbol".to_string(),
                latest.symbol.clone(),
            );
            meta.insert(
                "openclaw.straddle.leg2_side".to_string(),
                latest.leg2_side.clone(),
            );
            meta.insert(
                "openclaw.straddle.state".to_string(),
                latest.state.to_string(),
            );
        }

        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straddle_combined_cost() {
        let s = ActiveStraddle {
            id: "test".to_string(),
            symbol: "BTCUSDT".to_string(),
            state: StraddleState::Leg2Active,
            leg1_cost: Decimal::new(45, 2), // 0.45
            leg1_side: "UP".to_string(),
            leg1_entered_at: Utc::now(),
            leg2_side: "DOWN".to_string(),
            leg2_cost: Some(Decimal::new(48, 2)), // 0.48
            leg2_entered_at: Some(Utc::now()),
            leg1_spot_price: Decimal::from(95000),
        };

        assert_eq!(s.combined_cost(), Some(Decimal::new(93, 2))); // 0.93
        assert_eq!(s.is_profitable(), Some(true)); // 0.93 < 1.00
    }

    #[test]
    fn straddle_not_profitable() {
        let s = ActiveStraddle {
            id: "test".to_string(),
            symbol: "BTCUSDT".to_string(),
            state: StraddleState::Leg2Active,
            leg1_cost: Decimal::new(55, 2), // 0.55
            leg1_side: "UP".to_string(),
            leg1_entered_at: Utc::now(),
            leg2_side: "DOWN".to_string(),
            leg2_cost: Some(Decimal::new(50, 2)), // 0.50
            leg2_entered_at: Some(Utc::now()),
            leg1_spot_price: Decimal::from(95000),
        };

        assert_eq!(s.combined_cost(), Some(Decimal::new(105, 2))); // 1.05
        assert_eq!(s.is_profitable(), Some(false)); // 1.05 >= 1.00
    }
}
