//! Grok API client for real-time search and market intelligence
//!
//! Provides integration with xAI's Grok API for:
//! - Real-time X (Twitter) search
//! - Market sentiment analysis
//! - News and event detection

use crate::error::{PloyError, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Grok API client configuration
#[derive(Debug, Clone)]
pub struct GrokConfig {
    /// API key for Grok
    pub api_key: String,
    /// API base URL
    pub base_url: String,
    /// Request timeout
    pub timeout_secs: u64,
    /// Model to use
    pub model: String,
}

impl Default for GrokConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.x.ai/v1".to_string(),
            timeout_secs: 30,
            model: "grok-4-1-fast-reasoning".to_string(),
        }
    }
}

impl GrokConfig {
    pub fn from_env() -> Self {
        Self {
            api_key: std::env::var("GROK_API_KEY").unwrap_or_default(),
            base_url: std::env::var("GROK_API_URL")
                .unwrap_or_else(|_| "https://api.x.ai/v1".to_string()),
            timeout_secs: 30,
            model: std::env::var("GROK_MODEL")
                .unwrap_or_else(|_| "grok-4-1-fast-reasoning".to_string()),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty()
    }
}

/// Grok API message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokMessage {
    pub role: String,
    pub content: String,
}

/// Grok API request
#[derive(Debug, Clone, Serialize)]
struct GrokRequest {
    model: String,
    messages: Vec<GrokMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Enable real-time search capability
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<bool>,
}

/// Grok API response
#[derive(Debug, Clone, Deserialize)]
struct GrokResponse {
    choices: Vec<GrokChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct GrokChoice {
    message: GrokMessage,
}

/// Search result from Grok
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub query: String,
    pub summary: String,
    pub sentiment: Option<Sentiment>,
    pub key_points: Vec<String>,
}

/// Market sentiment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sentiment {
    Bullish,
    Bearish,
    Neutral,
    Mixed,
}

impl std::fmt::Display for Sentiment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sentiment::Bullish => write!(f, "bullish"),
            Sentiment::Bearish => write!(f, "bearish"),
            Sentiment::Neutral => write!(f, "neutral"),
            Sentiment::Mixed => write!(f, "mixed"),
        }
    }
}

/// Grok API client
pub struct GrokClient {
    config: GrokConfig,
    http: Client,
    /// Timestamp of last API call for rate limiting
    last_call: Arc<Mutex<Option<Instant>>>,
    /// Minimum interval between API calls (~10 req/min)
    min_interval: Duration,
}

