use serde::{Deserialize, Serialize};

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
