//! Agent communication protocol definitions
//!
//! Defines the data structures for communication between the trading system
//! and the Claude agent.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::{RiskState, Side, StrategyState};

/// Snapshot of current market conditions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    /// Market/event identifier
    pub market_id: String,
    /// YES token id for execution (when resolvable)
    pub yes_token_id: Option<String>,
    /// NO token id for execution (when resolvable)
    pub no_token_id: Option<String>,
    /// Human-readable description
    pub description: Option<String>,
    /// Current YES price (best bid)
    pub yes_bid: Option<Decimal>,
    /// Current YES price (best ask)
    pub yes_ask: Option<Decimal>,
    /// Current NO price (best bid)
    pub no_bid: Option<Decimal>,
    /// Current NO price (best ask)
    pub no_ask: Option<Decimal>,
    /// Bid size for YES
    pub yes_bid_size: Option<Decimal>,
    /// Ask size for YES
    pub yes_ask_size: Option<Decimal>,
    /// Market end time
    pub end_time: Option<DateTime<Utc>>,
    /// Minutes remaining until market close
    pub minutes_remaining: Option<i64>,
    /// Sum of YES ask + NO ask (for arbitrage detection)
    pub sum_asks: Option<Decimal>,
    /// Sum of YES bid + NO bid (for arbitrage detection)
    pub sum_bids: Option<Decimal>,
    /// Timestamp of this snapshot
    pub timestamp: DateTime<Utc>,
}

impl MarketSnapshot {
    pub fn new(market_id: String) -> Self {
        Self {
            market_id,
            yes_token_id: None,
            no_token_id: None,
            description: None,
            yes_bid: None,
            yes_ask: None,
            no_bid: None,
            no_ask: None,
            yes_bid_size: None,
            yes_ask_size: None,
            end_time: None,
            minutes_remaining: None,
            sum_asks: None,
            sum_bids: None,
            timestamp: Utc::now(),
        }
    }
}

/// Information about a current position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionInfo {
    /// Token ID
    pub token_id: String,
    /// Position side (YES/NO equivalent)
    pub side: Side,
    /// Number of shares held
    pub shares: Decimal,
    /// Average entry price
    pub avg_entry_price: Decimal,
    /// Current market price
    pub current_price: Option<Decimal>,
    /// Unrealized P&L
    pub unrealized_pnl: Option<Decimal>,
    /// When position was opened
    pub opened_at: DateTime<Utc>,
}

/// Record of a completed trade
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Trade ID
    pub trade_id: String,
    /// Side (buy/sell)
    pub side: String,
    /// Shares traded
    pub shares: Decimal,
    /// Execution price
    pub price: Decimal,
    /// Trade timestamp
    pub timestamp: DateTime<Utc>,
    /// Realized P&L (if closing trade)
    pub realized_pnl: Option<Decimal>,
}

/// Daily trading statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyStats {
    /// Total realized P&L today
    pub realized_pnl: Decimal,
    /// Number of trades executed
    pub trade_count: u32,
    /// Number of cycles completed
    pub cycle_count: u32,
    /// Win rate percentage
    pub win_rate: Option<f64>,
    /// Average profit per trade
    pub avg_profit: Option<Decimal>,
}

/// Risk assessment from the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// Overall risk level (1-10)
    pub risk_level: u8,
    /// Suggested position size adjustment (0.0 to 1.0)
    pub position_size_factor: f64,
    /// Whether to halt trading
    pub should_halt: bool,
    /// Reasoning for assessment
    pub reasoning: String,
    /// Specific concerns identified
    pub concerns: Vec<String>,
}

/// Complete context for agent decision-making
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    /// Current market snapshot
    pub market_state: MarketSnapshot,
    /// Current strategy state
    pub strategy_state: StrategyState,
    /// Current risk state
    pub risk_state: RiskState,
    /// Active positions
    pub positions: Vec<PositionInfo>,
    /// Recent trade history
    pub recent_trades: Vec<TradeRecord>,
    /// Today's statistics
    pub daily_stats: DailyStats,
    /// Account balance (USDC)
    pub account_balance: Option<Decimal>,
    /// Timestamp of context creation
    pub timestamp: DateTime<Utc>,
}