impl GrokClient {
    /// Create a new Grok client
    pub fn new(config: GrokConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| PloyError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            config,
            http,
            last_call: Arc::new(Mutex::new(None)),
            min_interval: Duration::from_secs(6),
        })
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        Self::new(GrokConfig::from_env())
    }

    /// Check if client is properly configured
    pub fn is_configured(&self) -> bool {
        self.config.is_configured()
    }

    /// Enforce minimum interval between API calls
    async fn rate_limit(&self) {
        let mut last = self.last_call.lock().await;
        if let Some(last_time) = *last {
            let elapsed = last_time.elapsed();
            if elapsed < self.min_interval {
                let wait = self.min_interval - elapsed;
                debug!("Grok rate limit: waiting {:?}", wait);
                tokio::time::sleep(wait).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// Search for real-time information about a topic
    pub async fn search(&self, query: &str) -> Result<SearchResult> {
        self.rate_limit().await;

        if !self.is_configured() {
            return Err(PloyError::Internal("Grok API key not configured".to_string()));
        }

        let prompt = format!(
            r#"Search for the latest real-time information about: {}

Please provide:
1. A brief summary of the current situation (2-3 sentences)
2. Overall market sentiment (bullish/bearish/neutral/mixed)
3. Key points or news items (bullet points)

Focus on information from the last few hours that could affect trading decisions."#,
            query
        );

        let response = self.chat(&prompt).await?;
        
        // Parse sentiment from response
        let sentiment = if response.to_lowercase().contains("bullish") {
            Some(Sentiment::Bullish)
        } else if response.to_lowercase().contains("bearish") {
            Some(Sentiment::Bearish)
        } else if response.to_lowercase().contains("mixed") {
            Some(Sentiment::Mixed)
        } else {
            Some(Sentiment::Neutral)
        };

        Ok(SearchResult {
            query: query.to_string(),
            summary: response.clone(),
            sentiment,
            key_points: extract_bullet_points(&response),
        })
    }

    /// Search for market-specific news
    pub async fn search_market(&self, asset: &str, timeframe: &str) -> Result<SearchResult> {
        self.rate_limit().await;

        let query = format!(
            "{} price prediction {} - latest news, sentiment, and market analysis",
            asset, timeframe
        );
        self.search(&query).await
    }

    /// Get sentiment analysis for a specific topic
    pub async fn analyze_sentiment(&self, topic: &str) -> Result<Sentiment> {
        self.rate_limit().await;

        if !self.is_configured() {
            return Err(PloyError::Internal("Grok API key not configured".to_string()));
        }

        let prompt = format!(
            r#"Analyze the current market sentiment for: {}

Based on recent social media posts, news, and market activity, what is the overall sentiment?

Respond with exactly one word: bullish, bearish, neutral, or mixed"#,
            topic
        );

        let response = self.chat(&prompt).await?;
        let response_lower = response.to_lowercase().trim().to_string();

        Ok(if response_lower.contains("bullish") {
            Sentiment::Bullish
        } else if response_lower.contains("bearish") {
            Sentiment::Bearish
        } else if response_lower.contains("mixed") {
            Sentiment::Mixed
        } else {
            Sentiment::Neutral
        })
    }

    /// Send a chat message to Grok
    pub async fn chat(&self, prompt: &str) -> Result<String> {
        if !self.is_configured() {
            return Err(PloyError::Internal("Grok API key not configured".to_string()));
        }

        debug!("Sending request to Grok API");

        let request = GrokRequest {
            model: self.config.model.clone(),
            messages: vec![GrokMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            temperature: Some(0.7),
            max_tokens: Some(1000),
            search: Some(true), // Enable real-time search
        };

        let url = format!("{}/chat/completions", self.config.base_url);
        
        let response = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| PloyError::Http(e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let truncated_body: String = body.chars().take(200).collect();
            warn!("Grok API error: {} - {}", status, truncated_body);
            return Err(PloyError::Internal(format!(
                "Grok API error: {} - {}",
                status, truncated_body
            )));
        }

        let grok_response: GrokResponse = response
            .json()
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to parse Grok response: {}", e)))?;

        let content = grok_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        debug!("Grok response received: {} chars", content.len());
        Ok(content)
    }
}

/// Extract bullet points from a text response
fn extract_bullet_points(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('-') 
                || trimmed.starts_with('•') 
                || trimmed.starts_with('*')
                || (trimmed.len() > 2 && trimmed.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) && trimmed.chars().nth(1) == Some('.'))
        })
        .map(|line| {
            line.trim()
                .trim_start_matches(|c: char| c == '-' || c == '•' || c == '*' || c.is_ascii_digit() || c == '.')
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_env() {
        let config = GrokConfig::from_env();
        assert_eq!(config.base_url, "https://api.x.ai/v1");
        assert_eq!(config.model, "grok-4-1-fast-reasoning");
    }

    #[test]
    fn test_extract_bullet_points() {
        let text = r#"
Summary of events:
- First point here
- Second point here
• Third point with bullet
1. Numbered point
2. Another numbered
Regular text not a bullet
"#;
        let points = extract_bullet_points(text);
        assert_eq!(points.len(), 5);
        assert_eq!(points[0], "First point here");
    }

    #[test]
    fn test_sentiment_display() {
        assert_eq!(Sentiment::Bullish.to_string(), "bullish");
        assert_eq!(Sentiment::Bearish.to_string(), "bearish");
    }
}
