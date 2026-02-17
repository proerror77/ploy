//! Unified Grok Decision Layer for NBA Sports Trading
//!
//! Instead of two independent order-submission paths (ESPN comeback +
//! Grok signal), ALL final trade decisions are routed through Grok as
//! a unified decision-maker that synthesizes ESPN game state, comeback
//! statistics, Polymarket prices, and X.com sentiment/injury intelligence.
//!
//! Key design choices:
//! - Parse failure → `Pass` (safe default: never trade on garbage).
//! - ESPN comeback path falls back to rule-based when Grok is down
//!   (it has its own statistical model). Grok signal path does NOT.
//! - Decision cooldown (60s) prevents spamming Grok for the same game.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::agent::grok::GrokClient;
use crate::strategy::nba_comeback::espn::LiveGame;
use crate::strategy::nba_comeback::grok_intel::{
    self, GrokGameIntel, GrokSignalType, MomentumDirection,
};

// ── Types ──────────────────────────────────────────────────────

/// What triggered the decision request
#[derive(Debug, Clone, Serialize)]
pub enum DecisionTrigger {
    /// ESPN 30s poll detected a comeback opportunity
    EspnComeback,
    /// ESPN poll detected scaling-in opportunity for existing position
    EspnScaleIn {
        /// Which add this is (1st, 2nd, 3rd, etc.)
        add_number: u32,
        /// Current total shares held
        existing_shares: u64,
        /// Current total cost of position
        existing_cost_usd: f64,
    },
    /// Grok 5-min tick produced a trade signal
    GrokSignal(GrokSignalType),
}

impl std::fmt::Display for DecisionTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EspnComeback => write!(f, "espn_comeback"),
            Self::EspnScaleIn { add_number, .. } => write!(f, "espn_scale_in_{}", add_number),
            Self::GrokSignal(s) => write!(f, "grok_signal_{}", s),
        }
    }
}

/// Snapshot of the statistical comeback model (only for ESPN trigger)
#[derive(Debug, Clone, Serialize)]
pub struct ComebackSnapshot {
    pub comeback_rate: f64,
    pub adjusted_win_prob: f64,
    pub statistical_edge: f64,
}

/// Snapshot of market conditions at decision time
#[derive(Debug, Clone, Serialize)]
pub struct MarketSnapshot {
    pub market_slug: String,
    pub token_id: String,
    pub market_price: Decimal,
    pub yes_best_bid: Option<Decimal>,
    pub yes_best_ask: Option<Decimal>,
}

/// Pre-computed risk metrics for filtering and prompt context
#[derive(Debug, Clone, Serialize)]
pub struct RiskMetrics {
    /// (1 - price) / price — potential gain divided by potential loss
    pub reward_risk_ratio: f64,
    /// fair_value - market_price (positive = underpriced)
    pub expected_value: f64,
    /// Kelly criterion fraction: edge / (1 - price)
    pub kelly_fraction: f64,
}

impl RiskMetrics {
    /// Calculate risk metrics from fair value estimate and market price.
    /// `fair_value` is the estimated win probability (0.0 to 1.0).
    /// `market_price` is the current YES share price (0.0 to 1.0).
    pub fn calculate(fair_value: f64, market_price: f64) -> Self {
        let price = market_price.clamp(0.001, 0.999); // avoid division by zero
        let reward_risk_ratio = (1.0 - price) / price;
        let expected_value = fair_value - price;
        let kelly_fraction = if (1.0 - price).abs() > f64::EPSILON {
            expected_value / (1.0 - price)
        } else {
            0.0
        };

        Self {
            reward_risk_ratio,
            expected_value,
            kelly_fraction: kelly_fraction.max(0.0),
        }
    }

    /// Check if this opportunity passes the minimum reward-to-risk filter
    pub fn passes_filter(&self, min_ratio: f64, min_ev: f64) -> bool {
        self.reward_risk_ratio >= min_ratio && self.expected_value >= min_ev
    }
}

