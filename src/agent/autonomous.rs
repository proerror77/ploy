//! Autonomous trading agent with Claude-powered decision making
//!
//! This agent operates with configurable autonomy levels,
//! from fully autonomous to requiring human confirmation for trades.

use crate::agent::client::ClaudeAgentClient;
use crate::agent::grok::{GrokClient, SearchResult};
use crate::agent::protocol::{
    AgentAction, AgentContext, AgentResponse,
    PositionInfo,
};
use crate::domain::RiskState;
use crate::error::Result;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// Autonomy level for the trading agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyLevel {
    /// Agent only provides advice, no execution
    AdvisoryOnly,
    /// Agent can execute small trades within limits
    LimitedAutonomy,
    /// Agent can execute any trade within risk parameters
    FullAutonomy,
}

/// Configuration for autonomous trading
#[derive(Debug, Clone)]
pub struct AutonomousConfig {
    /// Autonomy level
    pub autonomy_level: AutonomyLevel,
    /// Maximum exposure per trade (USDC)
    pub max_trade_size: Decimal,
    /// Maximum total autonomous exposure (USDC)
    pub max_total_exposure: Decimal,
    /// Minimum confidence required for execution
    pub min_confidence: f64,
    /// Trading hours enabled
    pub trading_enabled: bool,
    /// Interval between analysis cycles (seconds)
    pub analysis_interval_secs: u64,
    /// Allowed strategies (e.g., "arbitrage", "momentum", "dump_detection")
    pub allowed_strategies: Vec<String>,
    /// Whether to require confirmation for exits
    pub require_exit_confirmation: bool,
}

impl Default for AutonomousConfig {
    fn default() -> Self {
        Self {
            autonomy_level: AutonomyLevel::AdvisoryOnly,
            max_trade_size: Decimal::from(50),      // $50 per trade
            max_total_exposure: Decimal::from(200), // $200 total
            min_confidence: 0.75,
            trading_enabled: false,
            analysis_interval_secs: 30,
            allowed_strategies: vec!["arbitrage".to_string()],
            require_exit_confirmation: true,
        }
    }
}

impl AutonomousConfig {
    /// Create a conservative config for initial testing
    pub fn conservative() -> Self {
        Self {
            autonomy_level: AutonomyLevel::LimitedAutonomy,
            max_trade_size: Decimal::from(25),
            max_total_exposure: Decimal::from(100),
            min_confidence: 0.85,
            trading_enabled: true,
            analysis_interval_secs: 60,
            allowed_strategies: vec!["arbitrage".to_string()],
            require_exit_confirmation: true,
        }
    }

    /// Create an aggressive config for experienced use
    pub fn aggressive() -> Self {
        Self {
            autonomy_level: AutonomyLevel::FullAutonomy,
            max_trade_size: Decimal::from(200),
            max_total_exposure: Decimal::from(1000),
            min_confidence: 0.65,
            trading_enabled: true,
            analysis_interval_secs: 15,
            allowed_strategies: vec![
                "arbitrage".to_string(),
                "momentum".to_string(),
                "dump_detection".to_string(),
            ],
            require_exit_confirmation: false,
        }
    }
}

/// Execution result for an action
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub action: AgentAction,
    pub success: bool,
    pub message: String,
    pub trade_id: Option<String>,
}

/// Autonomous trading agent
pub struct AutonomousAgent {
    client: ClaudeAgentClient,
    config: AutonomousConfig,
    /// Current exposure
    current_exposure: Arc<RwLock<Decimal>>,
    /// Active positions managed by agent
    positions: Arc<RwLock<Vec<PositionInfo>>>,
    /// Shutdown signal
    shutdown: Arc<RwLock<bool>>,
    /// Action broadcast channel for external monitoring
    action_tx: broadcast::Sender<AgentAction>,
    /// Optional Grok client for real-time search
    grok: Option<GrokClient>,
}

impl AutonomousAgent {
    /// Create a new autonomous agent
    pub fn new(client: ClaudeAgentClient, config: AutonomousConfig) -> Self {
        let (action_tx, _) = broadcast::channel(100);
        Self {
            client,
            config,
            current_exposure: Arc::new(RwLock::new(Decimal::ZERO)),
            positions: Arc::new(RwLock::new(Vec::new())),
            shutdown: Arc::new(RwLock::new(false)),
            action_tx,
            grok: None,
        }
    }

    /// Add Grok client for real-time search capabilities
    pub fn with_grok(mut self, grok: GrokClient) -> Self {
        self.grok = Some(grok);
        self
    }

    /// Check if Grok is available
    pub fn has_grok(&self) -> bool {
        self.grok.as_ref().map(|g| g.is_configured()).unwrap_or(false)
    }

    /// Fetch real-time context from Grok
    pub async fn fetch_realtime_context(&self, market_slug: &str) -> Option<SearchResult> {
        if let Some(ref grok) = self.grok {
            if grok.is_configured() {
                match grok.search_market(market_slug, "15 minutes").await {
                    Ok(result) => {
                        info!("Grok search: sentiment={:?}", result.sentiment);
                        return Some(result);
                    }
                    Err(e) => {
                        warn!("Grok search failed: {}", e);
                    }
                }
            }
        }
        None
    }

