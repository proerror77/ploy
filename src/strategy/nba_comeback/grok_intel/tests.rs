use chrono::Utc;
use rust_decimal_macros::dec;

use super::*;

fn sample_game() -> crate::strategy::nba_comeback::espn::LiveGame {
    use crate::strategy::nba_comeback::espn::GameStatus;

    crate::strategy::nba_comeback::espn::LiveGame {
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
    assert!(prompt.contains("85"));
    assert!(prompt.contains("72"));
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
    let intel = GrokGameIntel {
        game_id: "401584701".to_string(),
        queried_at: Utc::now(),
        injury_updates: vec![InjuryUpdate {
            player_name: "Jayson Tatum".to_string(),
            team_abbrev: "BOS".to_string(),
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

    let signal = GrokSignalEvaluator::evaluate(&intel, &game, "LAL", dec!(0.30), 0.05, 0.5);

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
        momentum_direction: MomentumDirection::AwayTeamSurge,
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
        grok_home_win_prob: Some(0.40),
        grok_confidence: 0.85,
        key_factors: Vec::new(),
        raw_response: String::new(),
    };

    let signal = GrokSignalEvaluator::evaluate(&intel, &game, "LAL", dec!(0.30), 0.05, 0.5);

    assert!(signal.is_some(), "should produce fair value edge signal");
    let sig = signal.unwrap();
    assert_eq!(sig.signal_type, GrokSignalType::FairValueEdge);
    assert!(sig.edge > 0.20);
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
        grok_confidence: 0.3,
        key_factors: Vec::new(),
        raw_response: String::new(),
    };

    let signal = GrokSignalEvaluator::evaluate(&intel, &game, "LAL", dec!(0.30), 0.05, 0.5);

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
