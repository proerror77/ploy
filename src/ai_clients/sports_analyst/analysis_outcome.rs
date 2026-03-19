use super::*;

/// Sports analysis with DraftKings odds comparison.
#[derive(Debug, Clone)]
pub struct SportsAnalysisWithDK {
    pub base: SportsAnalysis,
    pub draftkings: Option<crate::ai_clients::odds_provider::EdgeAnalysis>,
}

impl SportsAnalyst {
    pub(super) async fn get_claude_prediction(
        &self,
        team1: &str,
        team2: &str,
        odds: &MarketOdds,
        structured_data: Option<&StructuredGameData>,
    ) -> Result<WinPrediction> {
        let data_section = match structured_data {
            Some(data) => format_for_claude(data),
            None => format!(
                "## Game: {} vs {}\n\
                (No structured data available - using Polymarket odds only)\n",
                team1, team2
            ),
        };

        let prompt = format!(
            r#"You are an expert sports analyst. Analyze this matchup and predict win probabilities.

{data_section}

## Polymarket Odds (for comparison)
{team1} YES: {:.3} (implied {:.1}%)
{team2} YES: {:.3} (implied {:.1}%)

## Your Task
Analyze ALL the structured data above carefully:
1. Player availability and recent performance
2. Betting line consensus and movement
3. Expert picks and public sentiment
4. Compare your analysis to market odds

Provide your prediction in this EXACT JSON format (no other text):
```json
{{
  "team1_win_prob": 0.XX,
  "team2_win_prob": 0.XX,
  "confidence": 0.XX,
  "reasoning": "2-3 sentence explanation of key factors",
  "key_factors": ["factor1", "factor2", "factor3"]
}}
```

IMPORTANT:
- team1_win_prob + team2_win_prob MUST equal 1.0
- confidence is 0.0-1.0 (how sure you are)
- Be specific in reasoning - cite actual player data or betting line movements"#,
            odds.team1_yes_price,
            odds.team1_yes_price
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.5)
                * 100.0,
            odds.team2_yes_price.unwrap_or(Decimal::new(50, 2)),
            odds.team2_yes_price
                .map(|p| p.to_string().parse::<f64>().unwrap_or(0.5) * 100.0)
                .unwrap_or(50.0),
            data_section = data_section,
            team1 = team1,
            team2 = team2,
        );

