use crate::adapters::PolymarketClient;
use crate::error::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

/// Direction of price movement for an outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeDirection {
    /// Price goes UP to or above this level (e.g., ↑ 94,000)
    Up,
    /// Price goes DOWN to or below this level (e.g., ↓ 86,000)
    Down,
}

impl OutcomeDirection {
    pub fn from_symbol(s: &str) -> Option<Self> {
        if s.contains('↑') || s.to_lowercase().contains("up") || s.contains('>') {
            Some(Self::Up)
        } else if s.contains('↓') || s.to_lowercase().contains("down") || s.contains('<') {
            Some(Self::Down)
        } else {
            None
        }
    }
}

/// A single outcome in a multi-outcome market
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    /// Token ID for this outcome
    pub token_id: String,
    /// Outcome name/description (e.g., "↑ 94,000")
    pub name: String,
    /// Price level extracted from name (e.g., 94000)
    pub price_level: Option<Decimal>,
    /// Direction (Up or Down)
    pub direction: Option<OutcomeDirection>,
    /// Current Yes price (probability)
    pub yes_price: Option<Decimal>,
    /// Current No price
    pub no_price: Option<Decimal>,
    /// Yes order size
    pub yes_size: Option<Decimal>,
    /// No order size
    pub no_size: Option<Decimal>,
    /// Last update time
    pub timestamp: DateTime<Utc>,
}

impl Outcome {
    /// Parse price level from outcome name like "↑ 94,000" or "↓ 86,000"
    pub fn parse_price_level(name: &str) -> Option<Decimal> {
        let cleaned: String = name
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.')
            .collect();

        Decimal::from_str(&cleaned).ok()
    }

    /// Calculate bid-ask spread
    pub fn spread(&self) -> Option<Decimal> {
        match (self.yes_price, self.no_price) {
            (Some(yes), Some(no)) => {
                let sum = yes + no;
                if sum > Decimal::ONE {
                    Some(sum - Decimal::ONE)
                } else {
                    Some(Decimal::ZERO)
                }
            }
            _ => None,
        }
    }

    /// Check if bid-ask presents arbitrage (sum < 1)
    pub fn has_spread_arbitrage(&self) -> bool {
        match (self.yes_price, self.no_price) {
            (Some(yes), Some(no)) => yes + no < Decimal::ONE,
            _ => false,
        }
    }

    /// Implied probability from Yes price
    pub fn implied_probability(&self) -> Option<Decimal> {
        self.yes_price
    }
}

/// Types of arbitrage opportunities in multi-outcome markets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArbitrageType {
    /// Monotonicity violation: lower target has lower probability than higher target
    MonotonicityViolation {
        outcome_a: String,
        outcome_b: String,
        prob_a: Decimal,
        prob_b: Decimal,
        expected_relationship: String,
    },
    /// Bid-ask spread arbitrage: Yes + No < 1
    SpreadArbitrage {
        outcome: String,
        yes_price: Decimal,
        no_price: Decimal,
        profit: Decimal,
    },
    /// Cross-outcome inconsistency
    CrossOutcomeArbitrage {
        description: String,
        outcomes: Vec<String>,
        estimated_profit: Decimal,
    },
}

impl std::fmt::Display for ArbitrageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArbitrageType::MonotonicityViolation {
                outcome_a,
                outcome_b,
                ..
            } => write!(f, "Monotonicity Violation: {} vs {}", outcome_a, outcome_b),
            ArbitrageType::SpreadArbitrage { outcome, .. } => {
                write!(f, "Spread Arbitrage: {}", outcome)
            }
            ArbitrageType::CrossOutcomeArbitrage { description, .. } => {
                write!(f, "Cross-Outcome: {}", description)
            }
        }
    }
}

/// Detected arbitrage opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiOutcomeArbitrage {
    /// Type of arbitrage
    pub arb_type: ArbitrageType,
    /// Estimated profit per $1 invested
    pub profit_per_dollar: Decimal,
    /// Confidence level (0-1)
    pub confidence: Decimal,
    /// Detection timestamp
    pub detected_at: DateTime<Utc>,
}

/// Summary of an outcome's current state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeSummary {
    pub name: String,
    pub direction: Option<OutcomeDirection>,
    pub price_level: Option<Decimal>,
    pub yes_price: Option<Decimal>,
    pub no_price: Option<Decimal>,
    pub spread: Option<Decimal>,
    pub implied_prob_pct: Option<Decimal>,
}

