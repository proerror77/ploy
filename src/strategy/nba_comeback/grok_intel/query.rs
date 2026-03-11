use tracing::{debug, warn};

use crate::ai_clients::grok::GrokClient;
use crate::strategy::nba_comeback::espn::LiveGame;

use super::parse_grok_response;

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

/// Query Grok for live intel on a specific game
pub async fn query_grok_for_game(
    grok: &GrokClient,
    game: &LiveGame,
) -> std::result::Result<super::GrokGameIntel, String> {
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