    /// Get a receiver for action notifications
    pub fn subscribe_actions(&self) -> broadcast::Receiver<AgentAction> {
        self.action_tx.subscribe()
    }

    /// Check if trading is allowed
    pub fn can_trade(&self) -> bool {
        self.config.trading_enabled
            && self.config.autonomy_level != AutonomyLevel::AdvisoryOnly
    }

    /// Request shutdown
    pub async fn shutdown(&self) {
        *self.shutdown.write().await = true;
        info!("Autonomous agent shutdown requested");
    }

    /// Run the autonomous trading loop
    pub async fn run<F, Fut>(
        &self,
        context_provider: F,
    ) -> Result<()>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<AgentContext>>,
    {
        info!(
            "Starting autonomous agent with autonomy level: {:?}",
            self.config.autonomy_level
        );

        let interval = Duration::from_secs(self.config.analysis_interval_secs);

        loop {
            // Check for shutdown
            if *self.shutdown.read().await {
                info!("Autonomous agent shutting down");
                break;
            }

            // Get current context
            let context = match context_provider().await {
                Ok(ctx) => ctx,
                Err(e) => {
                    error!("Failed to get context: {}", e);
                    tokio::time::sleep(interval).await;
                    continue;
                }
            };

            // Run analysis cycle
            match self.analyze_and_act(&context).await {
                Ok(actions) => {
                    for action in actions {
                        let _ = self.action_tx.send(action);
                    }
                }
                Err(e) => {
                    error!("Analysis cycle failed: {}", e);
                }
            }

            tokio::time::sleep(interval).await;
        }

        Ok(())
    }

    /// Analyze current state and determine actions
    async fn analyze_and_act(&self, context: &AgentContext) -> Result<Vec<AgentAction>> {
        // Fetch real-time context from Grok if available
        let grok_context = self.fetch_realtime_context(&context.market_state.market_id).await;

        let prompt = self.build_analysis_prompt(context, grok_context.as_ref());

        let response = self.client.query(&prompt, context).await?;

        debug!(
            "Agent response: confidence={}, actions={}",
            response.confidence,
            response.recommended_actions.len()
        );

        // Filter and validate actions
        let valid_actions = self.validate_actions(&response, context).await?;

        // Execute if allowed
        if self.can_trade() {
            for action in &valid_actions {
                match self.execute_action(action, context).await {
                    Ok(result) => {
                        info!("Executed action: {} - {}", result.success, result.message);
                    }
                    Err(e) => {
                        error!("Failed to execute action: {}", e);
                    }
                }
            }
        }

        Ok(valid_actions)
    }