impl AgentContext {
    pub fn new(
        market_state: MarketSnapshot,
        strategy_state: StrategyState,
        risk_state: RiskState,
    ) -> Self {
        Self {
            market_state,
            strategy_state,
            risk_state,
            positions: Vec::new(),
            recent_trades: Vec::new(),
            daily_stats: DailyStats {
                realized_pnl: Decimal::ZERO,
                trade_count: 0,
                cycle_count: 0,
                win_rate: None,
                avg_profit: None,
            },
            account_balance: None,
            timestamp: Utc::now(),
        }
    }

    pub fn with_positions(mut self, positions: Vec<PositionInfo>) -> Self {
        self.positions = positions;
        self
    }

    pub fn with_trades(mut self, trades: Vec<TradeRecord>) -> Self {
        self.recent_trades = trades;
        self
    }

    pub fn with_daily_stats(mut self, stats: DailyStats) -> Self {
        self.daily_stats = stats;
        self
    }

    pub fn with_balance(mut self, balance: Decimal) -> Self {
        self.account_balance = Some(balance);
        self
    }
}

/// Actions the agent can recommend or execute
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentAction {
    /// Request market analysis
    Analyze {
        market_id: String,
        depth: Option<String>, // "quick", "standard", "deep"
    },
    /// Enter a new position
    EnterPosition {
        side: Side,
        shares: u64,
        max_price: Decimal,
        reasoning: String,
    },
    /// Exit an existing position
    ExitPosition {
        token_id: String,
        min_price: Option<Decimal>,
        reasoning: String,
    },
    /// Adjust risk parameters
    AdjustRisk {
        new_state: RiskState,
        reasoning: String,
    },
    /// Suggest parameter optimization
    OptimizeParameters {
        parameter: String,
        suggested_value: String,
        reasoning: String,
    },
    /// Wait and continue monitoring
    Wait { duration_secs: u64, reason: String },
    /// Alert the user
    Alert {
        severity: String, // "info", "warning", "critical"
        message: String,
    },
    /// No action needed
    NoAction { reason: String },
}

/// Response from the Claude agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    /// Chain of thought reasoning
    pub reasoning: String,
    /// Confidence level (0.0 to 1.0)
    pub confidence: f64,
    /// Recommended actions in priority order
    pub recommended_actions: Vec<AgentAction>,
    /// Risk assessment
    pub risk_assessment: Option<RiskAssessment>,
    /// Summary for logging
    pub summary: String,
    /// Raw response for debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_response: Option<String>,
}

impl AgentResponse {
    pub fn no_action(reason: &str) -> Self {
        Self {
            reasoning: reason.to_string(),
            confidence: 1.0,
            recommended_actions: vec![AgentAction::NoAction {
                reason: reason.to_string(),
            }],
            risk_assessment: None,
            summary: format!("No action: {}", reason),
            raw_response: None,
        }
    }

    pub fn with_action(reasoning: &str, action: AgentAction) -> Self {
        let summary = match &action {
            AgentAction::EnterPosition { side, shares, .. } => {
                format!("Enter {:?} position with {} shares", side, shares)
            }
            AgentAction::ExitPosition { token_id, .. } => {
                format!("Exit position {}", token_id)
            }
            AgentAction::Wait { duration_secs, .. } => {
                format!("Wait {} seconds", duration_secs)
            }
            AgentAction::Alert { severity, message } => {
                format!("[{}] {}", severity.to_uppercase(), message)
            }
            _ => "Action recommended".to_string(),
        };

        Self {
            reasoning: reasoning.to_string(),
            confidence: 0.8,
            recommended_actions: vec![action],
            risk_assessment: None,
            summary,
            raw_response: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_context_creation() {
        let market = MarketSnapshot::new("test-market".to_string());
        let ctx = AgentContext::new(market, StrategyState::Idle, RiskState::Normal);

        assert_eq!(ctx.strategy_state, StrategyState::Idle);
        assert_eq!(ctx.risk_state, RiskState::Normal);
        assert!(ctx.positions.is_empty());
    }

    #[test]
    fn test_agent_response_no_action() {
        let response = AgentResponse::no_action("Market conditions unfavorable");
        assert_eq!(response.confidence, 1.0);
        assert_eq!(response.recommended_actions.len(), 1);
    }
}