/// Multi-outcome market monitor
pub struct MultiOutcomeMonitor {
    /// Event ID
    pub event_id: String,
    /// Event title
    pub event_title: String,
    /// All outcomes indexed by token_id
    outcomes: HashMap<String, Outcome>,
    /// Outcomes sorted by price level (for monotonicity checks)
    up_outcomes: Vec<String>,
    down_outcomes: Vec<String>,
}

impl MultiOutcomeMonitor {
    /// Create a new monitor for a multi-outcome event
    pub fn new(event_id: &str, event_title: &str) -> Self {
        Self {
            event_id: event_id.to_string(),
            event_title: event_title.to_string(),
            outcomes: HashMap::new(),
            up_outcomes: Vec::new(),
            down_outcomes: Vec::new(),
        }
    }

    /// Add an outcome to the monitor
    pub fn add_outcome(&mut self, token_id: String, name: String) {
        let price_level = Outcome::parse_price_level(&name);
        let direction = OutcomeDirection::from_symbol(&name);

        let outcome = Outcome {
            token_id: token_id.clone(),
            name,
            price_level,
            direction,
            yes_price: None,
            no_price: None,
            yes_size: None,
            no_size: None,
            timestamp: Utc::now(),
        };

        self.outcomes.insert(token_id, outcome);
        self.rebuild_sorted_lists();
    }

    fn rebuild_sorted_lists(&mut self) {
        self.up_outcomes.clear();
        self.down_outcomes.clear();

        for (token_id, outcome) in &self.outcomes {
            match outcome.direction {
                Some(OutcomeDirection::Up) => self.up_outcomes.push(token_id.clone()),
                Some(OutcomeDirection::Down) => self.down_outcomes.push(token_id.clone()),
                None => {}
            }
        }

        self.up_outcomes.sort_by(|a, b| {
            let level_a = self.outcomes.get(a).and_then(|o| o.price_level);
            let level_b = self.outcomes.get(b).and_then(|o| o.price_level);
            level_a.cmp(&level_b)
        });

        self.down_outcomes.sort_by(|a, b| {
            let level_a = self.outcomes.get(a).and_then(|o| o.price_level);
            let level_b = self.outcomes.get(b).and_then(|o| o.price_level);
            level_b.cmp(&level_a)
        });
    }

    /// Update quote for an outcome
    pub fn update_quote(
        &mut self,
        token_id: &str,
        yes_price: Option<Decimal>,
        no_price: Option<Decimal>,
        yes_size: Option<Decimal>,
        no_size: Option<Decimal>,
    ) {
        if let Some(outcome) = self.outcomes.get_mut(token_id) {
            outcome.yes_price = yes_price;
            outcome.no_price = no_price;
            outcome.yes_size = yes_size;
            outcome.no_size = no_size;
            outcome.timestamp = Utc::now();
        }
    }

    pub fn all_token_ids(&self) -> Vec<String> {
        self.outcomes.keys().cloned().collect()
    }

    pub fn outcome_count(&self) -> usize {
        self.outcomes.len()
    }

    /// Find all monotonicity violations
    pub fn find_monotonicity_violations(&self) -> Vec<MultiOutcomeArbitrage> {
        let mut violations = Vec::new();

        for i in 0..self.up_outcomes.len().saturating_sub(1) {
            let token_a = &self.up_outcomes[i];
            let token_b = &self.up_outcomes[i + 1];

            if let (Some(outcome_a), Some(outcome_b)) =
                (self.outcomes.get(token_a), self.outcomes.get(token_b))
            {
                if let (Some(prob_a), Some(prob_b)) = (
                    outcome_a.implied_probability(),
                    outcome_b.implied_probability(),
                ) {
                    if prob_a < prob_b {
                        let profit = prob_b - prob_a;
                        violations.push(MultiOutcomeArbitrage {
                            arb_type: ArbitrageType::MonotonicityViolation {
                                outcome_a: outcome_a.name.clone(),
                                outcome_b: outcome_b.name.clone(),
                                prob_a,
                                prob_b,
                                expected_relationship: format!(
                                    "{} should have >= probability than {}",
                                    outcome_a.name, outcome_b.name
                                ),
                            },
                            profit_per_dollar: profit,
                            confidence: dec!(0.8),
                            detected_at: Utc::now(),
                        });
                    }
                }
            }
        }

        for i in 0..self.down_outcomes.len().saturating_sub(1) {
            let token_a = &self.down_outcomes[i];
            let token_b = &self.down_outcomes[i + 1];

            if let (Some(outcome_a), Some(outcome_b)) =
                (self.outcomes.get(token_a), self.outcomes.get(token_b))
            {
                if let (Some(prob_a), Some(prob_b)) = (
                    outcome_a.implied_probability(),
                    outcome_b.implied_probability(),
                ) {
                    if prob_a < prob_b {
                        let profit = prob_b - prob_a;
                        violations.push(MultiOutcomeArbitrage {
                            arb_type: ArbitrageType::MonotonicityViolation {
                                outcome_a: outcome_a.name.clone(),
                                outcome_b: outcome_b.name.clone(),
                                prob_a,
                                prob_b,
                                expected_relationship: format!(
                                    "{} should have >= probability than {}",
                                    outcome_a.name, outcome_b.name
                                ),
                            },
                            profit_per_dollar: profit,
                            confidence: dec!(0.8),
                            detected_at: Utc::now(),
                        });
                    }
                }
            }
        }

        violations
    }

