//! ESPN Live Scoreboard Client
//!
//! Fetches live NBA game data from ESPN's public scoreboard API.
//! No API key required.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

// ── Public types ────────────────────────────────────────────────

/// Game status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GameStatus {
    Scheduled,
    InProgress,
    Final,
    Unknown,
}

/// Per-quarter score
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuarterScore {
    pub period: u8,
    pub points: f64,
}

/// A live NBA game parsed from ESPN data
#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveGame {
    pub espn_game_id: String,
    pub home_team: String,
    pub away_team: String,
    pub home_abbrev: String,
    pub away_abbrev: String,
    pub home_score: i32,
    pub away_score: i32,
    pub quarter: u8,
    pub clock: String,
    pub time_remaining_mins: f64,
    pub status: GameStatus,
    pub home_quarter_scores: Vec<QuarterScore>,
    pub away_quarter_scores: Vec<QuarterScore>,
}

impl LiveGame {
    /// Point differential from home team's perspective
    pub fn home_diff(&self) -> i32 {
        self.home_score - self.away_score
    }

    /// Which team is trailing and by how much
    /// Returns (trailing_team_name, trailing_abbrev, deficit) or None if tied
    pub fn trailing_team(&self) -> Option<(String, String, i32)> {
        let diff = self.home_score - self.away_score;
        if diff > 0 {
            // Away team is trailing
            Some((self.away_team.clone(), self.away_abbrev.clone(), diff))
        } else if diff < 0 {
            // Home team is trailing
            Some((self.home_team.clone(), self.home_abbrev.clone(), -diff))
        } else {
            None // Tied
        }
    }
}

// ── ESPN JSON deserialization structs ────────────────────────────

#[derive(Debug, Deserialize)]
struct EspnResponse {
    events: Vec<EspnEvent>,
}

#[derive(Debug, Deserialize)]
struct EspnEvent {
    id: String,
    competitions: Vec<EspnCompetition>,
}

#[derive(Debug, Deserialize)]
struct EspnCompetition {
    competitors: Vec<EspnCompetitor>,
    status: EspnStatus,
}

#[derive(Debug, Deserialize)]
struct EspnCompetitor {
    team: EspnTeam,
    #[serde(rename = "homeAway")]
    home_away: String,
    score: Option<String>,
    linescores: Option<Vec<EspnLinescore>>,
}

#[derive(Debug, Deserialize)]
struct EspnTeam {
    abbreviation: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct EspnLinescore {
    value: f64,
}

#[derive(Debug, Deserialize)]
struct EspnStatus {
    period: u8,
    #[serde(rename = "displayClock")]
    display_clock: String,
    #[serde(rename = "type")]
    status_type: EspnStatusType,
}

#[derive(Debug, Deserialize)]
struct EspnStatusType {
    state: String,
}

// ── Client ──────────────────────────────────────────────────────

const ESPN_SCOREBOARD_URL: &str =
    "https://site.api.espn.com/apis/site/v2/sports/basketball/nba/scoreboard";

/// ESPN live scoreboard client
pub struct EspnClient {
    http: reqwest::Client,
}

impl EspnClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Fetch all live NBA games from ESPN scoreboard
    pub async fn fetch_live_games(&self) -> Result<Vec<LiveGame>> {
        self.fetch_games_for_date_internal(None).await
    }

    /// Fetch NBA games for a specific calendar date (UTC), used for schedule syncing.
    pub async fn fetch_games_for_date(&self, date: NaiveDate) -> Result<Vec<LiveGame>> {
        self.fetch_games_for_date_internal(Some(date)).await
    }

    async fn fetch_games_for_date_internal(
        &self,
        date: Option<NaiveDate>,
    ) -> Result<Vec<LiveGame>> {
        let mut req = self.http.get(ESPN_SCOREBOARD_URL);
        if let Some(d) = date {
            req = req.query(&[("dates", d.format("%Y%m%d").to_string())]);
        }

        // ESPN supports date-scoped scoreboard queries via `dates=YYYYMMDD`.
        let resp = req.send().await.context("ESPN scoreboard request failed")?;

        let data: EspnResponse = resp
            .json()
            .await
            .context("ESPN scoreboard JSON parse failed")?;

        let mut games = Vec::new();
        for event in &data.events {
            if let Some(game) = Self::parse_event(event) {
                games.push(game);
            }
        }

        debug!("ESPN: fetched {} games", games.len());
        Ok(games)
    }

    /// Filter games currently in a specific quarter
    pub fn games_in_quarter(games: &[LiveGame], quarter: u8) -> Vec<&LiveGame> {
        games
            .iter()
            .filter(|g| g.status == GameStatus::InProgress && g.quarter == quarter)
            .collect()
    }