/// Everything Grok needs to make a final trade decision
#[derive(Debug, Clone, Serialize)]
pub struct UnifiedDecisionRequest {
    pub request_id: Uuid,
    pub trigger: DecisionTrigger,
    pub game: LiveGame,
    pub trailing_team: String,
    pub trailing_abbrev: String,
    pub deficit: i32,
    /// Statistical model data (only present for ESPN trigger)
    pub comeback: Option<ComebackSnapshot>,
    /// X.com intelligence (from Grok intel cache, if available)
    pub grok_intel: Option<GrokGameIntel>,
    /// Market data at decision time
    pub market: MarketSnapshot,
    /// Pre-computed risk metrics (reward-to-risk ratio, EV, Kelly)
    pub risk_metrics: RiskMetrics,
}

/// Grok's final decision
#[derive(Debug, Clone, Serialize)]
pub enum GrokDecision {
    Trade {
        request_id: Uuid,
        fair_value: f64,
        /// Grok's own independent win probability estimate (from X.com search)
        own_fair_value: f64,
        edge: f64,
        confidence: f64,
        reasoning: String,
        risk_factors: Vec<String>,
    },
    Pass {
        request_id: Uuid,
        reasoning: String,
    },
}

// ── JSON response parsing ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GrokDecisionJson {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    fair_value: f64,
    /// Grok's own independent probability estimate (may differ from Claude's)
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

// ── Prompt builder ─────────────────────────────────────────────

