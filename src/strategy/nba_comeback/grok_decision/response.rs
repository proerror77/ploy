//! Response decoding for unified NBA Grok trade decisions.

use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use super::{GrokDecision, grok_intel};

#[derive(Debug, Deserialize)]
struct GrokDecisionJson {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    fair_value: f64,
    #[serde(default)]
    own_fair_value: f64,
    #[serde(default)]
    edge: f64,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    risk_factors: Vec<String>,
}

/// Parse Grok's JSON response into a GrokDecision.
/// Defaults to Pass on any parse failure (safe default: never trade on garbage).
pub fn parse_decision_response(request_id: Uuid, raw: &str) -> GrokDecision {
    let json_str = grok_intel::extract_json_block(raw);

    match serde_json::from_str::<GrokDecisionJson>(&json_str) {
        Ok(parsed) => {
            if parsed.decision.to_ascii_lowercase().trim() == "trade" {
                GrokDecision::Trade {
                    request_id,
                    fair_value: parsed.fair_value.clamp(0.0, 1.0),
                    own_fair_value: parsed.own_fair_value.clamp(0.0, 1.0),
                    edge: parsed.edge,
                    confidence: parsed.confidence.clamp(0.0, 1.0),
                    reasoning: parsed.reasoning,
                    risk_factors: parsed.risk_factors,
                }
            } else {
                GrokDecision::Pass {
                    request_id,
                    reasoning: parsed.reasoning,
                }
            }
        }
        Err(e) => {
            warn!(
                request_id = %request_id,
                error = %e,
                "failed to parse grok decision JSON, defaulting to Pass"
            );
            GrokDecision::Pass {
                request_id,
                reasoning: format!("Parse failure: {}", e),
            }
        }
    }
}