        let response = self.claude.simple_query(&prompt).await?;
        self.parse_prediction_response(&response, team1, team2)
    }

    pub(super) fn parse_prediction_response(
        &self,
        response: &str,
        team1: &str,
        team2: &str,
    ) -> Result<WinPrediction> {
        if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                let json_str = &response[start..=end];
                if let Ok(pred) = serde_json::from_str::<WinPrediction>(json_str) {
                    return Ok(pred);
                }
            }
        }

        Ok(WinPrediction {
            team1_win_prob: 0.5,
            team2_win_prob: 0.5,
            confidence: 0.5,
            reasoning: format!(
                "Could not parse detailed prediction for {} vs {}",
                team1, team2
            ),
            key_factors: vec!["Insufficient data".to_string()],
        })
    }

    pub(super) fn generate_recommendation(
        &self,
        team1: &str,
        team2: &str,
        odds: &MarketOdds,
        prediction: &WinPrediction,
    ) -> TradeRecommendation {
        let market_prob = odds
            .team1_yes_price
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.5);
        let predicted_prob = prediction.team1_win_prob;
        let edge = predicted_prob - market_prob;

        const MIN_EDGE: f64 = 0.05;
        const MIN_CONFIDENCE: f64 = 0.7;

        let team2_edge = -edge;

        let (action, side, display_edge, reasoning) = if prediction.confidence < MIN_CONFIDENCE {
            (
                TradeAction::Avoid,
                "None".to_string(),
                0.0,
                format!("Confidence too low ({:.0}%)", prediction.confidence * 100.0),
            )
        } else if edge > MIN_EDGE {
            (
                TradeAction::Buy,
                format!("{} YES", team1),
                edge * 100.0,
                format!(
                    "Predicted {:.1}% vs market {:.1}% = {:.1}% edge",
                    predicted_prob * 100.0,
                    market_prob * 100.0,
                    edge * 100.0
                ),
            )
        } else if edge < -MIN_EDGE {
            (
                TradeAction::Buy,
                format!("{} YES", team2),
                team2_edge * 100.0,
                format!(
                    "{} undervalued: predicted {:.1}% vs market {:.1}%",
                    team2,
                    prediction.team2_win_prob * 100.0,
                    (1.0 - market_prob) * 100.0
                ),
            )
        } else {
            (
                TradeAction::Hold,
                "None".to_string(),
                edge.abs() * 100.0,
                format!("No significant edge detected ({:.1}%)", edge.abs() * 100.0),
            )
        };

        let kelly_fraction = if edge.abs() > MIN_EDGE && prediction.confidence >= MIN_CONFIDENCE {
            (edge.abs() * prediction.confidence).min(0.1)
        } else {
            0.0
        };

        TradeRecommendation {
            action,
            side,
            edge: display_edge,
            suggested_size: Decimal::from_f64_retain(kelly_fraction * 100.0)
                .unwrap_or(Decimal::ZERO),
            reasoning,
        }
    }

    pub async fn analyze_with_draftkings(&self, event_url: &str) -> Result<SportsAnalysisWithDK> {
        use crate::ai_clients::odds_provider::{OddsProvider, Sport};

        let analysis = self.analyze_event(event_url).await?;
        let dk_comparison = match OddsProvider::from_env() {
            Ok(provider) => {
                let sport = match analysis.league.to_uppercase().as_str() {
                    "NBA" => Sport::NBA,
                    "NFL" => Sport::NFL,
                    "NHL" => Sport::NHL,
                    "MLB" => Sport::MLB,
                    _ => Sport::NBA,
                };

                let predicted_home_prob =
                    Decimal::from_f64_retain(analysis.prediction.team1_win_prob)
                        .unwrap_or(Decimal::new(50, 2));

                match provider
                    .compare_with_prediction(
                        sport,
                        &analysis.teams.0,
                        &analysis.teams.1,
                        predicted_home_prob,
                    )
                    .await
                {
                    Ok(Some(edge)) => Some(edge),
                    Ok(None) => {
                        warn!(
                            "No DraftKings odds found for {} vs {}",
                            analysis.teams.0, analysis.teams.1
                        );
                        None
                    }
                    Err(e) => {
                        warn!("Failed to fetch DraftKings odds: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("DraftKings odds provider not configured: {}", e);
                None
            }
        };

        Ok(SportsAnalysisWithDK {
            base: analysis,
            draftkings: dk_comparison,
        })
    }
}

impl SportsAnalysisWithDK {
    pub fn best_edge(&self) -> (String, f64) {
        let pm_edge = self.base.recommendation.edge;

        if let Some(ref dk) = self.draftkings {
            let dk_edge = dk.edge.to_string().parse::<f64>().unwrap_or(0.0) * 100.0;

            if dk_edge.abs() > pm_edge.abs() {
                return (format!("DraftKings - {}", dk.recommended_side), dk_edge);
            }
        }

        (self.base.recommendation.side.clone(), pm_edge)
    }

    pub fn has_arbitrage(&self) -> bool {
        if let Some(ref dk) = self.draftkings {
            let pm_favors_team1 = self.base.recommendation.edge > 0.0;
            let dk_favors_team1 = dk.home_edge > dk.away_edge;

            if pm_favors_team1 != dk_favors_team1 {
                let pm_edge = self.base.recommendation.edge.abs();
                let dk_edge = dk.edge.to_string().parse::<f64>().unwrap_or(0.0).abs() * 100.0;

                return pm_edge > 3.0 && dk_edge > 3.0;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_analyst() -> SportsAnalyst {
        let grok = GrokClient::new(crate::ai_clients::grok::GrokConfig::default()).unwrap();
        let claude = ClaudeAgentClient::new();
        SportsAnalyst::new(grok, claude)
    }

    #[test]
    fn test_parse_prediction_response_extracts_json_payload() {
        let analyst = create_test_analyst();
        let response = r#"Here you go:
```json
{"team1_win_prob":0.56,"team2_win_prob":0.44,"confidence":0.72,"reasoning":"Healthy starters","key_factors":["injuries","pace","market drift"]}
```"#;

        let prediction = analyst
            .parse_prediction_response(response, "A", "B")
            .expect("prediction should parse");

        assert!((prediction.team1_win_prob - 0.56).abs() < f64::EPSILON);
        assert_eq!(prediction.key_factors.len(), 3);
    }

    #[test]
    fn test_parse_prediction_response_falls_back_to_neutral() {
        let analyst = create_test_analyst();
        let prediction = analyst
            .parse_prediction_response("not-json", "Knicks", "Celtics")
            .expect("fallback prediction");

        assert!((prediction.team1_win_prob - 0.5).abs() < f64::EPSILON);
        assert!(prediction.reasoning.contains("Knicks vs Celtics"));
    }
}