/// Build the unified decision prompt with ALL available context
pub fn build_unified_prompt(req: &UnifiedDecisionRequest) -> String {
    let mut prompt = format!(
        r#"You are a sports trading analyst. Decide whether to BUY YES shares for the trailing team.

GAME STATE:
- {away} {away_score} vs {home} {home_score} (Q{quarter} {clock})
- Trailing team: {trailing} (down {deficit} pts)
"#,
        away = req.game.away_team,
        home = req.game.home_team,
        away_score = req.game.away_score,
        home_score = req.game.home_score,
        quarter = req.game.quarter,
        clock = req.game.clock,
        trailing = req.trailing_team,
        deficit = req.deficit,
    );

    // Statistical model section (only for ESPN trigger)
    if let Some(ref comeback) = req.comeback {
        prompt.push_str(&format!(
            r#"
STATISTICAL MODEL:
- Historical comeback rate: {:.1}%
- Adjusted win probability: {:.1}%
- Statistical edge vs market: {:.1}%
"#,
            comeback.comeback_rate * 100.0,
            comeback.adjusted_win_prob * 100.0,
            comeback.statistical_edge * 100.0,
        ));
    } else {
        prompt.push_str("\nSTATISTICAL MODEL: Not available for this trigger.\n");
    }

    // X.com intelligence section
    if let Some(ref intel) = req.grok_intel {
        let momentum_str = match intel.momentum_direction {
            MomentumDirection::HomeTeamSurge => "Home team surge",
            MomentumDirection::AwayTeamSurge => "Away team surge",
            MomentumDirection::Neutral => "Neutral",
        };

        let injuries_summary = if intel.injury_updates.is_empty() {
            "None detected".to_string()
        } else {
            intel
                .injury_updates
                .iter()
                .map(|inj| format!("{} ({}) — {}", inj.player_name, inj.team_abbrev, inj.status))
                .collect::<Vec<_>>()
                .join("; ")
        };

        let grok_prob_str = intel
            .grok_home_win_prob
            .map(|p| format!("{:.1}%", p * 100.0))
            .unwrap_or_else(|| "N/A".to_string());

        prompt.push_str(&format!(
            r#"
X.COM INTELLIGENCE:
- Momentum: {momentum} — {narrative}
- Injuries since game start: {injuries}
- Home sentiment: {home_sent:.2}, Away sentiment: {away_sent:.2}
- Grok estimated home win prob: {grok_prob}
- Intel confidence: {confidence:.2}
"#,
            momentum = momentum_str,
            narrative = intel.momentum_narrative,
            injuries = injuries_summary,
            home_sent = intel.home_sentiment_score,
            away_sent = intel.away_sentiment_score,
            grok_prob = grok_prob_str,
            confidence = intel.grok_confidence,
        ));
    } else {
        prompt.push_str("\nX.COM INTELLIGENCE: Not yet available (first poll pending).\n");
    }

    // Market snapshot
    let best_bid_str = req
        .market
        .yes_best_bid
        .map(|d| d.to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let best_ask_str = req
        .market
        .yes_best_ask
        .map(|d| d.to_string())
        .unwrap_or_else(|| "N/A".to_string());

    prompt.push_str(&format!(
        r#"
MARKET:
- Current price for {trailing} YES: ${market_price}
- Best bid: ${best_bid}, Best ask: ${best_ask}

RISK METRICS (pre-computed):
- Reward-to-risk ratio: {rr:.1}x (gain ${gain:.2} / risk ${risk:.2})
- Expected value: {ev:+.1}%
- Kelly fraction: {kelly:.1}%
"#,
        trailing = req.trailing_team,
        market_price = req.market.market_price,
        best_bid = best_bid_str,
        best_ask = best_ask_str,
        rr = req.risk_metrics.reward_risk_ratio,
        gain = 1.0 - req.market.market_price.to_string().parse::<f64>().unwrap_or(0.0),
        risk = req.market.market_price.to_string().parse::<f64>().unwrap_or(0.0),
        ev = req.risk_metrics.expected_value * 100.0,
        kelly = req.risk_metrics.kelly_fraction * 100.0,
    ));

    // Position context for scale-in decisions
    if let DecisionTrigger::EspnScaleIn {
        add_number,
        existing_shares,
        existing_cost_usd,
    } = &req.trigger
    {
        prompt.push_str(&format!(
            r#"EXISTING POSITION (scale-in #{add_number}):
- Already holding {existing_shares} shares (cost: ${existing_cost:.2})
- This would be add #{add_number} to the position
- Consider whether adding increases or concentrates risk

"#,
            add_number = add_number,
            existing_shares = existing_shares,
            existing_cost = existing_cost_usd,
        ));
    }

    prompt.push_str(&format!(
        r#"TRIGGER: {trigger}

Decide: should we BUY YES shares on {trailing} winning?

IMPORTANT: Also provide your OWN independent win probability estimate (own_fair_value)
based on your X.com search. If it disagrees with the statistical model by >5%, explain why.

Respond ONLY in JSON:
{{
  "decision": "trade" or "pass",
  "fair_value": 0.0-1.0 (statistical model estimate),
  "own_fair_value": 0.0-1.0 (YOUR independent estimate from X.com intel),
  "edge": fair_value minus market_price,
  "confidence": 0.0-1.0,
  "reasoning": "2-3 sentences",
  "risk_factors": ["factor1", "factor2"]
}}"#,
        trailing = req.trailing_team,
        trigger = req.trigger,
    ));

    prompt
}

// ── Response parser ────────────────────────────────────────────

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

// ── Query helper ───────────────────────────────────────────────

/// Query Grok for a unified trade decision and parse the response.
/// Returns the prompt and raw response alongside the decision for persistence.
pub async fn request_unified_decision(
    grok: &GrokClient,
    req: &UnifiedDecisionRequest,
) -> std::result::Result<(GrokDecision, String, String), String> {
    let prompt = build_unified_prompt(req);
    let start = std::time::Instant::now();

    match grok.chat(&prompt).await {
        Ok(raw) => {
            let duration_ms = start.elapsed().as_millis() as u32;
            debug!(
                request_id = %req.request_id,
                game_id = %req.game.espn_game_id,
                trigger = %req.trigger,
                duration_ms,
                response_len = raw.len(),
                "grok unified decision query completed"
            );
            let decision = parse_decision_response(req.request_id, &raw);
            Ok((decision, prompt, raw))
        }
        Err(e) => {
            warn!(
                request_id = %req.request_id,
                game_id = %req.game.espn_game_id,
                error = %e,
                "grok unified decision query failed"
            );
            Err(format!("Grok decision query failed: {}", e))
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::nba_comeback::espn::GameStatus;
    use rust_decimal_macros::dec;

    fn sample_game() -> LiveGame {
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

    fn sample_request() -> UnifiedDecisionRequest {
        UnifiedDecisionRequest {
            request_id: Uuid::nil(),
            trigger: DecisionTrigger::EspnComeback,
            game: sample_game(),
            trailing_team: "Los Angeles Lakers".to_string(),
            trailing_abbrev: "LAL".to_string(),
            deficit: 13,
            comeback: Some(ComebackSnapshot {
                comeback_rate: 0.22,
                adjusted_win_prob: 0.35,
                statistical_edge: 0.10,
            }),
            grok_intel: None,
            market: MarketSnapshot {
                market_slug: "nba-lal-vs-bos".to_string(),
                token_id: "lal-win-yes".to_string(),
                market_price: dec!(0.25),
                yes_best_bid: Some(dec!(0.24)),
                yes_best_ask: Some(dec!(0.26)),
            },
            risk_metrics: RiskMetrics::calculate(0.35, 0.25),
        }
    }

    #[test]
    fn test_prompt_contains_all_sections() {
        let req = sample_request();
        let prompt = build_unified_prompt(&req);

        // Game state
        assert!(prompt.contains("Boston Celtics"));
        assert!(prompt.contains("Los Angeles Lakers"));
        assert!(prompt.contains("Q3"));
        assert!(prompt.contains("4:30"));
        assert!(prompt.contains("down 13 pts"));

        // Statistical model
        assert!(prompt.contains("Historical comeback rate: 22.0%"));
        assert!(prompt.contains("Adjusted win probability: 35.0%"));
        assert!(prompt.contains("Statistical edge vs market: 10.0%"));

        // Market
        assert!(prompt.contains("0.25"));
        assert!(prompt.contains("0.24"));
        assert!(prompt.contains("0.26"));

        // Risk metrics
        assert!(prompt.contains("Reward-to-risk ratio: 3.0x"));
        assert!(prompt.contains("Expected value: +10.0%"));
        assert!(prompt.contains("own_fair_value"));

        // Trigger
        assert!(prompt.contains("espn_comeback"));
    }

    #[test]
    fn test_prompt_without_comeback_data() {
        let mut req = sample_request();
        req.comeback = None;
        req.trigger = DecisionTrigger::GrokSignal(GrokSignalType::FairValueEdge);
        let prompt = build_unified_prompt(&req);

        assert!(prompt.contains("Not available for this trigger"));
        assert!(prompt.contains("grok_signal_fair_value_edge"));
    }

    #[test]
    fn test_prompt_with_grok_intel() {
        use crate::strategy::nba_comeback::grok_intel::InjuryUpdate;
        use chrono::Utc;

        let mut req = sample_request();
        req.grok_intel = Some(GrokGameIntel {
            game_id: "401584701".to_string(),
            queried_at: Utc::now(),
            injury_updates: vec![InjuryUpdate {
                player_name: "Jayson Tatum".to_string(),
                team_abbrev: "BOS".to_string(),
                status: "OUT".to_string(),
                impact: grok_intel::InjuryImpact::High,
                details: "ankle sprain".to_string(),
            }],
            momentum_narrative: "Lakers on a 12-0 run".to_string(),
            momentum_direction: MomentumDirection::AwayTeamSurge,
            home_sentiment_score: -0.3,
            away_sentiment_score: 0.7,
            grok_home_win_prob: Some(0.45),
            grok_confidence: 0.8,
            key_factors: vec!["Tatum injury".to_string()],
            raw_response: String::new(),
        });

        let prompt = build_unified_prompt(&req);
        assert!(prompt.contains("Jayson Tatum"));
        assert!(prompt.contains("Away team surge"));
        assert!(prompt.contains("Lakers on a 12-0 run"));
        assert!(prompt.contains("45.0%"));
    }

    #[test]
    fn test_parse_trade_decision() {
        let raw = r#"```json
{
  "decision": "trade",
  "fair_value": 0.38,
  "own_fair_value": 0.40,
  "edge": 0.13,
  "confidence": 0.75,
  "reasoning": "Statistical model shows edge with momentum confirmation.",
  "risk_factors": ["low liquidity", "key player minutes"]
}
```"#;

        let decision = parse_decision_response(Uuid::nil(), raw);
        match decision {
            GrokDecision::Trade {
                fair_value,
                own_fair_value,
                edge,
                confidence,
                risk_factors,
                ..
            } => {
                assert!((fair_value - 0.38).abs() < f64::EPSILON);
                assert!((own_fair_value - 0.40).abs() < f64::EPSILON);
                assert!((edge - 0.13).abs() < f64::EPSILON);
                assert!((confidence - 0.75).abs() < f64::EPSILON);
                assert_eq!(risk_factors.len(), 2);
            }
            GrokDecision::Pass { .. } => panic!("expected Trade, got Pass"),
        }
    }

    #[test]
    fn test_parse_pass_decision() {
        let raw = r#"{
  "decision": "pass",
  "fair_value": 0.28,
  "edge": 0.03,
  "confidence": 0.40,
  "reasoning": "Edge too thin given current uncertainty.",
  "risk_factors": ["uncertain momentum"]
}"#;

        let decision = parse_decision_response(Uuid::nil(), raw);
        match decision {
            GrokDecision::Pass { reasoning, .. } => {
                assert!(reasoning.contains("too thin"));
            }
            GrokDecision::Trade { .. } => panic!("expected Pass, got Trade"),
        }
    }

    #[test]
    fn test_parse_malformed_defaults_to_pass() {
        let raw = "I'm not sure about this game, the data looks unclear.";
        let decision = parse_decision_response(Uuid::nil(), raw);
        match decision {
            GrokDecision::Pass { reasoning, .. } => {
                assert!(reasoning.contains("Parse failure"));
            }
            GrokDecision::Trade { .. } => panic!("expected Pass on malformed input, got Trade"),
        }
    }

    #[test]
    fn test_fair_value_clamped() {
        let raw = r#"{"decision": "trade", "fair_value": 1.5, "own_fair_value": 1.8, "edge": 0.5, "confidence": 2.0, "reasoning": "test", "risk_factors": []}"#;
        let decision = parse_decision_response(Uuid::nil(), raw);
        match decision {
            GrokDecision::Trade {
                fair_value,
                own_fair_value,
                confidence,
                ..
            } => {
                assert!(fair_value <= 1.0, "fair_value should be clamped to 1.0");
                assert!(own_fair_value <= 1.0, "own_fair_value should be clamped to 1.0");
                assert!(confidence <= 1.0, "confidence should be clamped to 1.0");
            }
            GrokDecision::Pass { .. } => panic!("expected Trade"),
        }
    }

    #[test]
    fn test_risk_metrics_calculation() {
        // Price $0.20: gain $0.80, risk $0.20 → ratio 4.0x
        let m = RiskMetrics::calculate(0.35, 0.20);
        assert!((m.reward_risk_ratio - 4.0).abs() < 0.01);
        assert!((m.expected_value - 0.15).abs() < 0.01);
        assert!(m.passes_filter(4.0, 0.05));

        // Price $0.25: ratio 3.0x → fails 4.0x filter
        let m2 = RiskMetrics::calculate(0.35, 0.25);
        assert!((m2.reward_risk_ratio - 3.0).abs() < 0.01);
        assert!(!m2.passes_filter(4.0, 0.05));
        assert!(m2.passes_filter(3.0, 0.05)); // would pass at 3.0x

        // Price $0.10: ratio 9.0x → easily passes
        let m3 = RiskMetrics::calculate(0.20, 0.10);
        assert!((m3.reward_risk_ratio - 9.0).abs() < 0.01);
        assert!(m3.passes_filter(4.0, 0.05));
    }

    #[test]
    fn test_risk_metrics_edge_cases() {
        // Very low price → clamped to avoid div-by-zero
        let m = RiskMetrics::calculate(0.5, 0.0);
        assert!(m.reward_risk_ratio > 100.0); // 0.999/0.001

        // Negative expected value → kelly is 0
        let m2 = RiskMetrics::calculate(0.10, 0.30);
        assert!(m2.expected_value < 0.0);
        assert!((m2.kelly_fraction - 0.0).abs() < f64::EPSILON);
        assert!(!m2.passes_filter(4.0, 0.05));
    }
}
