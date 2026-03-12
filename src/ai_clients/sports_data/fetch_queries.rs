use tracing::{debug, warn};

use crate::error::Result;

use super::{
    AdvancedAnalytics, BettingLines, BettingTrends, HeadToHeadData, NewsData, PlayerStatus,
    SentimentData, SportsDataFetcher, TeamPerformance, TeamStats,
};

impl SportsDataFetcher {
    /// Fetch player injury/status data in structured format
    pub(super) async fn fetch_player_status(
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
    pub(super) async fn fetch_betting_lines(
        &self,
        team1: &str,
        team2: &str,
    ) -> Result<BettingLines> {
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
    pub(super) async fn fetch_sentiment(&self, team1: &str, team2: &str) -> Result<SentimentData> {
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

    /// Fetch breaking news and recent developments
    pub(super) async fn fetch_news(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<NewsData> {
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
    pub(super) async fn fetch_head_to_head(
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
    pub(super) async fn fetch_team_stats(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<TeamStats> {
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
    pub(super) async fn fetch_advanced_analytics(
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
    pub(super) fn parse_news_response(&self, response: &str) -> Result<NewsData> {
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
    pub(super) fn parse_h2h_response(
        &self,
        response: &str,
        _team1: &str,
    ) -> Result<HeadToHeadData> {
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
    pub(super) fn parse_team_stats_response(
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
    pub(super) fn parse_advanced_analytics_response(
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
}
