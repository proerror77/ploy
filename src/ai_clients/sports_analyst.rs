//! Sports Event Analyst - Combines Polymarket, Grok, and Claude for sports betting decisions
//!
//! Workflow:
//! 1. Parse Polymarket event URL to extract teams
//! 2. Use SportsDataFetcher (Grok) to get structured JSON data:
//!    - Player stats, injuries (fixed JSON format)
//!    - Betting lines from sportsbooks
//!    - Public sentiment and expert picks
//! 3. Format structured data and send to Claude Opus for analysis
//! 4. Generate trade recommendation based on edge detection

use crate::adapters::polymarket_clob::{GammaMarketInfo, MarketSummary as GammaMarketSummary};
use crate::adapters::{GammaEventInfo, PolymarketClient};
use crate::ai_clients::client::{AgentClientConfig, ClaudeAgentClient};
use crate::ai_clients::grok::GrokClient;
use crate::ai_clients::sports_data::{SportsDataFetcher, StructuredGameData, format_for_claude};
use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

mod market_odds;
mod url_parsing;

/// Sports event analysis result
#[derive(Debug, Clone)]
pub struct SportsAnalysis {
    /// Event identifier
    pub event_slug: String,
    /// Teams involved
    pub teams: (String, String),
    /// League (NBA, NFL, etc.)
    pub league: String,
    /// Structured game data from Grok (players, betting, sentiment)
    pub structured_data: Option<StructuredGameData>,
    /// Market odds from Polymarket (fallback)
    pub market_odds: MarketOdds,
    /// Claude's win probability prediction
    pub prediction: WinPrediction,
    /// Recommended action
    pub recommendation: TradeRecommendation,
}

/// Market odds from Polymarket
#[derive(Debug, Clone)]
pub struct MarketOdds {
    pub team1_yes_price: Decimal,
    pub team1_no_price: Decimal,
    pub team2_yes_price: Option<Decimal>,
    pub team2_no_price: Option<Decimal>,
    pub spread: Option<String>,
}

/// Win probability prediction from Claude
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WinPrediction {
    pub team1_win_prob: f64,
    pub team2_win_prob: f64,
    pub confidence: f64,
    pub reasoning: String,
    pub key_factors: Vec<String>,
}

/// Trade recommendation
#[derive(Debug, Clone)]
pub struct TradeRecommendation {
    pub action: TradeAction,
    pub side: String,
    pub edge: f64,
    pub suggested_size: Decimal,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradeAction {
    Buy,
    Sell,
    Hold,
    Avoid,
}

/// Sports Event Analyst - Uses structured data workflow
pub struct SportsAnalyst {
    data_fetcher: SportsDataFetcher,
    claude: ClaudeAgentClient,
}

const CLOB_BASE_URL: &str = "https://clob.polymarket.com";

impl SportsAnalyst {
    /// Create a new sports analyst with Grok and Claude
    pub fn new(grok: GrokClient, claude: ClaudeAgentClient) -> Self {
        let data_fetcher = SportsDataFetcher::new(grok);
        Self {
            data_fetcher,
            claude,
        }
    }

    /// Create from environment with Opus model for decision making
    pub fn from_env() -> Result<Self> {
        use crate::ai_clients::grok::GrokConfig;

        let grok = GrokClient::new(GrokConfig::from_env())?;
        if !grok.is_configured() {
            return Err(PloyError::Internal("GROK_API_KEY not configured".into()));
        }

        let data_fetcher = SportsDataFetcher::new(grok);

        // Use longer timeout and Opus model for complex sports analysis
        let mut config = AgentClientConfig::for_autonomous().with_timeout(300); // 5 minutes for detailed analysis
        config.model =
            Some(std::env::var("PLOY_CLAUDE_MODEL").unwrap_or_else(|_| "opus".to_string()));
        let claude = ClaudeAgentClient::with_config(config);

        Ok(Self {
            data_fetcher,
            claude,
        })
    }

