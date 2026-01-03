//! Claude Agent client using subprocess communication
//!
//! Communicates with Claude via the `claude` CLI tool using subprocess calls.
//! This approach provides isolation and leverages the existing Claude Code CLI.

use crate::agent::protocol::{AgentContext, AgentResponse};
use crate::error::{PloyError, Result};
use serde::Deserialize;
use serde_json;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Configuration for the Claude agent client
#[derive(Debug, Clone)]
pub struct AgentClientConfig {
    /// Path to the claude CLI executable
    pub cli_path: String,
    /// Timeout for agent responses
    pub timeout: Duration,
    /// Maximum retries on failure
    pub max_retries: u8,
    /// Model to use (e.g., "claude-3-opus", "claude-3-sonnet")
    pub model: Option<String>,
    /// System prompt for trading context
    pub system_prompt: Option<String>,
}

impl Default for AgentClientConfig {
    fn default() -> Self {
        Self {
            cli_path: "claude".to_string(),
            timeout: Duration::from_secs(120), // 2 minutes default
            max_retries: 2,
            model: None,
            system_prompt: Some(DEFAULT_TRADING_SYSTEM_PROMPT.to_string()),
        }
    }
}

impl AgentClientConfig {
    /// Create config for autonomous mode with longer timeout
    pub fn for_autonomous() -> Self {
        Self {
            cli_path: "claude".to_string(),
            timeout: Duration::from_secs(180), // 3 minutes for complex analysis
            max_retries: 3,
            model: None,
            system_prompt: Some(DEFAULT_TRADING_SYSTEM_PROMPT.to_string()),
        }
    }

    /// Set custom timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self
    }
}

/// Default system prompt for trading operations
const DEFAULT_TRADING_SYSTEM_PROMPT: &str = r#"You are an AI trading assistant for a Polymarket prediction market trading system.

Your role is to analyze market conditions, assess risks, and provide trading recommendations.

When analyzing markets, consider:
1. Current prices and spreads
2. Time remaining until market settlement
3. Arbitrage opportunities (sum of YES ask + NO ask < 1 or sum of bids > 1)
4. Recent price movements and volatility
5. Current positions and exposure
6. Daily P&L and risk limits

Always provide:
- Clear reasoning for your recommendations
- Confidence level (0-100%)
- Specific, actionable recommendations
- Risk assessment when relevant

Respond in JSON format matching the AgentResponse schema."#;

/// Claude agent client for subprocess communication
pub struct ClaudeAgentClient {
    config: AgentClientConfig,
}

impl ClaudeAgentClient {
    /// Create a new client with default configuration
    pub fn new() -> Self {
        Self {
            config: AgentClientConfig::default(),
        }
    }

    /// Create a new client with custom configuration
    pub fn with_config(config: AgentClientConfig) -> Self {
        Self { config }
    }

    /// Check if the claude CLI is available
    pub async fn check_availability(&self) -> Result<bool> {
        let output = Command::new(&self.config.cli_path)
            .arg("--version")
            .output()
            .await;

        match output {
            Ok(out) => {
                if out.status.success() {
                    let version = String::from_utf8_lossy(&out.stdout);
                    info!("Claude CLI available: {}", version.trim());
                    Ok(true)
                } else {
                    warn!("Claude CLI returned error status");
                    Ok(false)
                }
            }
            Err(e) => {
                error!("Claude CLI not found at '{}': {}", self.config.cli_path, e);
                Ok(false)
            }
        }
    }

