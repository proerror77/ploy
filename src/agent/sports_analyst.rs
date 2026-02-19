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
use crate::agent::client::{AgentClientConfig, ClaudeAgentClient};
use crate::agent::grok::GrokClient;
use crate::agent::sports_data::{format_for_claude, SportsDataFetcher, StructuredGameData};
use crate::error::{PloyError, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

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
        use crate::agent::grok::GrokConfig;

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

    /// Parse event URL to extract slug, league, and team names
    /// Supports two URL formats:
    /// - Short: https://polymarket.com/event/nba-phi-dal-2026-01-01
    /// - Long: https://polymarket.com/event/nba-regular-season-2024-2025/philadelphia-76ers-vs-dallas-mavericks-jan-2-2025
    fn parse_event_url(&self, url: &str) -> Result<(String, String, String, String)> {
        let slug = url
            .split("/event/")
            .nth(1)
            .ok_or_else(|| PloyError::Internal("Invalid event URL format".into()))?
            .split('?')
            .next()
            .unwrap_or("")
            .to_string();

        // Check if it's the long format (contains a slash separating season from matchup)
        if slug.contains('/') {
            // Long format: nba-regular-season-2024-2025/philadelphia-76ers-vs-dallas-mavericks-jan-2-2025
            let url_parts: Vec<&str> = slug.split('/').collect();

            // Extract league from first part (e.g., "nba-regular-season-2024-2025")
            let league = url_parts[0]
                .split('-')
                .next()
                .unwrap_or("NBA")
                .to_uppercase();

            // Parse teams from second part (e.g., "philadelphia-76ers-vs-dallas-mavericks-jan-2-2025")
            if url_parts.len() > 1 {
                let matchup = url_parts[1];

                // Find "-vs-" separator
                if let Some(vs_pos) = matchup.find("-vs-") {
                    let team1_slug = &matchup[..vs_pos];
                    let team2_part = &matchup[vs_pos + 4..]; // Skip "-vs-"

                    // Extract team2 name (everything before the date pattern like "jan-2-2025")
                    let team2_slug = self.extract_team_name(team2_part);

                    let team1 = self.slug_to_team_name(team1_slug, &league);
                    let team2 = self.slug_to_team_name(&team2_slug, &league);

                    return Ok((slug, league, team1, team2));
                }
            }

            return Err(PloyError::Internal(
                "Cannot parse teams from long URL format".into(),
            ));
        }

        // Short format: nba-phi-dal-2026-01-01
        let parts: Vec<&str> = slug.split('-').collect();
        if parts.len() < 3 {
            return Err(PloyError::Internal("Cannot parse teams from URL".into()));
        }

        let league = parts[0].to_uppercase(); // NBA, NFL, etc.
        let team1 = self.expand_team_code(parts[1], &league);
        let team2 = self.expand_team_code(parts[2], &league);

        Ok((slug, league, team1, team2))
    }

    /// Extract team name from slug, removing trailing date components
    fn extract_team_name(&self, slug: &str) -> String {
        // Month abbreviations that indicate start of date
        let months = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ];

        let parts: Vec<&str> = slug.split('-').collect();
        let mut team_parts = Vec::new();

        for part in parts {
            // Stop when we hit a month abbreviation or a number (date)
            if months.contains(&part.to_lowercase().as_str()) || part.parse::<u32>().is_ok() {
                break;
            }
            team_parts.push(part);
        }

        team_parts.join("-")
    }

    /// Convert URL slug to proper team name
    fn slug_to_team_name(&self, slug: &str, league: &str) -> String {
        // First try to extract team code and expand it
        let parts: Vec<&str> = slug.split('-').collect();

        // Check if last part is a team code (e.g., "philadelphia-76ers" -> check "76ers")
        if let Some(last) = parts.last() {
            // Try common patterns
            let code = if last.chars().all(|c| c.is_numeric()) {
                // Numeric suffix like "76ers" - combine with city
                slug.to_string()
            } else {
                last.to_string()
            };

            // Try to expand as team code first
            let expanded = self.expand_team_code(&code, league);
            if expanded != code.to_uppercase() {
                return expanded;
            }
        }

        // Convert slug to title case (e.g., "dallas-mavericks" -> "Dallas Mavericks")
        slug.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            })
            .collect::<Vec<String>>()
            .join(" ")
    }

    /// Expand team code to full name based on league context
    fn expand_team_code(&self, code: &str, league: &str) -> String {
        let code_upper = code.to_uppercase();

        match league {
            "NBA" => match code_upper.as_str() {
                "PHI" => "Philadelphia 76ers".to_string(),
                "DAL" => "Dallas Mavericks".to_string(),
                "LAL" => "Los Angeles Lakers".to_string(),
                "BOS" => "Boston Celtics".to_string(),
                "MIA" => "Miami Heat".to_string(),
                "GSW" => "Golden State Warriors".to_string(),
                "DEN" => "Denver Nuggets".to_string(),
                "MIL" => "Milwaukee Bucks".to_string(),
                "PHX" => "Phoenix Suns".to_string(),
                "MEM" => "Memphis Grizzlies".to_string(),
                "CLE" => "Cleveland Cavaliers".to_string(),
                "NYK" => "New York Knicks".to_string(),
                "SAC" => "Sacramento Kings".to_string(),
                "LAC" => "Los Angeles Clippers".to_string(),
                "MIN" => "Minnesota Timberwolves".to_string(),
                "NOP" => "New Orleans Pelicans".to_string(),
                "ATL" => "Atlanta Hawks".to_string(),
                "CHI" => "Chicago Bulls".to_string(),
                "TOR" => "Toronto Raptors".to_string(),
                "BKN" => "Brooklyn Nets".to_string(),
                "OKC" => "Oklahoma City Thunder".to_string(),
                "IND" => "Indiana Pacers".to_string(),
                "HOU" => "Houston Rockets".to_string(),
                "ORL" => "Orlando Magic".to_string(),
                "POR" => "Portland Trail Blazers".to_string(),
                "UTA" => "Utah Jazz".to_string(),
                "SAS" => "San Antonio Spurs".to_string(),
                "WAS" => "Washington Wizards".to_string(),
                "CHA" => "Charlotte Hornets".to_string(),
                "DET" => "Detroit Pistons".to_string(),
                _ => code_upper,
            },
            "NFL" => match code_upper.as_str() {
                "KC" | "KCC" => "Kansas City Chiefs".to_string(),
                "SF" | "SFO" => "San Francisco 49ers".to_string(),
                "BUF" => "Buffalo Bills".to_string(),
                "BAL" => "Baltimore Ravens".to_string(),
                "GB" | "GNB" => "Green Bay Packers".to_string(),
                "DET" => "Detroit Lions".to_string(),
                "TB" | "TBB" => "Tampa Bay Buccaneers".to_string(),
                "PHI" => "Philadelphia Eagles".to_string(),
                "DAL" => "Dallas Cowboys".to_string(),
                "MIA" => "Miami Dolphins".to_string(),
                "NYJ" => "New York Jets".to_string(),
                "NYG" => "New York Giants".to_string(),
                "NE" | "NEP" => "New England Patriots".to_string(),
                "LAR" => "Los Angeles Rams".to_string(),
                "LAC" => "Los Angeles Chargers".to_string(),
                "DEN" => "Denver Broncos".to_string(),
                "LV" | "LVR" => "Las Vegas Raiders".to_string(),
                "MIN" => "Minnesota Vikings".to_string(),
                "CHI" => "Chicago Bears".to_string(),
                "SEA" => "Seattle Seahawks".to_string(),
                "ARI" => "Arizona Cardinals".to_string(),
                "ATL" => "Atlanta Falcons".to_string(),
                "CAR" => "Carolina Panthers".to_string(),
                "NO" | "NOS" => "New Orleans Saints".to_string(),
                "CIN" => "Cincinnati Bengals".to_string(),
                "CLE" => "Cleveland Browns".to_string(),
                "PIT" => "Pittsburgh Steelers".to_string(),
                "IND" => "Indianapolis Colts".to_string(),
                "JAX" => "Jacksonville Jaguars".to_string(),
                "TEN" => "Tennessee Titans".to_string(),
                "HOU" => "Houston Texans".to_string(),
                "WAS" => "Washington Commanders".to_string(),
                _ => code_upper,
            },
            _ => code_upper, // Unknown league
        }
    }

    /// Fetch market odds from Polymarket
    /// Tries multiple strategies: slug query, team name search, matchup search
    async fn fetch_market_odds(
        &self,
        event_slug: &str,
        team1: &str,
        team2: &str,
    ) -> Result<MarketOdds> {
        let client = PolymarketClient::new(CLOB_BASE_URL, true)?;

        // Strategy 1: Try slug-based query (correct format with ?slug=)
        // For long URLs, extract just the matchup part
        let search_slug = if event_slug.contains('/') {
            event_slug.split('/').last().unwrap_or(event_slug)
        } else {
            event_slug
        };

        debug!("Searching Polymarket for slug: {}", search_slug);
        if let Some(odds) = self.try_fetch_odds(&client, search_slug).await {
            info!("Found market data via slug query");
            return Ok(odds);
        }

        // Strategy 2: Search by team names in title
        let team1_short = self.get_team_short_name(team1);
        let team2_short = self.get_team_short_name(team2);
        debug!("Searching for teams: {} vs {}", team1_short, team2_short);

        // Try searching with team1
        let team_events = client
            .get_active_sports_events(&team1_short)
            .await
            .unwrap_or_default();
        if let Some(odds) = self
            .try_search_team_matchup(&client, &team_events, team1, team2)
            .await
        {
            info!("Found market data via team search");
            return Ok(odds);
        }

        // Strategy 3: Search markets endpoint directly
        let markets = client
            .search_markets(&team1_short)
            .await
            .unwrap_or_default();
        if let Some(odds) = self.try_search_markets(&markets, team1, team2) {
            info!("Found market data via markets search");
            return Ok(odds);
        }

        // No market found - this might be expected for sports events
        // Polymarket may not have this specific matchup
        warn!(
            "Could not fetch market data for {} vs {} - market may not exist on Polymarket",
            team1, team2
        );
        warn!("Analysis will proceed with Grok data only (no Polymarket odds comparison)");

        // Return default if API fails
        Ok(MarketOdds {
            team1_yes_price: Decimal::new(50, 2),
            team1_no_price: Decimal::new(50, 2),
            team2_yes_price: Some(Decimal::new(50, 2)),
            team2_no_price: Some(Decimal::new(50, 2)),
            spread: None,
        })
    }

    /// Try to fetch odds from slug-based lookup.
    async fn try_fetch_odds(&self, client: &PolymarketClient, slug: &str) -> Option<MarketOdds> {
        let events = client.get_active_sports_events(slug).await.ok()?;
        let normalized = slug.trim_matches('/');
        let candidate = events
            .iter()
            .find(|event| {
                event.slug.as_deref().is_some_and(|event_slug| {
                    let event_slug = event_slug.trim_matches('/');
                    event_slug == normalized || event_slug.ends_with(&format!("/{}", normalized))
                })
            })
            .cloned()
            .or_else(|| events.into_iter().next())?;

        let event = self.ensure_event_has_markets(client, &candidate).await?;
        self.parse_event_odds(&event)
    }

    /// Search for a matchup in team search results
    async fn try_search_team_matchup(
        &self,
        client: &PolymarketClient,
        events: &[GammaEventInfo],
        team1: &str,
        team2: &str,
    ) -> Option<MarketOdds> {
        // Look for an event that mentions both teams
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        for event in events {
            if let Some(title) = event.title.as_deref() {
                let title_lower = title.to_lowercase();
                if title_lower.contains(&team1_lower) || title_lower.contains(&team2_lower) {
                    let Some(hydrated) = self.ensure_event_has_markets(client, event).await else {
                        continue;
                    };
                    if let Some(odds) = self.parse_event_odds(&hydrated) {
                        return Some(odds);
                    }
                }
            }
        }

        None
    }

    /// Search markets endpoint for matchup
    fn try_search_markets(
        &self,
        markets: &[GammaMarketSummary],
        team1: &str,
        team2: &str,
    ) -> Option<MarketOdds> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        for market in markets {
            if let Some(question) = market.question.as_deref() {
                let question_lower = question.to_lowercase();
                if (question_lower.contains(&team1_lower) || question_lower.contains(&team2_lower))
                    && (question_lower.contains("win") || question_lower.contains("beat"))
                {
                    return self.parse_market_summary_odds(market);
                }
            }
        }

        None
    }

    /// Parse odds from an event object
    fn parse_event_odds(&self, event: &GammaEventInfo) -> Option<MarketOdds> {
        let market = event.markets.first()?;
        self.parse_market_odds(market)
    }

    /// Parse odds from a market object
    fn parse_market_odds(&self, market: &GammaMarketInfo) -> Option<MarketOdds> {
        let yes_price = self
            .parse_yes_price(market.outcome_prices.as_deref())
            .unwrap_or(0.5);
        self.build_odds_from_yes_price(yes_price)
    }

    fn parse_market_summary_odds(&self, market: &GammaMarketSummary) -> Option<MarketOdds> {
        let yes_price = self
            .parse_yes_price(market.outcome_prices.as_deref())
            .unwrap_or(0.5);
        self.build_odds_from_yes_price(yes_price)
    }

    fn parse_yes_price(&self, outcome_prices: Option<&str>) -> Option<f64> {
        let raw = outcome_prices?;
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(raw) {
            return arr.first().and_then(|v| v.parse::<f64>().ok());
        }
        if let Ok(arr) = serde_json::from_str::<Vec<f64>>(raw) {
            return arr.first().copied();
        }
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
            return arr.first().and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
            });
        }
        None
    }

    fn build_odds_from_yes_price(&self, yes_price: f64) -> Option<MarketOdds> {
        Some(MarketOdds {
            team1_yes_price: Decimal::from_f64_retain(yes_price).unwrap_or(Decimal::new(50, 2)),
            team1_no_price: Decimal::from_f64_retain(1.0 - yes_price)
                .unwrap_or(Decimal::new(50, 2)),
            team2_yes_price: Some(
                Decimal::from_f64_retain(1.0 - yes_price).unwrap_or(Decimal::new(50, 2)),
            ),
            team2_no_price: Some(
                Decimal::from_f64_retain(yes_price).unwrap_or(Decimal::new(50, 2)),
            ),
            spread: None,
        })
    }

    async fn ensure_event_has_markets(
        &self,
        client: &PolymarketClient,
        event: &GammaEventInfo,
    ) -> Option<GammaEventInfo> {
        if !event.markets.is_empty() {
            return Some(event.clone());
        }
        client.get_event_details(&event.id).await.ok()
    }

    /// Get short team name for search
    fn get_team_short_name(&self, full_name: &str) -> String {
        // Extract just the team name without city (e.g., "76ers" from "Philadelphia 76ers")
        let parts: Vec<&str> = full_name.split_whitespace().collect();
        if parts.len() > 1 {
            parts.last().unwrap_or(&full_name).to_string()
        } else {
            full_name.to_string()
        }
    }

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
        use crate::agent::odds_provider::{OddsProvider, Sport};

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
    pub draftkings: Option<crate::agent::odds_provider::EdgeAnalysis>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_analyst() -> SportsAnalyst {
        let grok = GrokClient::new(crate::agent::grok::GrokConfig::default()).unwrap();
        let claude = ClaudeAgentClient::new();
        SportsAnalyst::new(grok, claude)
    }

    #[test]
    fn test_expand_team_code() {
        let analyst = create_test_analyst();

        // NBA teams
        assert_eq!(analyst.expand_team_code("phi", "NBA"), "Philadelphia 76ers");
        assert_eq!(analyst.expand_team_code("DAL", "NBA"), "Dallas Mavericks");
        assert_eq!(analyst.expand_team_code("LAL", "NBA"), "Los Angeles Lakers");
        assert_eq!(analyst.expand_team_code("DET", "NBA"), "Detroit Pistons");

        // NFL teams - same codes, different names
        assert_eq!(
            analyst.expand_team_code("phi", "NFL"),
            "Philadelphia Eagles"
        );
        assert_eq!(analyst.expand_team_code("DAL", "NFL"), "Dallas Cowboys");
        assert_eq!(analyst.expand_team_code("DET", "NFL"), "Detroit Lions");
    }

    #[test]
    fn test_parse_event_url() {
        let analyst = create_test_analyst();

        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nba-phi-dal-2026-01-01")
            .unwrap();

        assert_eq!(slug, "nba-phi-dal-2026-01-01");
        assert_eq!(league, "NBA");
        assert_eq!(team1, "Philadelphia 76ers");
        assert_eq!(team2, "Dallas Mavericks");
    }

    #[test]
    fn test_parse_nfl_event() {
        let analyst = create_test_analyst();

        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nfl-kc-sf-2026-02-09")
            .unwrap();

        assert_eq!(slug, "nfl-kc-sf-2026-02-09");
        assert_eq!(league, "NFL");
        assert_eq!(team1, "Kansas City Chiefs");
        assert_eq!(team2, "San Francisco 49ers");
    }

    #[test]
    fn test_parse_long_format_url() {
        let analyst = create_test_analyst();

        // Real Polymarket URL format
        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nba-regular-season-2024-2025/philadelphia-76ers-vs-dallas-mavericks-jan-2-2025")
            .unwrap();

        assert_eq!(league, "NBA");
        assert_eq!(team1, "Philadelphia 76ers");
        assert_eq!(team2, "Dallas Mavericks");
        assert!(slug.contains("philadelphia-76ers-vs-dallas-mavericks"));
    }

    #[test]
    fn test_extract_team_name() {
        let analyst = create_test_analyst();

        // Should strip date suffix
        assert_eq!(
            analyst.extract_team_name("dallas-mavericks-jan-2-2025"),
            "dallas-mavericks"
        );
        assert_eq!(
            analyst.extract_team_name("golden-state-warriors-dec-25-2024"),
            "golden-state-warriors"
        );
    }

    #[test]
    fn test_slug_to_team_name() {
        let analyst = create_test_analyst();

        // Should convert slug to title case
        assert_eq!(
            analyst.slug_to_team_name("dallas-mavericks", "NBA"),
            "Dallas Mavericks"
        );
        assert_eq!(
            analyst.slug_to_team_name("philadelphia-76ers", "NBA"),
            "Philadelphia 76ers"
        );
    }
}