    /// Analyze a sports event from Polymarket URL
    /// URL format: https://polymarket.com/event/nba-phi-dal-2026-01-01
    pub async fn analyze_event(&self, event_url: &str) -> Result<SportsAnalysis> {
        // 1. Parse event URL to extract slug, teams, and league
        let (event_slug, league, team1, team2) = self.parse_event_url(event_url)?;
        info!(
            "Analyzing {} event: {} vs {}",
            league.to_uppercase(),
            team1,
            team2
        );

        // 2. Fetch structured data from Grok (player stats, betting lines, sentiment)
        info!("Fetching structured game data via Grok...");
        let structured_data = match self
            .data_fetcher
            .fetch_game_data(&team1, &team2, &league)
            .await
        {
            Ok(data) => {
                info!(
                    "Got structured data: {} {} players, {} {} players",
                    data.team1_players.len(),
                    team1,
                    data.team2_players.len(),
                    team2
                );
                info!(
                    "Betting: {} {} spread, O/U {}",
                    data.betting_lines.spread_team,
                    data.betting_lines.spread,
                    data.betting_lines.over_under
                );
                info!(
                    "Sentiment: {} pick at {:.0}% confidence",
                    data.sentiment.expert_pick,
                    data.sentiment.expert_confidence * 100.0
                );
                Some(data)
            }
            Err(e) => {
                warn!(
                    "Failed to fetch structured data: {}, will use Polymarket odds only",
                    e
                );
                None
            }
        };

        // 3. Also fetch market data from Polymarket for comparison
        let market_odds = self.fetch_market_odds(&event_slug, &team1, &team2).await?;
        info!(
            "Polymarket odds: {} @ {:.3}",
            team1, market_odds.team1_yes_price
        );

        // 4. Send structured data to Claude Opus for win probability analysis
        info!("Sending to Claude Opus for analysis...");
        let prediction = self
            .get_claude_prediction(&team1, &team2, &market_odds, structured_data.as_ref())
            .await?;
        info!(
            "Claude prediction: {} {:.1}% vs {} {:.1}% (confidence: {:.0}%)",
            team1,
            prediction.team1_win_prob * 100.0,
            team2,
            prediction.team2_win_prob * 100.0,
            prediction.confidence * 100.0
        );

        // 5. Generate trade recommendation based on edge
        let recommendation =
            self.generate_recommendation(&team1, &team2, &market_odds, &prediction);

        Ok(SportsAnalysis {
            event_slug,
            teams: (team1, team2),
            league,
            structured_data,
            market_odds,
            prediction,
            recommendation,
        })
    }

    /// Fetch market odds from Polymarket
    /// Tries multiple strategies: slug query, team name search, matchup search
    /// Get Claude Opus prediction using structured game data
    async fn get_claude_prediction(
        &self,
        team1: &str,
        team2: &str,
        odds: &MarketOdds,
        structured_data: Option<&StructuredGameData>,
    ) -> Result<WinPrediction> {
        // Format structured data for Claude, or use minimal format
        let data_section = match structured_data {
            Some(data) => format_for_claude(data),
            None => format!(
                "## Game: {} vs {}\n\
                (No structured data available - using Polymarket odds only)\n",
                team1, team2
            ),
        };

        let prompt = format!(
            r#"You are an expert sports analyst. Analyze this matchup and predict win probabilities.

{data_section}

## Polymarket Odds (for comparison)
{team1} YES: {:.3} (implied {:.1}%)
{team2} YES: {:.3} (implied {:.1}%)

## Your Task
Analyze ALL the structured data above carefully:
1. Player availability and recent performance
2. Betting line consensus and movement
3. Expert picks and public sentiment
4. Compare your analysis to market odds

Provide your prediction in this EXACT JSON format (no other text):
```json
{{
  "team1_win_prob": 0.XX,
  "team2_win_prob": 0.XX,
  "confidence": 0.XX,
  "reasoning": "2-3 sentence explanation of key factors",
  "key_factors": ["factor1", "factor2", "factor3"]
}}
```

IMPORTANT:
- team1_win_prob + team2_win_prob MUST equal 1.0
- confidence is 0.0-1.0 (how sure you are)
- Be specific in reasoning - cite actual player data or betting line movements"#,
            odds.team1_yes_price,
            odds.team1_yes_price
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.5)
                * 100.0,
            odds.team2_yes_price.unwrap_or(Decimal::new(50, 2)),
            odds.team2_yes_price
                .map(|p| p.to_string().parse::<f64>().unwrap_or(0.5) * 100.0)
                .unwrap_or(50.0),
            data_section = data_section,
            team1 = team1,
            team2 = team2,
        );

        // Query Claude using simple_query (returns raw text without parsing into AgentResponse)
        let response = self.claude.simple_query(&prompt).await?;

        // Parse prediction from raw response
        self.parse_prediction_response(&response, team1, team2)
    }

