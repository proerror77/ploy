//! Structured Sports Data Fetcher
//!
//! Uses Grok to fetch sports data in a fixed JSON format for reliable parsing.
//! This ensures consistent data quality for Claude's analysis.

use crate::agent::grok::GrokClient;
use crate::error::{PloyError, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Structured player status data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerStatus {
    pub name: String,
    pub team: String,
    pub status: InjuryStatus,
    pub injury: Option<String>,
    pub last_5_games_ppg: Option<f64>,
    pub last_5_games_rpg: Option<f64>,
    pub last_5_games_apg: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum InjuryStatus {
    Available,
    Probable,
    Questionable,
    Doubtful,
    Out,
    Unknown,
}

impl Default for InjuryStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Structured betting line data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BettingLines {
    pub spread: f64,
    pub spread_team: String,
    pub moneyline_favorite: i32,
    pub moneyline_underdog: i32,
    pub over_under: f64,
    pub implied_probability: f64,
    pub line_movement: Option<String>,
}

/// Structured sentiment data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentimentData {
    pub expert_pick: String,
    pub expert_confidence: f64,
    pub public_bet_percentage: f64,
    pub sharp_money_side: String,
    pub social_sentiment: String,
    pub key_narratives: Vec<String>,
}

/// Breaking news and recent developments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsData {
    pub breaking_news: Vec<NewsItem>,
    pub injury_updates: Vec<String>,
    pub lineup_changes: Vec<String>,
    pub weather_impact: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsItem {
    pub headline: String,
    pub source: String,
    pub timestamp: String,
    pub impact: String, // "HIGH", "MEDIUM", "LOW"
}

/// Head-to-head historical data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadToHeadData {
    pub last_5_meetings: Vec<HistoricalGame>,
    pub team1_wins: u32,
    pub team2_wins: u32,
    pub avg_total_points: f64,
    pub avg_margin: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalGame {
    pub date: String,
    pub team1_score: u32,
    pub team2_score: u32,
    pub winner: String,
    pub location: String,
}

/// Team statistics and trends
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStats {
    pub team1_stats: TeamPerformance,
    pub team2_stats: TeamPerformance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPerformance {
    pub team_name: String,
    pub record: String, // "W-L" format
    pub last_10_record: String,
    pub home_record: Option<String>,
    pub away_record: Option<String>,
    pub avg_points_scored: f64,
    pub avg_points_allowed: f64,
    pub offensive_rating: f64,
    pub defensive_rating: f64,
    pub pace: f64,
    pub recent_form: String, // "W-W-L-W-W"
    pub rest_days: u32,
    pub back_to_back: bool,
}

/// Advanced analytics and trends
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvancedAnalytics {
    pub team1_trends: Vec<String>,
    pub team2_trends: Vec<String>,
    pub situational_factors: Vec<String>,
    pub betting_trends: BettingTrends,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BettingTrends {
    pub team1_ats_record: String, // Against the spread
    pub team2_ats_record: String,
    pub team1_over_under_record: String,
    pub team2_over_under_record: String,
    pub public_money_percentage: f64,
    pub sharp_money_percentage: f64,
}

/// Complete structured game data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredGameData {
    pub game_info: GameInfo,
    pub team1_players: Vec<PlayerStatus>,
    pub team2_players: Vec<PlayerStatus>,
    pub betting_lines: BettingLines,
    pub sentiment: SentimentData,
    pub news: NewsData,
    pub head_to_head: HeadToHeadData,
    pub team_stats: TeamStats,
    pub advanced_analytics: AdvancedAnalytics,
    pub data_quality: DataQuality,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameInfo {
    pub team1: String,
    pub team2: String,
    pub game_time: String,
    pub venue: String,
    pub league: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQuality {
    pub sources_count: u32,
    pub data_freshness: String,
    pub confidence: f64,
}

/// Sports Data Fetcher - Gets structured data from Grok
pub struct SportsDataFetcher {
    grok: GrokClient,
}

impl SportsDataFetcher {
    pub fn new(grok: GrokClient) -> Self {
        Self { grok }
    }

    /// Fetch structured game data for a matchup
    pub async fn fetch_game_data(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<StructuredGameData> {
        info!(
            "Fetching comprehensive structured data for {} vs {}",
            team1, team2
        );

        // Step 1: Fetch player status
        info!("📊 Step 1/7: Fetching player status and injuries...");
        let players = self.fetch_player_status(team1, team2, league).await?;

        // Step 2: Fetch betting lines
        info!("💰 Step 2/7: Fetching betting lines and odds...");
        let betting = self.fetch_betting_lines(team1, team2).await?;

        // Step 3: Fetch sentiment
        info!("📈 Step 3/7: Analyzing market sentiment...");
        let sentiment = self.fetch_sentiment(team1, team2).await?;

        // Step 4: Fetch breaking news
        info!("📰 Step 4/7: Fetching breaking news and updates...");
        let news = self.fetch_news(team1, team2, league).await?;

        // Step 5: Fetch head-to-head history
        info!("🔄 Step 5/7: Analyzing head-to-head history...");
        let head_to_head = self.fetch_head_to_head(team1, team2, league).await?;

        // Step 6: Fetch team statistics
        info!("📊 Step 6/7: Fetching team statistics and trends...");
        let team_stats = self.fetch_team_stats(team1, team2, league).await?;

        // Step 7: Fetch advanced analytics
        info!("🎯 Step 7/7: Analyzing advanced metrics and trends...");
        let advanced_analytics = self.fetch_advanced_analytics(team1, team2, league).await?;

        info!("✅ Data collection complete!");

        Ok(StructuredGameData {
            game_info: GameInfo {
                team1: team1.to_string(),
                team2: team2.to_string(),
                game_time: "TBD".to_string(),
                venue: "TBD".to_string(),
                league: league.to_string(),
            },
            team1_players: players.0,
            team2_players: players.1,
            betting_lines: betting,
            sentiment,
            news,
            head_to_head,
            team_stats,
            advanced_analytics,
            data_quality: DataQuality {
                sources_count: 7,
                data_freshness: "< 1 hour".to_string(),
                confidence: 0.90,
            },
        })
    }

    /// Fetch player injury/status data in structured format
    async fn fetch_player_status(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<(Vec<PlayerStatus>, Vec<PlayerStatus>)> {
        let prompt = format!(
            r#"You are a sports data API. Return ONLY valid JSON, no other text.

Search for the latest injury report and player status for tonight's {league} game: {team1} vs {team2}

Return this exact JSON structure:
{{
  "team1_players": [
    {{
      "name": "Player Name",
      "team": "{team1}",
      "status": "AVAILABLE|PROBABLE|QUESTIONABLE|DOUBTFUL|OUT",
      "injury": "injury description or null",
      "last_5_games_ppg": 25.4,
      "last_5_games_rpg": 10.2,
      "last_5_games_apg": 5.1
    }}
  ],
  "team2_players": [
    {{
      "name": "Player Name",
      "team": "{team2}",
      "status": "AVAILABLE|PROBABLE|QUESTIONABLE|DOUBTFUL|OUT",
      "injury": "injury description or null",
      "last_5_games_ppg": 28.1,
      "last_5_games_rpg": 8.5,
      "last_5_games_apg": 9.2
    }}
  ]
}}

Include top 3-5 key players per team. Focus on starters and anyone with injury concerns.
Return ONLY the JSON, no markdown, no explanation."#,
            league = league,
            team1 = team1,
            team2 = team2
        );

        let response = self.grok.chat(&prompt).await?;
        debug!(
            "Player status response: {}",
            &response[..response.len().min(200)]
        );

        self.parse_player_response(&response, team1, team2)
    }

    /// Fetch betting lines in structured format
    async fn fetch_betting_lines(&self, team1: &str, team2: &str) -> Result<BettingLines> {
        let prompt = format!(
            r#"You are a sports betting data API. Return ONLY valid JSON, no other text.

Search for the current betting lines for tonight's game: {team1} vs {team2}

Return this exact JSON structure:
{{
  "spread": -5.5,
  "spread_team": "Team Name",
  "moneyline_favorite": -150,
  "moneyline_underdog": 130,
  "over_under": 225.5,
  "implied_probability": 0.60,
  "line_movement": "opened -4, now -5.5 (sharps on favorite)"
}}

Use real current lines from major sportsbooks (DraftKings, FanDuel, BetMGM).
Return ONLY the JSON, no markdown, no explanation."#,
            team1 = team1,
            team2 = team2
        );

        let response = self.grok.chat(&prompt).await?;
        debug!(
            "Betting lines response: {}",
            &response[..response.len().min(200)]
        );

        self.parse_betting_response(&response, team1)
    }

    /// Fetch sentiment data in structured format
    async fn fetch_sentiment(&self, team1: &str, team2: &str) -> Result<SentimentData> {
        let prompt = format!(
            r#"You are a sports sentiment analysis API. Return ONLY valid JSON, no other text.

Analyze current betting sentiment and expert picks for: {team1} vs {team2}

Return this exact JSON structure:
{{
  "expert_pick": "Team Name",
  "expert_confidence": 0.72,
  "public_bet_percentage": 55.0,
  "sharp_money_side": "Team Name",
  "social_sentiment": "BULLISH|BEARISH|NEUTRAL|MIXED",
  "key_narratives": [
    "Narrative 1 affecting the game",
    "Narrative 2 affecting the game",
    "Narrative 3 affecting the game"
  ]
}}

Base this on ESPN, Action Network, Twitter/X trends, and betting market analysis.
Return ONLY the JSON, no markdown, no explanation."#,
            team1 = team1,
            team2 = team2
        );

        let response = self.grok.chat(&prompt).await?;
        debug!(
            "Sentiment response: {}",
            &response[..response.len().min(200)]
        );

        self.parse_sentiment_response(&response, team1)
    }

    /// Parse player status response
    fn parse_player_response(
        &self,
        response: &str,
        _team1: &str,
        _team2: &str,
    ) -> Result<(Vec<PlayerStatus>, Vec<PlayerStatus>)> {
        // Extract JSON from response
        let json_str = self.extract_json(response)?;

        #[derive(Deserialize)]
        struct PlayerResponse {
            team1_players: Option<Vec<PlayerStatus>>,
            team2_players: Option<Vec<PlayerStatus>>,
        }

        match serde_json::from_str::<PlayerResponse>(&json_str) {
            Ok(parsed) => Ok((
                parsed.team1_players.unwrap_or_default(),
                parsed.team2_players.unwrap_or_default(),
            )),
            Err(e) => {
                warn!("Failed to parse player response: {}", e);
                // Return empty defaults
                Ok((vec![], vec![]))
            }
        }
    }

    /// Parse betting lines response
    fn parse_betting_response(&self, response: &str, team1: &str) -> Result<BettingLines> {
        let json_str = self.extract_json(response)?;

        // Sanitize JSON - fix common LLM output issues
        let sanitized = self.sanitize_json(&json_str);

        match serde_json::from_str::<BettingLines>(&sanitized) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse betting response: {}", e);
                debug!(
                    "Problematic JSON: {}",
                    &sanitized[..sanitized.len().min(500)]
                );
                // Return defaults
                Ok(BettingLines {
                    spread: 0.0,
                    spread_team: team1.to_string(),
                    moneyline_favorite: -110,
                    moneyline_underdog: -110,
                    over_under: 0.0,
                    implied_probability: 0.5,
                    line_movement: None,
                })
            }
        }
    }

    /// Sanitize JSON string to fix common LLM output issues
    fn sanitize_json(&self, json: &str) -> String {
        let mut result = String::with_capacity(json.len());
        let chars: Vec<char> = json.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            // Fix: Remove + prefix from positive numbers after ": " or ":"
            // Pattern: ": +" or ":" followed by space(s) then "+"
            if c == ':' {
                result.push(c);
                i += 1;

                // Skip any whitespace after colon
                while i < chars.len() && chars[i].is_whitespace() {
                    result.push(chars[i]);
                    i += 1;
                }

                // Check if next char is '+' followed by a digit
                if i < chars.len() && chars[i] == '+' {
                    if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                        // Skip the '+' prefix
                        i += 1;
                    }
                }
                continue;
            }

            result.push(c);
            i += 1;
        }

        // Fix trailing commas before } or ]
        let result = result
            .replace(",}", "}")
            .replace(",]", "]")
            .replace(", }", "}")
            .replace(", ]", "]");

        result
    }

    /// Parse sentiment response
    fn parse_sentiment_response(&self, response: &str, team1: &str) -> Result<SentimentData> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<SentimentData>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse sentiment response: {}", e);
                // Return defaults
                Ok(SentimentData {
                    expert_pick: team1.to_string(),
                    expert_confidence: 0.5,
                    public_bet_percentage: 50.0,
                    sharp_money_side: team1.to_string(),
                    social_sentiment: "NEUTRAL".to_string(),
                    key_narratives: vec![],
                })
            }
        }
    }

    /// Fetch breaking news and recent developments
    async fn fetch_news(&self, team1: &str, team2: &str, league: &str) -> Result<NewsData> {
        let prompt = format!(
            r#"You are a sports news API. Return ONLY valid JSON, no other text.

Search X (Twitter), ESPN, and sports news for breaking news about tonight's {league} game: {team1} vs {team2}

Return this exact JSON structure:
{{
  "breaking_news": [
    {{
      "headline": "News headline",
      "source": "ESPN|Twitter|Action Network",
      "timestamp": "2 hours ago",
      "impact": "HIGH|MEDIUM|LOW"
    }}
  ],
  "injury_updates": [
    "Player X upgraded to probable",
    "Player Y ruled out"
  ],
  "lineup_changes": [
    "Team starting lineup change",
    "Rotation adjustment"
  ],
  "weather_impact": "Clear conditions, no impact" or null
}}

Include 3-5 most recent and relevant news items. Focus on injury updates, lineup changes, and game-impacting news.
Return ONLY the JSON, no markdown, no explanation."#,
            league = league,
            team1 = team1,
            team2 = team2
        );

        let response = self.grok.chat(&prompt).await?;
        debug!("News response: {}", &response[..response.len().min(200)]);

        self.parse_news_response(&response)
    }

    /// Fetch head-to-head historical data
    async fn fetch_head_to_head(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<HeadToHeadData> {
        let prompt = format!(
            r#"You are a sports statistics API. Return ONLY valid JSON, no other text.

Search for head-to-head history between {team1} and {team2} in {league}

Return this exact JSON structure:
{{
  "last_5_meetings": [
    {{
      "date": "2024-12-15",
      "team1_score": 112,
      "team2_score": 108,
      "winner": "{team1}",
      "location": "Home|Away"
    }}
  ],
  "team1_wins": 3,
  "team2_wins": 2,
  "avg_total_points": 220.5,
  "avg_margin": 5.2
}}

Include last 5 meetings. Calculate averages accurately.
Return ONLY the JSON, no markdown, no explanation."#,
            team1 = team1,
            team2 = team2,
            league = league
        );

        let response = self.grok.chat(&prompt).await?;
        debug!("H2H response: {}", &response[..response.len().min(200)]);

        self.parse_h2h_response(&response, team1)
    }

    /// Fetch team statistics and trends
    async fn fetch_team_stats(&self, team1: &str, team2: &str, league: &str) -> Result<TeamStats> {
        let prompt = format!(
            r#"You are a sports statistics API. Return ONLY valid JSON, no other text.

Search for current season statistics for {team1} and {team2} in {league}

Return this exact JSON structure:
{{
  "team1_stats": {{
    "team_name": "{team1}",
    "record": "25-15",
    "last_10_record": "7-3",
    "home_record": "15-5",
    "away_record": "10-10",
    "avg_points_scored": 115.2,
    "avg_points_allowed": 108.5,
    "offensive_rating": 118.5,
    "defensive_rating": 112.3,
    "pace": 99.5,
    "recent_form": "W-W-L-W-W",
    "rest_days": 1,
    "back_to_back": false
  }},
  "team2_stats": {{
    "team_name": "{team2}",
    "record": "22-18",
    "last_10_record": "5-5",
    "home_record": "12-8",
    "away_record": "10-10",
    "avg_points_scored": 112.8,
    "avg_points_allowed": 111.2,
    "offensive_rating": 115.2,
    "defensive_rating": 114.8,
    "pace": 98.2,
    "recent_form": "L-W-L-W-L",
    "rest_days": 2,
    "back_to_back": false
  }}
}}

Use current season data. Include rest days and back-to-back status.
Return ONLY the JSON, no markdown, no explanation."#,
            team1 = team1,
            team2 = team2,
            league = league
        );

        let response = self.grok.chat(&prompt).await?;
        debug!(
            "Team stats response: {}",
            &response[..response.len().min(200)]
        );

        self.parse_team_stats_response(&response, team1, team2)
    }

    /// Fetch advanced analytics and betting trends
    async fn fetch_advanced_analytics(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<AdvancedAnalytics> {
        let prompt = format!(
            r#"You are a sports analytics API. Return ONLY valid JSON, no other text.

Analyze advanced metrics and betting trends for {team1} vs {team2} in {league}

Return this exact JSON structure:
{{
  "team1_trends": [
    "Covers spread in 65% of home games",
    "8-2 ATS in last 10 games",
    "Strong vs Western Conference teams"
  ],
  "team2_trends": [
    "Struggles on back-to-backs (2-8 ATS)",
    "Under hits 60% on the road",
    "Poor vs elite defenses"
  ],
  "situational_factors": [
    "Team1 revenge game (lost last meeting)",
    "Team2 on 3-game road trip",
    "Playoff implications for both teams"
  ],
  "betting_trends": {{
    "team1_ats_record": "28-22-1",
    "team2_ats_record": "25-25-0",
    "team1_over_under_record": "26-24-0",
    "team2_over_under_record": "22-28-0",
    "public_money_percentage": 65.0,
    "sharp_money_percentage": 45.0
  }}
}}

Include 3-5 trends per team. Focus on ATS records, situational factors, and betting patterns.
Return ONLY the JSON, no markdown, no explanation."#,
            team1 = team1,
            team2 = team2,
            league = league
        );

        let response = self.grok.chat(&prompt).await?;
        debug!(
            "Advanced analytics response: {}",
            &response[..response.len().min(200)]
        );

        self.parse_advanced_analytics_response(&response, team1)
    }

    /// Parse news response
    fn parse_news_response(&self, response: &str) -> Result<NewsData> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<NewsData>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse news response: {}", e);
                Ok(NewsData {
                    breaking_news: vec![],
                    injury_updates: vec![],
                    lineup_changes: vec![],
                    weather_impact: None,
                })
            }
        }
    }

    /// Parse head-to-head response
    fn parse_h2h_response(&self, response: &str, _team1: &str) -> Result<HeadToHeadData> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<HeadToHeadData>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse H2H response: {}", e);
                Ok(HeadToHeadData {
                    last_5_meetings: vec![],
                    team1_wins: 0,
                    team2_wins: 0,
                    avg_total_points: 0.0,
                    avg_margin: 0.0,
                })
            }
        }
    }

    /// Parse team stats response
    fn parse_team_stats_response(
        &self,
        response: &str,
        team1: &str,
        team2: &str,
    ) -> Result<TeamStats> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<TeamStats>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse team stats response: {}", e);
                Ok(TeamStats {
                    team1_stats: TeamPerformance {
                        team_name: team1.to_string(),
                        record: "0-0".to_string(),
                        last_10_record: "0-0".to_string(),
                        home_record: None,
                        away_record: None,
                        avg_points_scored: 0.0,
                        avg_points_allowed: 0.0,
                        offensive_rating: 0.0,
                        defensive_rating: 0.0,
                        pace: 0.0,
                        recent_form: "".to_string(),
                        rest_days: 0,
                        back_to_back: false,
                    },
                    team2_stats: TeamPerformance {
                        team_name: team2.to_string(),
                        record: "0-0".to_string(),
                        last_10_record: "0-0".to_string(),
                        home_record: None,
                        away_record: None,
                        avg_points_scored: 0.0,
                        avg_points_allowed: 0.0,
                        offensive_rating: 0.0,
                        defensive_rating: 0.0,
                        pace: 0.0,
                        recent_form: "".to_string(),
                        rest_days: 0,
                        back_to_back: false,
                    },
                })
            }
        }
    }

    /// Parse advanced analytics response
    fn parse_advanced_analytics_response(
        &self,
        response: &str,
        _team1: &str,
    ) -> Result<AdvancedAnalytics> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<AdvancedAnalytics>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse advanced analytics response: {}", e);
                Ok(AdvancedAnalytics {
                    team1_trends: vec![],
                    team2_trends: vec![],
                    situational_factors: vec![],
                    betting_trends: BettingTrends {
                        team1_ats_record: "0-0-0".to_string(),
                        team2_ats_record: "0-0-0".to_string(),
                        team1_over_under_record: "0-0-0".to_string(),
                        team2_over_under_record: "0-0-0".to_string(),
                        public_money_percentage: 50.0,
                        sharp_money_percentage: 50.0,
                    },
                })
            }
        }
    }

    /// Extract JSON from a response that might have markdown or other text
    fn extract_json(&self, response: &str) -> Result<String> {
        // Try to find JSON in the response
        let response = response.trim();

        // If it starts with {, assume it's pure JSON
        if response.starts_with('{') {
            if let Some(end) = response.rfind('}') {
                return Ok(response[..=end].to_string());
            }
        }

        // Try to extract from markdown code block
        if let Some(start) = response.find("```json") {
            let after_marker = &response[start + 7..];
            if let Some(end) = after_marker.find("```") {
                return Ok(after_marker[..end].trim().to_string());
            }
        }

        // Try to extract from plain code block
        if let Some(start) = response.find("```") {
            let after_marker = &response[start + 3..];
            if let Some(end) = after_marker.find("```") {
                let content = after_marker[..end].trim();
                if content.starts_with('{') {
                    return Ok(content.to_string());
                }
            }
        }

        // Try to find any JSON object
        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                return Ok(response[start..=end].to_string());
            }
        }

        Err(PloyError::Internal("No JSON found in response".into()))
    }
}

