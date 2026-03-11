use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Direction of momentum detected from X.com chatter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentumDirection {
    HomeTeamSurge,
    AwayTeamSurge,
    Neutral,
}

impl MomentumDirection {
    pub(super) fn from_str_loose(s: &str) -> Self {
        let lower = s.to_ascii_lowercase();
        if lower.contains("home") {
            Self::HomeTeamSurge
        } else if lower.contains("away") {
            Self::AwayTeamSurge
        } else {
            Self::Neutral
        }
    }
}

/// Impact level of an injury update
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjuryImpact {
    High,
    Medium,
    Low,
}

impl InjuryImpact {
    pub(super) fn from_str_loose(s: &str) -> Self {
        let lower = s.to_ascii_lowercase();
        if lower.contains("high") {
            Self::High
        } else if lower.contains("low") {
            Self::Low
        } else {
            Self::Medium
        }
    }
}

/// A single injury/availability update detected from X.com
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjuryUpdate {
    pub player_name: String,
    pub team_abbrev: String,
    /// "OUT", "RETURNED", "QUESTIONABLE"
    pub status: String,
    pub impact: InjuryImpact,
    pub details: String,
}

/// Structured intelligence returned from Grok for a live game
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokGameIntel {
    pub game_id: String,
    pub queried_at: DateTime<Utc>,
    /// Injury/availability shifts detected since game start
    pub injury_updates: Vec<InjuryUpdate>,
    /// Momentum narrative from X.com
    pub momentum_narrative: String,
    pub momentum_direction: MomentumDirection,
    /// X.com sentiment for home team (-1.0 to 1.0)
    pub home_sentiment_score: f64,
    /// X.com sentiment for away team (-1.0 to 1.0)
    pub away_sentiment_score: f64,
    /// Grok's estimated fair win probability for home team (independent of our model)
    pub grok_home_win_prob: Option<f64>,
    /// Grok's confidence in its assessment (0.0 to 1.0)
    pub grok_confidence: f64,
    /// Key factors driving the assessment
    pub key_factors: Vec<String>,
    /// Raw response for audit
    pub raw_response: String,
}

/// Type of edge detected by Grok signal evaluator
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokSignalType {
    /// Star player injury creates mispriced opponent odds
    InjuryEdge,
    /// Momentum surge + sentiment alignment
    MomentumEdge,
    /// Grok fair prob significantly diverges from market price
    FairValueEdge,
}

impl std::fmt::Display for GrokSignalType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InjuryEdge => write!(f, "injury_edge"),
            Self::MomentumEdge => write!(f, "momentum_edge"),
            Self::FairValueEdge => write!(f, "fair_value_edge"),
        }
    }
}

/// A tradeable signal produced by Grok intelligence
#[derive(Debug, Clone, Serialize)]
pub struct GrokTradeSignal {
    pub signal_type: GrokSignalType,
    pub target_team_abbrev: String,
    pub estimated_fair_value: f64,
    pub market_price: Decimal,
    pub edge: f64,
    pub confidence: f64,
    pub reasoning: String,
}
