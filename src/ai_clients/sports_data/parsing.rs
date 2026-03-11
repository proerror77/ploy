use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::{PloyError, Result};

use super::{BettingLines, PlayerStatus, SentimentData, SportsDataFetcher};

impl SportsDataFetcher {
    /// Parse player status response
    pub(super) fn parse_player_response(
        &self,
        response: &str,
        _team1: &str,
        _team2: &str,
    ) -> Result<(Vec<PlayerStatus>, Vec<PlayerStatus>)> {
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
                Ok((vec![], vec![]))
            }
        }
    }

    /// Parse betting lines response
    pub(super) fn parse_betting_response(
        &self,
        response: &str,
        team1: &str,
    ) -> Result<BettingLines> {
        let json_str = self.extract_json(response)?;

        let sanitized = self.sanitize_json(&json_str);

        match serde_json::from_str::<BettingLines>(&sanitized) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse betting response: {}", e);
                debug!(
                    "Problematic JSON: {}",
                    &sanitized[..sanitized.len().min(500)]
                );
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
    pub(super) fn sanitize_json(&self, json: &str) -> String {
        let mut result = String::with_capacity(json.len());
        let chars: Vec<char> = json.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            if c == ':' {
                result.push(c);
                i += 1;

                while i < chars.len() && chars[i].is_whitespace() {
                    result.push(chars[i]);
                    i += 1;
                }

                if i < chars.len() && chars[i] == '+' {
                    if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                        i += 1;
                    }
                }
                continue;
            }

            result.push(c);
            i += 1;
        }

        result
            .replace(",}", "}")
            .replace(",]", "]")
            .replace(", }", "}")
            .replace(", ]", "]")
    }

    /// Parse sentiment response
    pub(super) fn parse_sentiment_response(
        &self,
        response: &str,
        team1: &str,
    ) -> Result<SentimentData> {
        let json_str = self.extract_json(response)?;

        match serde_json::from_str::<SentimentData>(&json_str) {
            Ok(parsed) => Ok(parsed),
            Err(e) => {
                warn!("Failed to parse sentiment response: {}", e);
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

    /// Extract JSON from a response that might have markdown or other text
    pub(super) fn extract_json(&self, response: &str) -> Result<String> {
        let response = response.trim();

        if response.starts_with('{') {
            if let Some(end) = response.rfind('}') {
                return Ok(response[..=end].to_string());
            }
        }

        if let Some(start) = response.find("```json") {
            let after_marker = &response[start + 7..];
            if let Some(end) = after_marker.find("```") {
                return Ok(after_marker[..end].trim().to_string());
            }
        }

        if let Some(start) = response.find("```") {
            let after_marker = &response[start + 3..];
            if let Some(end) = after_marker.find("```") {
                let content = after_marker[..end].trim();
                if content.starts_with('{') {
                    return Ok(content.to_string());
                }
            }
        }

        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                return Ok(response[start..=end].to_string());
            }
        }

        Err(PloyError::Internal("No JSON found in response".into()))
    }
}