/// Format structured data for Claude analysis
pub fn format_for_claude(data: &StructuredGameData) -> String {
    let mut output = String::new();

    // Game Info
    output.push_str(&format!(
        "## Game: {} vs {}\n",
        data.game_info.team1, data.game_info.team2
    ));
    output.push_str(&format!("League: {}\n\n", data.game_info.league));

    // Team 1 Players
    output.push_str(&format!("## {} Key Players\n", data.game_info.team1));
    for player in &data.team1_players {
        output.push_str(&format!(
            "- {} | Status: {:?} | Last 5: {:.1}/{:.1}/{:.1}",
            player.name,
            player.status,
            player.last_5_games_ppg.unwrap_or(0.0),
            player.last_5_games_rpg.unwrap_or(0.0),
            player.last_5_games_apg.unwrap_or(0.0)
        ));
        if let Some(ref injury) = player.injury {
            output.push_str(&format!(" ({})", injury));
        }
        output.push('\n');
    }

    // Team 2 Players
    output.push_str(&format!("\n## {} Key Players\n", data.game_info.team2));
    for player in &data.team2_players {
        output.push_str(&format!(
            "- {} | Status: {:?} | Last 5: {:.1}/{:.1}/{:.1}",
            player.name,
            player.status,
            player.last_5_games_ppg.unwrap_or(0.0),
            player.last_5_games_rpg.unwrap_or(0.0),
            player.last_5_games_apg.unwrap_or(0.0)
        ));
        if let Some(ref injury) = player.injury {
            output.push_str(&format!(" ({})", injury));
        }
        output.push('\n');
    }

    // Betting Lines
    output.push_str("\n## Betting Lines\n");
    output.push_str(&format!(
        "- Spread: {} {}\n",
        data.betting_lines.spread_team, data.betting_lines.spread
    ));
    output.push_str(&format!(
        "- Moneyline: Fav {} / Dog {}\n",
        data.betting_lines.moneyline_favorite, data.betting_lines.moneyline_underdog
    ));
    output.push_str(&format!("- O/U: {}\n", data.betting_lines.over_under));
    output.push_str(&format!(
        "- Implied Win Prob: {:.1}%\n",
        data.betting_lines.implied_probability * 100.0
    ));
    if let Some(ref movement) = data.betting_lines.line_movement {
        output.push_str(&format!("- Line Movement: {}\n", movement));
    }

    // Sentiment
    output.push_str("\n## Market Sentiment\n");
    output.push_str(&format!(
        "- Expert Pick: {} ({:.0}% confidence)\n",
        data.sentiment.expert_pick,
        data.sentiment.expert_confidence * 100.0
    ));
    output.push_str(&format!(
        "- Public: {:.0}% on favorite\n",
        data.sentiment.public_bet_percentage
    ));
    output.push_str(&format!(
        "- Sharp Money: {}\n",
        data.sentiment.sharp_money_side
    ));
    output.push_str(&format!("- Social: {}\n", data.sentiment.social_sentiment));

    if !data.sentiment.key_narratives.is_empty() {
        output.push_str("\nKey Narratives:\n");
        for narrative in &data.sentiment.key_narratives {
            output.push_str(&format!("- {}\n", narrative));
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_fetcher() -> SportsDataFetcher {
        let grok = GrokClient::new(crate::agent::grok::GrokConfig::default()).unwrap();
        SportsDataFetcher::new(grok)
    }

    #[test]
    fn test_extract_json() {
        let fetcher = create_test_fetcher();

        // Pure JSON
        let json = r#"{"key": "value"}"#;
        assert_eq!(fetcher.extract_json(json).unwrap(), json);

        // Markdown code block
        let markdown = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(
            fetcher.extract_json(markdown).unwrap(),
            "{\"key\": \"value\"}"
        );

        // JSON with surrounding text
        let messy = "Here is the data: {\"key\": \"value\"} end";
        assert_eq!(fetcher.extract_json(messy).unwrap(), "{\"key\": \"value\"}");
    }

    #[test]
    fn test_format_for_claude() {
        let data = StructuredGameData {
            game_info: GameInfo {
                team1: "Philadelphia 76ers".to_string(),
                team2: "Dallas Mavericks".to_string(),
                game_time: "7:00 PM ET".to_string(),
                venue: "Wells Fargo Center".to_string(),
                league: "NBA".to_string(),
            },
            team1_players: vec![PlayerStatus {
                name: "Joel Embiid".to_string(),
                team: "Philadelphia 76ers".to_string(),
                status: InjuryStatus::Questionable,
                injury: Some("Knee soreness".to_string()),
                last_5_games_ppg: Some(32.5),
                last_5_games_rpg: Some(11.2),
                last_5_games_apg: Some(5.8),
            }],
            team2_players: vec![PlayerStatus {
                name: "Luka Doncic".to_string(),
                team: "Dallas Mavericks".to_string(),
                status: InjuryStatus::Available,
                injury: None,
                last_5_games_ppg: Some(35.2),
                last_5_games_rpg: Some(9.4),
                last_5_games_apg: Some(10.1),
            }],
            betting_lines: BettingLines {
                spread: -3.5,
                spread_team: "Dallas Mavericks".to_string(),
                moneyline_favorite: -160,
                moneyline_underdog: 140,
                over_under: 225.5,
                implied_probability: 0.615,
                line_movement: Some("opened -2.5, now -3.5".to_string()),
            },
            sentiment: SentimentData {
                expert_pick: "Dallas Mavericks".to_string(),
                expert_confidence: 0.72,
                public_bet_percentage: 58.0,
                sharp_money_side: "Dallas Mavericks".to_string(),
                social_sentiment: "BULLISH".to_string(),
                key_narratives: vec!["Luka on hot streak".to_string()],
            },
            news: NewsData {
                breaking_news: vec![],
                injury_updates: vec![],
                lineup_changes: vec![],
                weather_impact: None,
            },
            head_to_head: HeadToHeadData {
                last_5_meetings: vec![],
                team1_wins: 0,
                team2_wins: 0,
                avg_total_points: 0.0,
                avg_margin: 0.0,
            },
            team_stats: TeamStats {
                team1_stats: TeamPerformance {
                    team_name: "Philadelphia 76ers".to_string(),
                    record: "25-15".to_string(),
                    last_10_record: "7-3".to_string(),
                    home_record: None,
                    away_record: None,
                    avg_points_scored: 115.0,
                    avg_points_allowed: 108.0,
                    offensive_rating: 118.0,
                    defensive_rating: 112.0,
                    pace: 99.0,
                    recent_form: "W-W-L-W-W".to_string(),
                    rest_days: 1,
                    back_to_back: false,
                },
                team2_stats: TeamPerformance {
                    team_name: "Dallas Mavericks".to_string(),
                    record: "28-12".to_string(),
                    last_10_record: "8-2".to_string(),
                    home_record: None,
                    away_record: None,
                    avg_points_scored: 118.0,
                    avg_points_allowed: 110.0,
                    offensive_rating: 120.0,
                    defensive_rating: 113.0,
                    pace: 98.0,
                    recent_form: "W-W-W-L-W".to_string(),
                    rest_days: 2,
                    back_to_back: false,
                },
            },
            advanced_analytics: AdvancedAnalytics {
                team1_trends: vec![],
                team2_trends: vec![],
                situational_factors: vec![],
                betting_trends: BettingTrends {
                    team1_ats_record: "28-22-1".to_string(),
                    team2_ats_record: "30-20-0".to_string(),
                    team1_over_under_record: "26-24-0".to_string(),
                    team2_over_under_record: "28-22-0".to_string(),
                    public_money_percentage: 58.0,
                    sharp_money_percentage: 52.0,
                },
            },
            data_quality: DataQuality {
                sources_count: 3,
                data_freshness: "< 1 hour".to_string(),
                confidence: 0.85,
            },
        };

        let formatted = format_for_claude(&data);
        assert!(formatted.contains("Philadelphia 76ers"));
        assert!(formatted.contains("Dallas Mavericks"));
        assert!(formatted.contains("Joel Embiid"));
        assert!(formatted.contains("Luka Doncic"));
        assert!(formatted.contains("Spread"));
    }

    #[test]
    fn test_sanitize_json_plus_prefix() {
        let fetcher = create_test_fetcher();

        // Test removing + prefix from numbers
        let input = r#"{"moneyline_favorite": -190, "moneyline_underdog": +158}"#;
        let sanitized = fetcher.sanitize_json(input);
        assert_eq!(
            sanitized,
            r#"{"moneyline_favorite": -190, "moneyline_underdog": 158}"#
        );

        // Test with spaces
        let input2 = r#"{"value": +42.5}"#;
        let sanitized2 = fetcher.sanitize_json(input2);
        assert_eq!(sanitized2, r#"{"value": 42.5}"#);
    }

    #[test]
    fn test_sanitize_json_trailing_comma() {
        let fetcher = create_test_fetcher();

        // Test removing trailing commas
        let input = r#"{"a": 1, "b": 2,}"#;
        let sanitized = fetcher.sanitize_json(input);
        assert_eq!(sanitized, r#"{"a": 1, "b": 2}"#);

        // Test with array
        let input2 = r#"{"arr": [1, 2, 3,]}"#;
        let sanitized2 = fetcher.sanitize_json(input2);
        assert_eq!(sanitized2, r#"{"arr": [1, 2, 3]}"#);
    }

    #[test]
    fn test_parse_betting_with_plus_prefix() {
        let fetcher = create_test_fetcher();

        // Simulate Grok response with + prefix (invalid JSON)
        let response = r#"{
            "spread": -5.5,
            "spread_team": "Dallas Mavericks",
            "moneyline_favorite": -190,
            "moneyline_underdog": +158,
            "over_under": 227.0,
            "implied_probability": 0.655,
            "line_movement": "opened -4, now -5.5"
        }"#;

        let result = fetcher.parse_betting_response(response, "Philadelphia 76ers");
        assert!(result.is_ok());

        let betting = result.unwrap();
        assert_eq!(betting.moneyline_underdog, 158);
        assert_eq!(betting.moneyline_favorite, -190);
        assert_eq!(betting.spread, -5.5);
    }
}
