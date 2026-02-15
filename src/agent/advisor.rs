//! Advisory agent for market analysis and trading recommendations
//!
//! Provides non-autonomous advisory capabilities that help traders
//! make informed decisions.

use crate::agent::client::ClaudeAgentClient;
use crate::agent::protocol::{
    AgentContext, AgentResponse, DailyStats, MarketSnapshot, RiskAssessment,
};
use crate::domain::{RiskState, StrategyState};
use crate::error::Result;
use rust_decimal::Decimal;
use tracing::{debug, info};

/// Advisory agent for trading assistance
pub struct AdvisoryAgent {
    client: ClaudeAgentClient,
}

impl AdvisoryAgent {
    /// Create a new advisory agent
    pub fn new(client: ClaudeAgentClient) -> Self {
        Self { client }
    }

    /// Create with default client
    pub fn default_client() -> Self {
        Self::new(ClaudeAgentClient::new())
    }

    /// Analyze a market and provide recommendations
    pub async fn analyze_market(&self, market: &MarketSnapshot) -> Result<AgentResponse> {
        let prompt = format!(
            r#"Analyze this prediction market and provide trading recommendations:

Market: {}
Description: {}
YES Bid/Ask: {:?} / {:?}
NO Bid/Ask: {:?} / {:?}
Minutes Remaining: {:?}
Sum of Asks: {:?}
Sum of Bids: {:?}

Evaluate:
1. Is there an arbitrage opportunity? (sum_asks < 1 or sum_bids > 1)
2. What's the expected value if we have conviction on an outcome?
3. Is the spread acceptable for trading?
4. What are the risks given time remaining?

Provide specific, actionable recommendations."#,
            market.market_id,
            market.description.as_deref().unwrap_or("N/A"),
            market.yes_bid,
            market.yes_ask,
            market.no_bid,
            market.no_ask,
            market.minutes_remaining,
            market.sum_asks,
            market.sum_bids,
        );

        let context = AgentContext::new(market.clone(), StrategyState::Idle, RiskState::Normal);

        info!("Requesting market analysis from Claude agent");
        self.client.query(&prompt, &context).await
    }

    /// Evaluate whether to enter a position
    pub async fn recommend_entry(
        &self,
        market: &MarketSnapshot,
        strategy_state: StrategyState,
        risk_state: RiskState,
        daily_stats: &DailyStats,
    ) -> Result<AgentResponse> {
        let prompt = format!(
            r#"Evaluate whether we should enter a new position:

Current Strategy State: {:?}
Risk State: {:?}
Daily P&L: ${}
Trades Today: {}
Win Rate: {:?}%

Market conditions are provided in context.

Should we:
1. Enter a position? If yes, which side and at what price?
2. Wait for better conditions?
3. Stay out due to risk concerns?

Consider our daily performance and risk limits."#,
            strategy_state,
            risk_state,
            daily_stats.realized_pnl,
            daily_stats.trade_count,
            daily_stats.win_rate,
        );

        let context = AgentContext::new(market.clone(), strategy_state, risk_state)
            .with_daily_stats(daily_stats.clone());

        debug!("Requesting entry recommendation from Claude agent");
        self.client.query(&prompt, &context).await
    }

    /// Evaluate whether to exit a position
    pub async fn recommend_exit(
        &self,
        context: &AgentContext,
        position_value: Decimal,
        unrealized_pnl: Decimal,
    ) -> Result<AgentResponse> {
        let prompt = format!(
            r#"Evaluate whether we should exit our current position:

Position Value: ${}
Unrealized P&L: ${}
Strategy State: {:?}
Risk State: {:?}

Should we:
1. Exit now to lock in profit/limit loss?
2. Hold for better exit price?
3. Adjust our exit strategy?

Consider market conditions and time remaining."#,
            position_value, unrealized_pnl, context.strategy_state, context.risk_state,
        );

        self.client.query(&prompt, context).await
    }

    /// Assess overall risk and trading viability
    pub async fn assess_risk(&self, context: &AgentContext) -> Result<RiskAssessment> {
        let prompt = r#"Assess the current risk level for trading:

Review the context and provide:
1. Overall risk level (1-10)
2. Recommended position size adjustment factor (0.0-1.0)
3. Whether trading should be halted
4. Specific concerns if any

Be conservative - protecting capital is priority #1."#;

        let response = self.client.query(prompt, context).await?;

        // Extract risk assessment from response
        if let Some(assessment) = response.risk_assessment {
            Ok(assessment)
        } else {
            // Create default from response
            Ok(RiskAssessment {
                risk_level: 5,
                position_size_factor: 1.0,
                should_halt: false,
                reasoning: response.reasoning,
                concerns: Vec::new(),
            })
        }
    }

    /// Get strategy optimization suggestions
    pub async fn suggest_optimizations(
        &self,
        context: &AgentContext,
        current_parameters: &str,
    ) -> Result<AgentResponse> {
        let prompt = format!(
            r#"Review our current trading parameters and suggest optimizations:

Current Parameters:
{}

Recent Performance:
- Realized P&L: ${}
- Trade Count: {}
- Cycle Count: {}
- Win Rate: {:?}%

Suggest improvements to:
1. Entry criteria (price thresholds, timing)
2. Exit criteria (profit targets, stop losses)
3. Position sizing
4. Risk management

Be specific with numerical suggestions where possible."#,
            current_parameters,
            context.daily_stats.realized_pnl,
            context.daily_stats.trade_count,
            context.daily_stats.cycle_count,
            context.daily_stats.win_rate,
        );

        self.client.query(&prompt, context).await
    }

    /// Interactive chat for ad-hoc questions
    pub async fn chat(&self, question: &str, context: Option<&AgentContext>) -> Result<String> {
        if let Some(ctx) = context {
            let prompt = format!(
                r#"Trading Question: {}

Current context is provided. Please answer the question considering our current market position and risk state."#,
                question
            );
            let response = self.client.query(&prompt, ctx).await?;
            Ok(response.reasoning)
        } else {
            self.client.simple_query(question).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_advisor_creation() {
        let advisor = AdvisoryAgent::default_client();
        // Just verify it can be created
        let _ = advisor;
    }
}