    /// Query the Claude agent with context
    pub async fn query(&self, prompt: &str, context: &AgentContext) -> Result<AgentResponse> {
        let mut attempts = 0;
        let mut last_error = None;

        while attempts < self.config.max_retries {
            attempts += 1;

            match self.execute_query(prompt, context).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    warn!("Agent query attempt {} failed: {}", attempts, e);
                    last_error = Some(e);

                    if attempts < self.config.max_retries {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            PloyError::Internal("Agent query failed with unknown error".to_string())
        }))
    }

    /// Execute a single query attempt
    async fn execute_query(&self, prompt: &str, context: &AgentContext) -> Result<AgentResponse> {
        // Build the full prompt with context
        let context_json = serde_json::to_string_pretty(context)?;
        let full_prompt = format!(
            r#"## Current Trading Context

```json
{}
```

## Request

{}

## Instructions

Analyze the context and provide your response in JSON format matching this schema:
{{
    "reasoning": "Your chain of thought analysis",
    "confidence": 0.0 to 1.0,
    "recommended_actions": [...],
    "risk_assessment": {{ ... }} or null,
    "summary": "Brief summary"
}}

Respond ONLY with valid JSON."#,
            context_json, prompt
        );

        // Build command arguments
        let mut cmd = Command::new(&self.config.cli_path);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("text")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref model) = self.config.model {
            cmd.arg("--model").arg(model);
        }

        // Add system prompt if configured
        if let Some(ref system_prompt) = self.config.system_prompt {
            cmd.arg("--system-prompt").arg(system_prompt);
        }

        debug!("Spawning claude process");
        let mut child = cmd.spawn().map_err(|e| {
            PloyError::Internal(format!("Failed to spawn claude process: {}", e))
        })?;

        // Write prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(full_prompt.as_bytes()).await.map_err(|e| {
                PloyError::Internal(format!("Failed to write to claude stdin: {}", e))
            })?;
        }

        // Wait for response with timeout
        let output = timeout(self.config.timeout, child.wait_with_output())
            .await
            .map_err(|_| PloyError::Internal("Agent query timed out".to_string()))?
            .map_err(|e| PloyError::Internal(format!("Failed to get claude output: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PloyError::Internal(format!(
                "Claude process failed: {}",
                stderr
            )));
        }

        let response_text = String::from_utf8_lossy(&output.stdout);
        debug!("Raw agent response: {}", response_text);

        // Extract JSON from response (may have markdown code blocks)
        let json_str = extract_json(&response_text);

        // Parse response using flexible parser
        parse_flexible_response(json_str)
    }

    /// Send a simple query without full context
    pub async fn simple_query(&self, prompt: &str) -> Result<String> {
        let mut cmd = Command::new(&self.config.cli_path);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("text")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            PloyError::Internal(format!("Failed to spawn claude process: {}", e))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await.map_err(|e| {
                PloyError::Internal(format!("Failed to write to claude stdin: {}", e))
            })?;
        }

        let output = timeout(self.config.timeout, child.wait_with_output())
            .await
            .map_err(|_| PloyError::Internal("Query timed out".to_string()))?
            .map_err(|e| PloyError::Internal(format!("Failed to get output: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PloyError::Internal(format!("Claude failed: {}", stderr)));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Default for ClaudeAgentClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract JSON from a response that may contain markdown code blocks
fn extract_json(text: &str) -> &str {
    // Try to find JSON in code blocks first
    if let Some(start) = text.find("```json") {
        if let Some(end) = text[start + 7..].find("```") {
            return text[start + 7..start + 7 + end].trim();
        }
    }

    // Try generic code blocks
    if let Some(start) = text.find("```") {
        if let Some(end) = text[start + 3..].find("```") {
            let content = text[start + 3..start + 3 + end].trim();
            // Skip language identifier if present
            if let Some(newline) = content.find('\n') {
                return content[newline + 1..].trim();
            }
            return content;
        }
    }

    // Try to find raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            return &text[start..=end];
        }
    }

    text.trim()
}

/// Flexible response from Claude that can be converted to AgentResponse
#[derive(Debug, Deserialize)]
struct FlexibleAgentResponse {
    reasoning: String,
    confidence: f64,
    #[serde(default)]
    recommended_actions: Vec<serde_json::Value>,
    #[serde(default)]
    risk_assessment: Option<serde_json::Value>,
    summary: String,
}

use crate::agent::protocol::AgentAction;

/// Parse a flexible response from Claude into our AgentResponse type
fn parse_flexible_response(json_str: &str) -> Result<AgentResponse> {
    // First try direct parsing
    if let Ok(response) = serde_json::from_str::<AgentResponse>(json_str) {
        return Ok(response);
    }

    // Try flexible parsing
    let flexible: FlexibleAgentResponse = serde_json::from_str(json_str)
        .map_err(|e| PloyError::Internal(format!("Failed to parse agent response: {}", e)))?;

    // Convert recommended_actions to AgentAction
    let actions: Vec<AgentAction> = flexible.recommended_actions.iter()
        .filter_map(|v| convert_to_agent_action(v))
        .collect();

    // If no valid actions were parsed, create a NoAction from the summary
    let actions = if actions.is_empty() {
        vec![AgentAction::Alert {
            severity: "info".to_string(),
            message: flexible.summary.clone(),
        }]
    } else {
        actions
    };

    Ok(AgentResponse {
        reasoning: flexible.reasoning,
        confidence: flexible.confidence,
        recommended_actions: actions,
        risk_assessment: None, // Could parse this more thoroughly if needed
        summary: flexible.summary,
        raw_response: Some(json_str.to_string()),
    })
}

/// Convert a flexible JSON value to an AgentAction
fn convert_to_agent_action(value: &serde_json::Value) -> Option<AgentAction> {
    let obj = value.as_object()?;

    // Extract action type (could be "action", "type", or infer from fields)
    let action_type = obj.get("action")
        .or_else(|| obj.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_uppercase();

    let details = obj.get("details")
        .or_else(|| obj.get("message"))
        .or_else(|| obj.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("No details provided")
        .to_string();

    let priority = obj.get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("MEDIUM");

    // Map common action types to AgentAction
    match action_type.as_str() {
        "ENTER" | "ENTER_POSITION" | "BUY" => {
            // Would need more details to construct EnterPosition
            Some(AgentAction::Alert {
                severity: priority_to_severity(priority),
                message: format!("Entry signal: {}", details),
            })
        }
        "EXIT" | "EXIT_POSITION" | "SELL" => {
            Some(AgentAction::Alert {
                severity: priority_to_severity(priority),
                message: format!("Exit signal: {}", details),
            })
        }
        "WAIT" | "HOLD" | "NO_TRADE" => {
            Some(AgentAction::Wait {
                duration_secs: 60,
                reason: details,
            })
        }
        "ALERT" | "WARNING" | "CRITICAL" => {
            Some(AgentAction::Alert {
                severity: priority_to_severity(priority),
                message: details,
            })
        }
        "NO_ACTION" | "NONE" => {
            Some(AgentAction::NoAction { reason: details })
        }
        _ => {
            // Default: create an alert with the information
            Some(AgentAction::Alert {
                severity: priority_to_severity(priority),
                message: format!("{}: {}", action_type, details),
            })
        }
    }
}

fn priority_to_severity(priority: &str) -> String {
    match priority.to_uppercase().as_str() {
        "CRITICAL" | "HIGH" => "warning".to_string(),
        "LOW" => "info".to_string(),
        _ => "info".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_code_block() {
        let text = r#"Here's my analysis:

```json
{"reasoning": "test", "confidence": 0.9}
```

That's my recommendation."#;

        let json = extract_json(text);
        assert!(json.starts_with('{'));
        assert!(json.contains("reasoning"));
    }

    #[test]
    fn test_extract_json_raw() {
        let text = r#"{"reasoning": "test", "confidence": 0.9}"#;
        let json = extract_json(text);
        assert_eq!(json, text);
    }

    #[test]
    fn test_default_config() {
        let config = AgentClientConfig::default();
        assert_eq!(config.cli_path, "claude");
        assert_eq!(config.max_retries, 2);
    }
}