    /// Parse Claude's prediction response
    fn parse_prediction_response(
        &self,
        response: &str,
        team1: &str,
        team2: &str,
    ) -> Result<WinPrediction> {
        // Try to extract JSON from response
        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                let json_str = &response[start..=end];
                if let Ok(pred) = serde_json::from_str::<WinPrediction>(json_str) {
                    return Ok(pred);
                }
            }
        }

        // Fallback: return neutral prediction
        Ok(WinPrediction {
            team1_win_prob: 0.5,
            team2_win_prob: 0.5,
            confidence: 0.5,
            reasoning: format!(
                "Could not parse detailed prediction for {} vs {}",
                team1, team2
            ),
            key_factors: vec!["Insufficient data".to_string()],
        })
    }

    /// Generate trade recommendation based on edge
    fn generate_recommendation(
        &self,
        team1: &str,
        team2: &str,
        odds: &MarketOdds,
        prediction: &WinPrediction,
    ) -> TradeRecommendation {
        let market_prob = odds
            .team1_yes_price
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.5);
        let predicted_prob = prediction.team1_win_prob;
        let edge = predicted_prob - market_prob;

        // Minimum edge threshold (5%)
        const MIN_EDGE: f64 = 0.05;
        // Confidence threshold
        const MIN_CONFIDENCE: f64 = 0.7;

        // Calculate edge for team2 (inverse of team1 edge)
        let team2_edge = -edge; // If team1 edge is -8%, team2 edge is +8%

        let (action, side, display_edge, reasoning) = if prediction.confidence < MIN_CONFIDENCE {
            (
                TradeAction::Avoid,
                "None".to_string(),
                0.0,
                format!("Confidence too low ({:.0}%)", prediction.confidence * 100.0),
            )
        } else if edge > MIN_EDGE {
            // Team1 is undervalued
            (
                TradeAction::Buy,
                format!("{} YES", team1),
                edge * 100.0,
                format!(
                    "Predicted {:.1}% vs market {:.1}% = {:.1}% edge",
                    predicted_prob * 100.0,
                    market_prob * 100.0,
                    edge * 100.0
                ),
            )
        } else if edge < -MIN_EDGE {
            // Team2 is undervalued (show positive edge)
            (
                TradeAction::Buy,
                format!("{} YES", team2),
                team2_edge * 100.0, // Show positive edge for team2
                format!(
                    "{} undervalued: predicted {:.1}% vs market {:.1}%",
                    team2,
                    prediction.team2_win_prob * 100.0,
                    (1.0 - market_prob) * 100.0
                ),
            )
        } else {
            (
                TradeAction::Hold,
                "None".to_string(),
                edge.abs() * 100.0,
                format!("No significant edge detected ({:.1}%)", edge.abs() * 100.0),
            )
        };

        // Calculate suggested size based on Kelly criterion (simplified)
        let kelly_fraction = if edge.abs() > MIN_EDGE && prediction.confidence >= MIN_CONFIDENCE {
            (edge.abs() * prediction.confidence).min(0.1) // Max 10% of bankroll
        } else {
            0.0
        };

        TradeRecommendation {
            action,
            side,
            edge: display_edge,
            suggested_size: Decimal::from_f64_retain(kelly_fraction * 100.0)
                .unwrap_or(Decimal::ZERO), // As percentage
            reasoning,
        }
    }

    /// Analyze with DraftKings odds comparison
    pub async fn analyze_with_draftkings(&self, event_url: &str) -> Result<SportsAnalysisWithDK> {
        use crate::ai_clients::odds_provider::{OddsProvider, Sport};

        // Get base analysis first
        let analysis = self.analyze_event(event_url).await?;

        // Try to get DraftKings odds for comparison
        let dk_comparison = match OddsProvider::from_env() {
            Ok(provider) => {
                let sport = match analysis.league.to_uppercase().as_str() {
                    "NBA" => Sport::NBA,
                    "NFL" => Sport::NFL,
                    "NHL" => Sport::NHL,
                    "MLB" => Sport::MLB,
                    _ => Sport::NBA, // Default to NBA
                };

                // Get predicted probability from Claude
                let predicted_home_prob =
                    Decimal::from_f64_retain(analysis.prediction.team1_win_prob)
                        .unwrap_or(Decimal::new(50, 2));

                match provider
                    .compare_with_prediction(
                        sport,
                        &analysis.teams.0,
                        &analysis.teams.1,
                        predicted_home_prob,
                    )
                    .await
                {
                    Ok(Some(edge)) => Some(edge),
                    Ok(None) => {
                        warn!(
                            "No DraftKings odds found for {} vs {}",
                            analysis.teams.0, analysis.teams.1
                        );
                        None
                    }
                    Err(e) => {
                        warn!("Failed to fetch DraftKings odds: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("DraftKings odds provider not configured: {}", e);
                None
            }
        };

        Ok(SportsAnalysisWithDK {
            base: analysis,
            draftkings: dk_comparison,
        })
    }
}

/// Sports analysis with DraftKings odds comparison
#[derive(Debug, Clone)]
pub struct SportsAnalysisWithDK {
    pub base: SportsAnalysis,
    pub draftkings: Option<crate::ai_clients::odds_provider::EdgeAnalysis>,
}

impl SportsAnalysisWithDK {
    /// Get the best edge across all sources
    pub fn best_edge(&self) -> (String, f64) {
        let pm_edge = self.base.recommendation.edge;

        if let Some(ref dk) = self.draftkings {
            let dk_edge = dk.edge.to_string().parse::<f64>().unwrap_or(0.0) * 100.0;

            if dk_edge.abs() > pm_edge.abs() {
                return (format!("DraftKings - {}", dk.recommended_side), dk_edge);
            }
        }

        (self.base.recommendation.side.clone(), pm_edge)
    }

    /// Check if there's arbitrage opportunity between PM and DK
    pub fn has_arbitrage(&self) -> bool {
        if let Some(ref dk) = self.draftkings {
            // Check if PM and DK have opposite signals
            let pm_favors_team1 = self.base.recommendation.edge > 0.0;
            let dk_favors_team1 = dk.home_edge > dk.away_edge;

            // If they disagree and both have significant edge, potential arb
            if pm_favors_team1 != dk_favors_team1 {
                let pm_edge = self.base.recommendation.edge.abs();
                let dk_edge = dk.edge.to_string().parse::<f64>().unwrap_or(0.0).abs() * 100.0;

                return pm_edge > 3.0 && dk_edge > 3.0;
            }
        }
        false
    }
}
