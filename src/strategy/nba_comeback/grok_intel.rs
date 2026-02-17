//! Grok X.com Live Search Integration for NBA Games
//!
//! Queries Grok (xAI API with real-time X.com search) for live NBA game
//! intelligence: injury updates, momentum shifts, fan/analyst sentiment,
//! and independent win probability estimates.
//!
//! Produces `GrokGameIntel` structs that can be evaluated by
//! `GrokSignalEvaluator` to generate independent trading signals.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::agent::grok::GrokClient;
use crate::strategy::nba_comeback::espn::LiveGame;

// ── Types ──────────────────────────────────────────────────────

/// Direction of momentum detected from X.com chatter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentumDirection {
    HomeTeamSurge,
    AwayTeamSurge,
    Neutral,
}

impl MomentumDirection {
    fn from_str_loose(s: &str) -> Self {
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
    fn from_str_loose(s: &str) -> Self {
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

// ── Grok JSON response schema ──────────────────────────────────

/// Intermediate struct for parsing Grok's JSON response
#[derive(Debug, Deserialize)]
struct GrokJsonResponse {
    #[serde(default)]
    injuries: Vec<GrokInjuryJson>,
    #[serde(default)]
    momentum_narrative: String,
    #[serde(default)]
    momentum_direction: String,
    #[serde(default)]
    home_sentiment: f64,
    #[serde(default)]
    away_sentiment: f64,
    #[serde(default)]
    home_win_probability: Option<f64>,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    key_factors: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GrokInjuryJson {
    #[serde(default)]
    player: String,
    #[serde(default)]
    team: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    impact: String,
    #[serde(default)]
    details: String,
}

// ── Prompt builder ─────────────────────────────────────────────

/// Build the Grok query prompt for a specific live game
pub fn build_grok_game_prompt(game: &LiveGame) -> String {
    format!(
        r#"Search X.com for live updates on the {away} vs {home} NBA game. Current score: {away} {away_score}, {home} {home_score} (Q{quarter} {clock}).

Report the following in JSON format with these exact keys:
{{
  "injuries": [
    {{"player": "Name", "team": "ABBREV", "status": "OUT|RETURNED|QUESTIONABLE", "impact": "high|medium|low", "details": "brief description"}}
  ],
  "momentum_narrative": "2-3 sentence summary of game momentum and key plays",
  "momentum_direction": "home_surge|away_surge|neutral",
  "home_sentiment": 0.0,
  "away_sentiment": 0.0,
  "home_win_probability": 0.0,
  "confidence": 0.0,
  "key_factors": ["factor1", "factor2"]
}}

Rules:
- injuries: only include changes SINCE the game started (not pre-game injury reports)
- sentiment scores: -1.0 (very negative) to 1.0 (very positive) based on X.com fan/analyst posts
- home_win_probability: your best estimate of {home}'s chance to win (0.0 to 1.0)
- confidence: how confident you are in your assessment (0.0 to 1.0)
- key_factors: 2-4 key factors driving the assessment

Respond ONLY with the JSON object, no other text."#,
        home = game.home_team,
        away = game.away_team,
        home_score = game.home_score,
        away_score = game.away_score,
        quarter = game.quarter,
        clock = game.clock,
    )
}

// ── Response parser ────────────────────────────────────────────

/// Parse Grok's raw text response into a structured GrokGameIntel
pub fn parse_grok_response(game_id: &str, raw: &str) -> GrokGameIntel {
    let now = Utc::now();

    // Try to extract JSON from the response (Grok may wrap it in markdown)
    let json_str = extract_json_block(raw);

    match serde_json::from_str::<GrokJsonResponse>(&json_str) {
        Ok(parsed) => GrokGameIntel {
            game_id: game_id.to_string(),
            queried_at: now,
            injury_updates: parsed
                .injuries
                .into_iter()
                .map(|inj| InjuryUpdate {
                    player_name: inj.player,
                    team_abbrev: inj.team,
                    status: inj.status.to_ascii_uppercase(),
                    impact: InjuryImpact::from_str_loose(&inj.impact),
                    details: inj.details,
                })
                .collect(),
            momentum_narrative: parsed.momentum_narrative,
            momentum_direction: MomentumDirection::from_str_loose(&parsed.momentum_direction),
            home_sentiment_score: parsed.home_sentiment.clamp(-1.0, 1.0),
            away_sentiment_score: parsed.away_sentiment.clamp(-1.0, 1.0),
            grok_home_win_prob: parsed.home_win_probability.map(|p| p.clamp(0.0, 1.0)),
            grok_confidence: parsed.confidence.clamp(0.0, 1.0),
            key_factors: parsed.key_factors,
            raw_response: raw.to_string(),
        },
        Err(e) => {
            warn!(game_id, error = %e, "failed to parse Grok JSON response, using defaults");
            GrokGameIntel {
                game_id: game_id.to_string(),
                queried_at: now,
                injury_updates: Vec::new(),
                momentum_narrative: String::new(),
                momentum_direction: MomentumDirection::Neutral,
                home_sentiment_score: 0.0,
                away_sentiment_score: 0.0,
                grok_home_win_prob: None,
                grok_confidence: 0.0,
                key_factors: Vec::new(),
                raw_response: raw.to_string(),
            }
        }
    }
}

/// Extract JSON object from a response that may contain markdown fences
pub(crate) fn extract_json_block(raw: &str) -> String {
    let trimmed = raw.trim();

    // Try stripping ```json ... ``` fences
    if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        if let Some(end) = after_fence.find("```") {
            let block = after_fence[..end].trim();
            if block.starts_with('{') {
                return block.to_string();
            }
        }
    }

    // Try finding raw JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

// ── Query helper ───────────────────────────────────────────────

/// Query Grok for live intel on a specific game
pub async fn query_grok_for_game(
    grok: &GrokClient,
    game: &LiveGame,
) -> std::result::Result<GrokGameIntel, String> {
    let prompt = build_grok_game_prompt(game);
    let start = std::time::Instant::now();

    match grok.chat(&prompt).await {
        Ok(raw) => {
            let duration_ms = start.elapsed().as_millis() as u32;
            debug!(
                game_id = %game.espn_game_id,
                duration_ms,
                response_len = raw.len(),
                "grok query completed"
            );
            Ok(parse_grok_response(&game.espn_game_id, &raw))
        }
        Err(e) => {
            warn!(
                game_id = %game.espn_game_id,
                error = %e,
                "grok query failed"
            );
            Err(format!("Grok query failed: {}", e))
        }
    }
}

// ── Signal evaluator ───────────────────────────────────────────

pub struct GrokSignalEvaluator;

impl GrokSignalEvaluator {
    /// Evaluate Grok intel for tradeable signals.
    ///
    /// `trailing_abbrev`: the team currently trailing in the game
    /// `market_price`: current Polymarket price for the trailing team's YES token
    /// `min_edge`: minimum edge required to generate a signal
    /// `min_confidence`: minimum Grok confidence required to act
    pub fn evaluate(
        intel: &GrokGameIntel,
        game: &LiveGame,
        trailing_abbrev: &str,
        market_price: Decimal,
        min_edge: f64,
        min_confidence: f64,
    ) -> Option<GrokTradeSignal> {
        let market_price_f64 = market_price.to_string().parse::<f64>().unwrap_or(1.0);

        // Skip low-confidence assessments
        if intel.grok_confidence < min_confidence {
            debug!(
                game_id = %intel.game_id,
                confidence = intel.grok_confidence,
                min_confidence,
                "grok confidence below threshold"
            );
            return None;
        }

        // Determine if the trailing team is home or away
        let trailing_is_home = game.home_abbrev == trailing_abbrev;

        // --- Signal 1: Star injury on LEADING team → trailing team's price should rise ---
        let injury_signal = Self::evaluate_injury_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if injury_signal.is_some() {
            return injury_signal;
        }

        // --- Signal 2: Momentum surge + sentiment → trailing team rallying ---
        let momentum_signal = Self::evaluate_momentum_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if momentum_signal.is_some() {
            return momentum_signal;
        }

        // --- Signal 3: Grok fair prob diverges from market price ---
        let fair_value_signal = Self::evaluate_fair_value_edge(
            intel,
            trailing_abbrev,
            trailing_is_home,
            market_price_f64,
            min_edge,
        );
        if fair_value_signal.is_some() {
            return fair_value_signal;
        }

        None
    }

    /// Signal 1: High-impact injury on the LEADING team creates edge for trailing team
    fn evaluate_injury_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        // Look for high-impact injuries on the LEADING team (opposing trailing)
        let leading_injuries: Vec<&InjuryUpdate> = intel
            .injury_updates
            .iter()
            .filter(|inj| {
                inj.team_abbrev != trailing_abbrev
                    && inj.impact == InjuryImpact::High
                    && inj.status == "OUT"
            })
            .collect();

        if leading_injuries.is_empty() {
            return None;
        }

        // Star player out on leading team → bump trailing team's fair value
        // Rough heuristic: each high-impact OUT player adds ~5-8% to trailing team's win prob
        let injury_boost = (leading_injuries.len() as f64 * 0.06).min(0.15);
        let base_prob = if trailing_is_home {
            intel.grok_home_win_prob.unwrap_or(market_price)
        } else {
            intel
                .grok_home_win_prob
                .map(|p| 1.0 - p)
                .unwrap_or(market_price)
        };
        let fair_value = (base_prob + injury_boost).min(0.95);
        let edge = fair_value - market_price;

        if edge < min_edge {
            return None;
        }

        let player_names: Vec<&str> = leading_injuries
            .iter()
            .map(|i| i.player_name.as_str())
            .collect();
        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            edge = format!("{:.3}", edge),
            fair_value = format!("{:.3}", fair_value),
            injured = ?player_names,
            "grok injury edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::InjuryEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: fair_value,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence,
            reasoning: format!(
                "High-impact injury on opposing team ({}). Fair value {:.1}% vs market {:.1}%",
                player_names.join(", "),
                fair_value * 100.0,
                market_price * 100.0
            ),
        })
    }

    /// Signal 2: Momentum surge toward the trailing team + positive sentiment
    fn evaluate_momentum_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        // Check if momentum direction favors the trailing team
        let momentum_favors_trailing = match intel.momentum_direction {
            MomentumDirection::HomeTeamSurge => trailing_is_home,
            MomentumDirection::AwayTeamSurge => !trailing_is_home,
            MomentumDirection::Neutral => return None,
        };

        if !momentum_favors_trailing {
            return None;
        }

        // Check sentiment alignment (trailing team has positive sentiment)
        let trailing_sentiment = if trailing_is_home {
            intel.home_sentiment_score
        } else {
            intel.away_sentiment_score
        };

        // Require positive sentiment (> 0.2) for the trailing team
        if trailing_sentiment < 0.2 {
            return None;
        }

        // Momentum + sentiment → estimate fair value boost
        let sentiment_boost = trailing_sentiment * 0.04; // up to ~4% boost
        let momentum_boost = 0.03; // fixed 3% for momentum confirmation
        let base_prob = if trailing_is_home {
            intel.grok_home_win_prob.unwrap_or(market_price)
        } else {
            intel
                .grok_home_win_prob
                .map(|p| 1.0 - p)
                .unwrap_or(market_price)
        };
        let fair_value = (base_prob + sentiment_boost + momentum_boost).min(0.95);
        let edge = fair_value - market_price;

        if edge < min_edge {
            return None;
        }

        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            edge = format!("{:.3}", edge),
            sentiment = format!("{:.2}", trailing_sentiment),
            "grok momentum edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::MomentumEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: fair_value,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence * (0.5 + trailing_sentiment * 0.5),
            reasoning: format!(
                "Momentum surge + positive sentiment ({:.0}%) for {}. Fair value {:.1}% vs market {:.1}%",
                trailing_sentiment * 100.0,
                trailing_abbrev,
                fair_value * 100.0,
                market_price * 100.0
            ),
        })
    }

    /// Signal 3: Grok's fair probability significantly diverges from market price
    fn evaluate_fair_value_edge(
        intel: &GrokGameIntel,
        trailing_abbrev: &str,
        trailing_is_home: bool,
        market_price: f64,
        min_edge: f64,
    ) -> Option<GrokTradeSignal> {
        let grok_home_prob = intel.grok_home_win_prob?;

        let grok_trailing_prob = if trailing_is_home {
            grok_home_prob
        } else {
            1.0 - grok_home_prob
        };

        let edge = grok_trailing_prob - market_price;

        if edge < min_edge {
            return None;
        }

        info!(
            game_id = %intel.game_id,
            trailing = trailing_abbrev,
            grok_prob = format!("{:.3}", grok_trailing_prob),
            market_price = format!("{:.3}", market_price),
            edge = format!("{:.3}", edge),
            "grok fair value edge signal"
        );

        Some(GrokTradeSignal {
            signal_type: GrokSignalType::FairValueEdge,
            target_team_abbrev: trailing_abbrev.to_string(),
            estimated_fair_value: grok_trailing_prob,
            market_price: Decimal::from_f64_retain(market_price).unwrap_or(Decimal::ONE),
            edge,
            confidence: intel.grok_confidence,
            reasoning: format!(
                "Grok estimates {}% fair value for {} vs market {:.1}%",
                format!("{:.1}", grok_trailing_prob * 100.0),
                trailing_abbrev,
                market_price * 100.0
            ),
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_game() -> LiveGame {
        use crate::strategy::nba_comeback::espn::GameStatus;
        LiveGame {
            espn_game_id: "401584701".to_string(),
            home_team: "Boston Celtics".to_string(),
            away_team: "Los Angeles Lakers".to_string(),
            home_abbrev: "BOS".to_string(),
            away_abbrev: "LAL".to_string(),
            home_score: 85,
            away_score: 72,
            quarter: 3,
            clock: "4:30".to_string(),
            time_remaining_mins: 16.5,
            status: GameStatus::InProgress,
            home_quarter_scores: Vec::new(),
            away_quarter_scores: Vec::new(),
        }
    }

    #[test]
    fn test_build_prompt_contains_teams() {
        let game = sample_game();
        let prompt = build_grok_game_prompt(&game);
        assert!(prompt.contains("Boston Celtics"));
        assert!(prompt.contains("Los Angeles Lakers"));
        assert!(prompt.contains("Q3"));
        assert!(prompt.contains("4:30"));
        assert!(prompt.contains("85")); // home score
        assert!(prompt.contains("72")); // away score
    }

    #[test]
    fn test_parse_valid_json_response() {
        let raw = r#"```json
{
  "injuries": [
    {"player": "Jayson Tatum", "team": "BOS", "status": "OUT", "impact": "high", "details": "ankle sprain"}
  ],
  "momentum_narrative": "Lakers on a 12-0 run in Q3",
  "momentum_direction": "away_surge",
  "home_sentiment": -0.3,
  "away_sentiment": 0.7,
  "home_win_probability": 0.55,
  "confidence": 0.8,
  "key_factors": ["Tatum injury", "Lakers 12-0 run"]
}
```"#;

        let intel = parse_grok_response("401584701", raw);
        assert_eq!(intel.game_id, "401584701");
        assert_eq!(intel.injury_updates.len(), 1);
        assert_eq!(intel.injury_updates[0].player_name, "Jayson Tatum");
        assert_eq!(intel.injury_updates[0].impact, InjuryImpact::High);
        assert_eq!(intel.momentum_direction, MomentumDirection::AwayTeamSurge);
        assert!((intel.home_sentiment_score - (-0.3)).abs() < f64::EPSILON);
        assert!((intel.away_sentiment_score - 0.7).abs() < f64::EPSILON);
        assert_eq!(intel.grok_home_win_prob, Some(0.55));
        assert!((intel.grok_confidence - 0.8).abs() < f64::EPSILON);
        assert_eq!(intel.key_factors.len(), 2);
    }

    #[test]
    fn test_parse_malformed_response_returns_defaults() {
        let raw = "I couldn't find any updates about this game.";
        let intel = parse_grok_response("401584701", raw);
        assert_eq!(intel.game_id, "401584701");
        assert!(intel.injury_updates.is_empty());
        assert_eq!(intel.momentum_direction, MomentumDirection::Neutral);
        assert!(intel.grok_confidence < f64::EPSILON);
    }

    #[test]
    fn test_injury_edge_signal() {
        let game = sample_game();
        // LAL is trailing (away_score 72 < home_score 85)
        let intel = GrokGameIntel {
            game_id: "401584701".to_string(),
            queried_at: Utc::now(),
            injury_updates: vec![InjuryUpdate {
                player_name: "Jayson Tatum".to_string(),
                team_abbrev: "BOS".to_string(), // leading team player is OUT
                status: "OUT".to_string(),
                impact: InjuryImpact::High,
                details: "ankle".to_string(),
            }],
            momentum_narrative: String::new(),
            momentum_direction: MomentumDirection::Neutral,
            home_sentiment_score: 0.0,
            away_sentiment_score: 0.0,
            grok_home_win_prob: Some(0.55),
            grok_confidence: 0.8,
            key_factors: Vec::new(),
            raw_response: String::new(),
        };

        // LAL trailing at market price 0.30
        let signal = GrokSignalEvaluator::evaluate(
            &intel,
            &game,
            "LAL",
            dec!(0.30),
            0.05, // min edge 5%
            0.5,  // min confidence
        );

        assert!(signal.is_some(), "should produce injury edge signal");
        let sig = signal.unwrap();
        assert_eq!(sig.signal_type, GrokSignalType::InjuryEdge);
        assert_eq!(sig.target_team_abbrev, "LAL");
        assert!(sig.edge >= 0.05);
    }

    #[test]
    fn test_momentum_edge_signal() {
        let game = sample_game();
        let intel = GrokGameIntel {
            game_id: "401584701".to_string(),
            queried_at: Utc::now(),
            injury_updates: Vec::new(),
            momentum_narrative: "Lakers on a huge run".to_string(),
            momentum_direction: MomentumDirection::AwayTeamSurge, // LAL is away
            home_sentiment_score: -0.3,
            away_sentiment_score: 0.6,
            grok_home_win_prob: Some(0.50),
            grok_confidence: 0.7,
            key_factors: Vec::new(),
            raw_response: String::new(),
        };

        let signal = GrokSignalEvaluator::evaluate(&intel, &game, "LAL", dec!(0.30), 0.05, 0.5);

        assert!(signal.is_some(), "should produce momentum edge signal");
        let sig = signal.unwrap();
        assert_eq!(sig.signal_type, GrokSignalType::MomentumEdge);
    }

    #[test]
    fn test_fair_value_edge_signal() {
        let game = sample_game();
        let intel = GrokGameIntel {
            game_id: "401584701".to_string(),
            queried_at: Utc::now(),
            injury_updates: Vec::new(),
            momentum_narrative: String::new(),
            momentum_direction: MomentumDirection::Neutral,
            home_sentiment_score: 0.0,
            away_sentiment_score: 0.0,
            grok_home_win_prob: Some(0.40), // Grok thinks home only 40% → away 60%
            grok_confidence: 0.85,
            key_factors: Vec::new(),
            raw_response: String::new(),
        };

        // LAL (away) trailing at 0.30, but Grok thinks 60% fair → big edge
        let signal = GrokSignalEvaluator::evaluate(&intel, &game, "LAL", dec!(0.30), 0.05, 0.5);

        assert!(signal.is_some(), "should produce fair value edge signal");
        let sig = signal.unwrap();
        assert_eq!(sig.signal_type, GrokSignalType::FairValueEdge);
        assert!(sig.edge > 0.20); // 60% - 30% = 30% edge
    }

    #[test]
    fn test_no_signal_below_confidence() {
        let game = sample_game();
        let intel = GrokGameIntel {
            game_id: "401584701".to_string(),
            queried_at: Utc::now(),
            injury_updates: Vec::new(),
            momentum_narrative: String::new(),
            momentum_direction: MomentumDirection::Neutral,
            home_sentiment_score: 0.0,
            away_sentiment_score: 0.0,
            grok_home_win_prob: Some(0.40),
            grok_confidence: 0.3, // below threshold
            key_factors: Vec::new(),
            raw_response: String::new(),
        };

        let signal = GrokSignalEvaluator::evaluate(
            &intel,
            &game,
            "LAL",
            dec!(0.30),
            0.05,
            0.5, // min confidence 50%
        );

        assert!(
            signal.is_none(),
            "should not produce signal below confidence threshold"
        );
    }

    #[test]
    fn test_extract_json_from_markdown() {
        let raw = "Here is my analysis:\n```json\n{\"confidence\": 0.8}\n```\nThank you.";
        let extracted = extract_json_block(raw);
        assert_eq!(extracted, "{\"confidence\": 0.8}");
    }

    #[test]
    fn test_extract_json_bare() {
        let raw = "{\"confidence\": 0.8}";
        let extracted = extract_json_block(raw);
        assert_eq!(extracted, "{\"confidence\": 0.8}");
    }

    #[test]
    fn test_momentum_direction_from_str() {
        assert_eq!(
            MomentumDirection::from_str_loose("home_surge"),
            MomentumDirection::HomeTeamSurge
        );
        assert_eq!(
            MomentumDirection::from_str_loose("away_surge"),
            MomentumDirection::AwayTeamSurge
        );
        assert_eq!(
            MomentumDirection::from_str_loose("neutral"),
            MomentumDirection::Neutral
        );
        assert_eq!(
            MomentumDirection::from_str_loose("something_random"),
            MomentumDirection::Neutral
        );
    }
}
