use chrono::Utc;
use serde::Deserialize;
use tracing::warn;

use super::{GrokGameIntel, InjuryImpact, InjuryUpdate, MomentumDirection};

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

/// Parse Grok's raw text response into a structured GrokGameIntel
pub fn parse_grok_response(game_id: &str, raw: &str) -> GrokGameIntel {
    let now = Utc::now();
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

    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}