    /// Build analysis prompt based on current state
    fn build_analysis_prompt(&self, _context: &AgentContext, grok_context: Option<&SearchResult>) -> String {
        let strategies = self.config.allowed_strategies.join(", ");

        let realtime_info = if let Some(search) = grok_context {
            let sentiment = search.sentiment
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let key_points = if search.key_points.is_empty() {
                "None available".to_string()
            } else {
                search.key_points.iter()
                    .take(5)
                    .map(|p| format!("  - {}", p))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            format!(
                r#"
## Real-Time Market Intelligence (from Grok)
Sentiment: {}
Key Points:
{}
"#,
                sentiment, key_points
            )
        } else {
            String::new()
        };

        format!(
            r#"Analyze the current market state and recommend trading actions.

Allowed Strategies: {}
Maximum Trade Size: ${}
Current Exposure: Check positions in context
Minimum Confidence Required: {}%
{}
Your analysis should:
1. Identify opportunities matching allowed strategies
2. Assess risk for each opportunity
3. Recommend specific actions with reasoning
4. Consider current positions and exposure limits
5. Factor in real-time sentiment if available

Prioritize capital preservation while seeking profitable opportunities."#,
            strategies,
            self.config.max_trade_size,
            (self.config.min_confidence * 100.0) as u32,
            realtime_info,
        )
    }

    /// Validate actions against configuration limits
    async fn validate_actions(
        &self,
        response: &AgentResponse,
        context: &AgentContext,
    ) -> Result<Vec<AgentAction>> {
        let mut valid_actions = Vec::new();
        let current_exposure = *self.current_exposure.read().await;

        // Check confidence threshold
        if response.confidence < self.config.min_confidence {
            debug!(
                "Response confidence {} below threshold {}",
                response.confidence, self.config.min_confidence
            );
            return Ok(valid_actions);
        }

        for action in &response.recommended_actions {
            match action {
                AgentAction::EnterPosition { shares, max_price, .. } => {
                    let trade_value = Decimal::from(*shares) * *max_price;

                    // Check trade size limit
                    if trade_value > self.config.max_trade_size {
                        warn!(
                            "Trade size ${} exceeds limit ${}",
                            trade_value, self.config.max_trade_size
                        );
                        continue;
                    }

                    // Check total exposure limit
                    if current_exposure + trade_value > self.config.max_total_exposure {
                        warn!(
                            "Would exceed total exposure limit: current=${}, trade=${}, limit=${}",
                            current_exposure, trade_value, self.config.max_total_exposure
                        );
                        continue;
                    }

                    valid_actions.push(action.clone());
                }
                AgentAction::ExitPosition { .. } => {
                    if self.config.require_exit_confirmation
                        && self.config.autonomy_level != AutonomyLevel::FullAutonomy
                    {
                        // Convert to alert for confirmation
                        valid_actions.push(AgentAction::Alert {
                            severity: "warning".to_string(),
                            message: "Exit recommended but requires confirmation".to_string(),
                        });
                    } else {
                        valid_actions.push(action.clone());
                    }
                }
                AgentAction::Wait { .. }
                | AgentAction::Alert { .. }
                | AgentAction::NoAction { .. } => {
                    valid_actions.push(action.clone());
                }
                AgentAction::AdjustRisk { new_state, reasoning: _ } => {
                    // Only allow risk increases (more conservative)
                    if *new_state == RiskState::Halted
                        || (*new_state == RiskState::Elevated
                            && context.risk_state == RiskState::Normal)
                    {
                        valid_actions.push(action.clone());
                    } else {
                        debug!("Ignoring risk reduction: {:?}", new_state);
                    }
                }
                _ => {
                    // Other actions passed through
                    valid_actions.push(action.clone());
                }
            }
        }

        Ok(valid_actions)
    }

    /// Execute a validated action
    async fn execute_action(
        &self,
        action: &AgentAction,
        _context: &AgentContext,
    ) -> Result<ExecutionResult> {
        match action {
            AgentAction::EnterPosition {
                side,
                shares,
                max_price,
                reasoning,
            } => {
                info!(
                    "Executing entry: {:?} {} shares @ max ${}: {}",
                    side, shares, max_price, reasoning
                );

                // TODO: Integrate with actual order executor
                // For now, just track the exposure
                let trade_value = Decimal::from(*shares) * *max_price;
                *self.current_exposure.write().await += trade_value;

                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: format!("Entered {:?} position", side),
                    trade_id: Some(format!("agent-{}", chrono::Utc::now().timestamp())),
                })
            }
            AgentAction::ExitPosition {
                token_id,
                min_price,
                reasoning,
            } => {
                info!(
                    "Executing exit: {} @ min {:?}: {}",
                    token_id, min_price, reasoning
                );

                // TODO: Integrate with actual order executor
                // Reduce exposure (simplified)
                let current = *self.current_exposure.read().await;
                *self.current_exposure.write().await = current * Decimal::from(80) / Decimal::from(100);

                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: format!("Exited position {}", token_id),
                    trade_id: Some(format!("agent-exit-{}", chrono::Utc::now().timestamp())),
                })
            }
            AgentAction::Wait { duration_secs, reason } => {
                debug!("Agent waiting {} seconds: {}", duration_secs, reason);
                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: format!("Waiting: {}", reason),
                    trade_id: None,
                })
            }
            AgentAction::Alert { severity, message } => {
                match severity.as_str() {
                    "critical" => error!("[AGENT ALERT] {}", message),
                    "warning" => warn!("[AGENT ALERT] {}", message),
                    _ => info!("[AGENT ALERT] {}", message),
                }
                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: format!("Alert sent: {}", message),
                    trade_id: None,
                })
            }
            AgentAction::AdjustRisk { new_state, reasoning } => {
                warn!("Risk adjustment to {:?}: {}", new_state, reasoning);
                // TODO: Integrate with RiskManager
                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: format!("Risk adjusted to {:?}", new_state),
                    trade_id: None,
                })
            }
            AgentAction::NoAction { reason } => {
                debug!("No action: {}", reason);
                Ok(ExecutionResult {
                    action: action.clone(),
                    success: true,
                    message: reason.clone(),
                    trade_id: None,
                })
            }
            _ => Ok(ExecutionResult {
                action: action.clone(),
                success: false,
                message: "Action type not implemented".to_string(),
                trade_id: None,
            }),
        }
    }

    /// Get current exposure
    pub async fn current_exposure(&self) -> Decimal {
        *self.current_exposure.read().await
    }

    /// Get configuration
    pub fn config(&self) -> &AutonomousConfig {
        &self.config
    }

    /// Update configuration
    pub fn set_config(&mut self, config: AutonomousConfig) {
        self.config = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AutonomousConfig::default();
        assert_eq!(config.autonomy_level, AutonomyLevel::AdvisoryOnly);
        assert!(!config.trading_enabled);
    }

    #[test]
    fn test_conservative_config() {
        let config = AutonomousConfig::conservative();
        assert_eq!(config.autonomy_level, AutonomyLevel::LimitedAutonomy);
        assert!(config.trading_enabled);
    }

    #[test]
    fn test_can_trade() {
        let client = ClaudeAgentClient::new();
        let mut agent = AutonomousAgent::new(client, AutonomousConfig::default());
        assert!(!agent.can_trade());

        agent.set_config(AutonomousConfig::conservative());
        assert!(agent.can_trade());
    }
}