    /// Find spread arbitrage opportunities (Yes + No < 1)
    pub fn find_spread_arbitrage(&self) -> Vec<MultiOutcomeArbitrage> {
        self.outcomes
            .values()
            .filter_map(|outcome| {
                if let (Some(yes), Some(no)) = (outcome.yes_price, outcome.no_price) {
                    let sum = yes + no;
                    if sum < Decimal::ONE {
                        let profit = Decimal::ONE - sum;
                        return Some(MultiOutcomeArbitrage {
                            arb_type: ArbitrageType::SpreadArbitrage {
                                outcome: outcome.name.clone(),
                                yes_price: yes,
                                no_price: no,
                                profit,
                            },
                            profit_per_dollar: profit,
                            confidence: dec!(0.95),
                            detected_at: Utc::now(),
                        });
                    }
                }
                None
            })
            .collect()
    }

    pub fn find_all_arbitrage(&self) -> Vec<MultiOutcomeArbitrage> {
        let mut arbs = Vec::new();
        arbs.extend(self.find_monotonicity_violations());
        arbs.extend(self.find_spread_arbitrage());
        arbs.sort_by(|a, b| b.profit_per_dollar.cmp(&a.profit_per_dollar));
        arbs
    }

    pub fn summary(&self) -> Vec<OutcomeSummary> {
        let mut summaries: Vec<_> = self
            .outcomes
            .values()
            .map(|o| OutcomeSummary {
                name: o.name.clone(),
                direction: o.direction,
                price_level: o.price_level,
                yes_price: o.yes_price,
                no_price: o.no_price,
                spread: o.spread(),
                implied_prob_pct: o.implied_probability().map(|p| p * dec!(100)),
            })
            .collect();

        summaries.sort_by(|a, b| match (&a.direction, &b.direction) {
            (Some(OutcomeDirection::Up), Some(OutcomeDirection::Down)) => std::cmp::Ordering::Less,
            (Some(OutcomeDirection::Down), Some(OutcomeDirection::Up)) => {
                std::cmp::Ordering::Greater
            }
            (Some(OutcomeDirection::Up), Some(OutcomeDirection::Up)) => {
                b.price_level.cmp(&a.price_level)
            }
            (Some(OutcomeDirection::Down), Some(OutcomeDirection::Down)) => {
                a.price_level.cmp(&b.price_level)
            }
            _ => a.name.cmp(&b.name),
        });

        summaries
    }
}

/// Fetch multi-outcome market data from Polymarket
pub async fn fetch_multi_outcome_event(
    client: &PolymarketClient,
    event_id: &str,
) -> Result<MultiOutcomeMonitor> {
    let event = client.get_event_details(event_id).await?;

    let title = event.title.unwrap_or_else(|| event_id.to_string());
    let mut monitor = MultiOutcomeMonitor::new(event_id, &title);

    for market in &event.markets {
        let outcome_name = market
            .group_item_title
            .clone()
            .or_else(|| market.question.clone())
            .unwrap_or_else(|| "Unknown".to_string());

        if let Some(clob_ids_str) = &market.clob_token_ids {
            if let Ok(token_ids) = serde_json::from_str::<Vec<String>>(clob_ids_str) {
                if let Some(yes_token_id) = token_ids.first() {
                    monitor.add_outcome(yes_token_id.clone(), outcome_name.clone());

                    if let Some(prices_str) = &market.outcome_prices {
                        if let Ok(prices) = serde_json::from_str::<Vec<String>>(prices_str) {
                            let yes_price = prices.first().and_then(|p| Decimal::from_str(p).ok());
                            let no_price = prices.get(1).and_then(|p| Decimal::from_str(p).ok());

                            monitor.update_quote(yes_token_id, yes_price, no_price, None, None);
                        }
                    }
                }
            }
        }
    }

    Ok(monitor)
}
