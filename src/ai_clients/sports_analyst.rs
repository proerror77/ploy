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

mod analysis_outcome;
mod market_odds;
mod url_parsing;

pub use analysis_outcome::SportsAnalysisWithDK;

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
}