    fn parse_event(event: &EspnEvent) -> Option<LiveGame> {
        let comp = event.competitions.first()?;
        if comp.competitors.len() < 2 {
            return None;
        }

        let (home, away) = Self::split_competitors(&comp.competitors)?;

        let home_score = home.score.as_deref().unwrap_or("0").parse().unwrap_or(0);
        let away_score = away.score.as_deref().unwrap_or("0").parse().unwrap_or(0);

        let status = match comp.status.status_type.state.as_str() {
            "in" => GameStatus::InProgress,
            "post" => GameStatus::Final,
            "pre" => GameStatus::Scheduled,
            _ => GameStatus::Unknown,
        };

        let quarter = comp.status.period;
        let clock = comp.status.display_clock.clone();
        let time_remaining_mins = Self::calc_time_remaining(quarter, &clock);

        let home_qs = Self::parse_linescores(&home.linescores);
        let away_qs = Self::parse_linescores(&away.linescores);

        Some(LiveGame {
            espn_game_id: event.id.clone(),
            home_team: home.team.display_name.clone(),
            away_team: away.team.display_name.clone(),
            home_abbrev: home.team.abbreviation.clone(),
            away_abbrev: away.team.abbreviation.clone(),
            home_score,
            away_score,
            quarter,
            clock,
            time_remaining_mins,
            status,
            home_quarter_scores: home_qs,
            away_quarter_scores: away_qs,
        })
    }

    fn split_competitors<'a>(
        comps: &'a [EspnCompetitor],
    ) -> Option<(&'a EspnCompetitor, &'a EspnCompetitor)> {
        let home = comps.iter().find(|c| c.home_away == "home")?;
        let away = comps.iter().find(|c| c.home_away == "away")?;
        Some((home, away))
    }

    /// Calculate total minutes remaining in the game.
    /// NBA: 4 quarters x 12 minutes = 48 minutes total.
    fn calc_time_remaining(quarter: u8, clock: &str) -> f64 {
        let clock_mins = Self::parse_clock(clock);
        let quarters_left = if quarter <= 4 {
            (4u8.saturating_sub(quarter)) as f64
        } else {
            0.0 // overtime
        };
        quarters_left * 12.0 + clock_mins
    }

    /// Parse "5:42" or "0:30.2" into fractional minutes
    fn parse_clock(clock: &str) -> f64 {
        let parts: Vec<&str> = clock.split(':').collect();
        if parts.len() == 2 {
            let mins: f64 = parts[0].parse().unwrap_or(0.0);
            let secs: f64 = parts[1].parse().unwrap_or(0.0);
            mins + secs / 60.0
        } else {
            0.0
        }
    }

    fn parse_linescores(ls: &Option<Vec<EspnLinescore>>) -> Vec<QuarterScore> {
        match ls {
            Some(scores) => scores
                .iter()
                .enumerate()
                .map(|(i, s)| QuarterScore {
                    period: (i + 1) as u8,
                    points: s.value,
                })
                .collect(),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clock() {
        assert!((EspnClient::parse_clock("5:42") - 5.7).abs() < 0.1);
        assert!((EspnClient::parse_clock("0:30") - 0.5).abs() < 0.01);
        assert!((EspnClient::parse_clock("12:00") - 12.0).abs() < 0.01);
    }

    #[test]
    fn test_time_remaining() {
        // Q3, 5:42 on clock → 1 quarter left (12 min) + 5.7 min = ~17.7
        let tr = EspnClient::calc_time_remaining(3, "5:42");
        assert!((tr - 17.7).abs() < 0.2);

        // Q4, 2:00 on clock → 0 quarters left + 2.0 min = 2.0
        let tr = EspnClient::calc_time_remaining(4, "2:00");
        assert!((tr - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_trailing_team() {
        let game = LiveGame {
            espn_game_id: "1".into(),
            home_team: "Lakers".into(),
            away_team: "Celtics".into(),
            home_abbrev: "LAL".into(),
            away_abbrev: "BOS".into(),
            home_score: 75,
            away_score: 82,
            quarter: 3,
            clock: "5:00".into(),
            time_remaining_mins: 17.0,
            status: GameStatus::InProgress,
            home_quarter_scores: vec![],
            away_quarter_scores: vec![],
        };

        let (_name, abbrev, deficit) = game.trailing_team().unwrap();
        assert_eq!(abbrev, "LAL");
        assert_eq!(deficit, 7);
    }

    #[test]
    fn test_parse_espn_json() {
        let json = r#"{
            "events": [{
                "id": "401584701",
                "competitions": [{
                    "competitors": [
                        {
                            "team": {"abbreviation": "BOS", "displayName": "Boston Celtics"},
                            "homeAway": "home",
                            "score": "89",
                            "linescores": [{"value": 28.0}, {"value": 31.0}, {"value": 30.0}]
                        },
                        {
                            "team": {"abbreviation": "LAL", "displayName": "Los Angeles Lakers"},
                            "homeAway": "away",
                            "score": "82",
                            "linescores": [{"value": 25.0}, {"value": 29.0}, {"value": 28.0}]
                        }
                    ],
                    "status": {
                        "period": 3,
                        "displayClock": "5:42",
                        "type": {"state": "in"}
                    }
                }]
            }]
        }"#;

        let resp: EspnResponse = serde_json::from_str(json).unwrap();
        let game = EspnClient::parse_event(&resp.events[0]).unwrap();

        assert_eq!(game.espn_game_id, "401584701");
        assert_eq!(game.home_abbrev, "BOS");
        assert_eq!(game.away_abbrev, "LAL");
        assert_eq!(game.home_score, 89);
        assert_eq!(game.away_score, 82);
        assert_eq!(game.quarter, 3);
        assert_eq!(game.status, GameStatus::InProgress);
        assert!((game.time_remaining_mins - 17.7).abs() < 0.2);
    }
}
