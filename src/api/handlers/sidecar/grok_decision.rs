use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::{auth::ensure_sidecar_authorized, state::AppState};
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};
use crate::strategy::nba_comeback::grok_decision::{
    build_unified_prompt, parse_decision_response, ComebackSnapshot, DecisionTrigger, GrokDecision,
    MarketSnapshot, RiskMetrics, UnifiedDecisionRequest,
};
use crate::strategy::nba_comeback::grok_intel::{
    GrokGameIntel, InjuryImpact, InjuryUpdate, MomentumDirection,
};

/// Accepts injury_updates as either a Vec<SidecarInjuryUpdate> or a plain String.
/// The TypeScript sidecar may send it as a string (from WebSearch), while
/// direct API callers may send structured data.
fn deserialize_injury_updates<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<SidecarInjuryUpdate>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum InjuryInput {
        Structured(Vec<SidecarInjuryUpdate>),
        Plain(String),
        Null,
    }

    match Option::<InjuryInput>::deserialize(deserializer)? {
        Some(InjuryInput::Structured(v)) => Ok(Some(v)),
        Some(InjuryInput::Plain(s)) if s.is_empty() => Ok(None),
        Some(InjuryInput::Plain(s)) => {
            if let Ok(v) = serde_json::from_str::<Vec<SidecarInjuryUpdate>>(&s) {
                return Ok(Some(v));
            }
            Ok(Some(vec![SidecarInjuryUpdate {
                player_name: "See details".to_string(),
                team_abbrev: "N/A".to_string(),
                status: "reported".to_string(),
                impact: Some("medium".to_string()),
                details: Some(s),
            }]))
        }
        Some(InjuryInput::Null) | None => Ok(None),
    }
}

/// POST /api/sidecar/grok/decision — request body from TypeScript sidecar
#[derive(Debug, Deserialize)]
pub struct GrokDecisionRequest {
    // Game state (from ESPN MCP tool)
    pub game_id: String,
    pub home_team: String,
    pub away_team: String,
    pub home_abbrev: Option<String>,
    pub away_abbrev: Option<String>,
    pub trailing_team: String,
    pub trailing_abbrev: String,
    pub home_score: i32,
    pub away_score: i32,
    pub quarter: u8,
    pub clock: String,
    pub deficit: i32,
    // Market data (from Polymarket MCP tool)
    pub market_slug: String,
    pub token_id: Option<String>,
    pub market_price: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    // Statistical model (optional — may not be available from sidecar)
    pub comeback_rate: Option<f64>,
    pub adjusted_win_prob: Option<f64>,
    pub statistical_edge: Option<f64>,
    // X.com intelligence (from sidecar's WebSearch research)
    #[serde(alias = "sentiment_home")]
    pub x_sentiment_home: Option<f64>,
    #[serde(alias = "sentiment_away")]
    pub x_sentiment_away: Option<f64>,
    pub momentum_direction: Option<String>, // "home_surge" | "away_surge" | "neutral"
    pub momentum_narrative: Option<String>,
    /// Accepts either structured Vec or a JSON string (from TS sidecar)
    #[serde(default, deserialize_with = "deserialize_injury_updates")]
    pub injury_updates: Option<Vec<SidecarInjuryUpdate>>,
    pub research_summary: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarInjuryUpdate {
    pub player_name: String,
    pub team_abbrev: String,
    pub status: String,
    pub impact: Option<String>, // "high" | "medium" | "low"
    pub details: Option<String>,
}

/// POST /api/sidecar/grok/decision — response
#[derive(Debug, Serialize)]
pub struct GrokDecisionResponse {
    pub request_id: String,
    pub decision: String, // "trade" | "pass"
    pub fair_value: Option<f64>,
    pub own_fair_value: Option<f64>,
    pub edge: Option<f64>,
    pub confidence: Option<f64>,
    pub reasoning: String,
    pub risk_factors: Vec<String>,
    pub query_duration_ms: u32,
}

/// POST /api/sidecar/grok/decision
///
/// The sidecar sends all research data (game state, market, X.com intel).
/// We construct a UnifiedDecisionRequest, query Grok, and return the decision.
pub async fn sidecar_grok_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GrokDecisionRequest>,
) -> std::result::Result<Json<GrokDecisionResponse>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;

    let grok = state.grok_client.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Grok client not configured (GROK_API_KEY missing)".to_string(),
        )
    })?;

    let request_id = Uuid::new_v4();
    let start = std::time::Instant::now();
    let unified_req = build_unified_decision_request(request_id, req);

    let prompt = build_unified_prompt(&unified_req);
    let grok_result = grok.chat(&prompt).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match grok_result {
        Ok(raw_response) => {
            let decision = parse_decision_response(request_id, &raw_response);

            info!(
                request_id = %request_id,
                game_id = %unified_req.game.espn_game_id,
                decision = match &decision {
                    GrokDecision::Trade { .. } => "trade",
                    GrokDecision::Pass { .. } => "pass",
                },
                duration_ms,
                "sidecar grok decision completed"
            );

            let _ = persist_sidecar_decision(
                state.store.pool(),
                &request_id,
                &unified_req,
                &decision,
                &prompt,
                &raw_response,
                duration_ms,
            )
            .await;

            Ok(Json(build_grok_decision_response(
                request_id,
                decision,
                duration_ms,
            )))
        }
        Err(e) => {
            warn!(request_id = %request_id, error = %e, "sidecar grok decision failed");
            Err((StatusCode::BAD_GATEWAY, format!("Grok query failed: {}", e)))
        }
    }
}

fn build_unified_decision_request(
    request_id: Uuid,
    req: GrokDecisionRequest,
) -> UnifiedDecisionRequest {
    let fair_value_estimate = req.adjusted_win_prob.unwrap_or(req.market_price);
    let game = build_live_game(&req);
    let comeback = build_comeback_snapshot(&req);
    let grok_intel = build_grok_intel(&req);
    let market = build_market_snapshot(&req);
    let risk_metrics = RiskMetrics::calculate(fair_value_estimate, req.market_price);

    UnifiedDecisionRequest {
        request_id,
        trigger: DecisionTrigger::EspnComeback,
        game,
        trailing_team: req.trailing_team,
        trailing_abbrev: req.trailing_abbrev,
        deficit: req.deficit,
        comeback,
        grok_intel,
        market,
        risk_metrics,
    }
}

fn build_live_game(req: &GrokDecisionRequest) -> LiveGame {
    LiveGame {
        espn_game_id: req.game_id.clone(),
        home_team: req.home_team.clone(),
        away_team: req.away_team.clone(),
        home_abbrev: req.home_abbrev.clone().unwrap_or_default(),
        away_abbrev: req.away_abbrev.clone().unwrap_or_default(),
        home_score: req.home_score,
        away_score: req.away_score,
        quarter: req.quarter,
        clock: req.clock.clone(),
        time_remaining_mins: 0.0,
        status: GameStatus::InProgress,
        home_quarter_scores: Vec::new(),
        away_quarter_scores: Vec::new(),
    }
}

fn build_comeback_snapshot(req: &GrokDecisionRequest) -> Option<ComebackSnapshot> {
    match (
        req.comeback_rate,
        req.adjusted_win_prob,
        req.statistical_edge,
    ) {
        (Some(rate), Some(prob), Some(edge)) => Some(ComebackSnapshot {
            comeback_rate: rate,
            adjusted_win_prob: prob,
            statistical_edge: edge,
        }),
        _ => None,
    }
}

fn build_grok_intel(req: &GrokDecisionRequest) -> Option<GrokGameIntel> {
    if req.x_sentiment_home.is_none() && req.momentum_narrative.is_none() {
        return None;
    }

    let momentum_dir = match req.momentum_direction.as_deref() {
        Some("home_surge") => MomentumDirection::HomeTeamSurge,
        Some("away_surge") => MomentumDirection::AwayTeamSurge,
        _ => MomentumDirection::Neutral,
    };

    let injuries = req
        .injury_updates
        .as_ref()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|inj| InjuryUpdate {
            player_name: inj.player_name,
            team_abbrev: inj.team_abbrev,
            status: inj.status,
            impact: match inj.impact.as_deref() {
                Some("high") => InjuryImpact::High,
                Some("medium") => InjuryImpact::Medium,
                _ => InjuryImpact::Low,
            },
            details: inj.details.unwrap_or_default(),
        })
        .collect();

    Some(GrokGameIntel {
        game_id: req.game_id.clone(),
        queried_at: Utc::now(),
        injury_updates: injuries,
        momentum_narrative: req.momentum_narrative.clone().unwrap_or_default(),
        momentum_direction: momentum_dir,
        home_sentiment_score: req.x_sentiment_home.unwrap_or(0.0),
        away_sentiment_score: req.x_sentiment_away.unwrap_or(0.0),
        grok_home_win_prob: None,
        grok_confidence: 0.5,
        key_factors: Vec::new(),
        raw_response: req.research_summary.clone().unwrap_or_default(),
    })
}

fn build_market_snapshot(req: &GrokDecisionRequest) -> MarketSnapshot {
    let market_price =
        Decimal::from_str(&format!("{:.4}", req.market_price)).unwrap_or_else(|_| Decimal::ZERO);

    MarketSnapshot {
        market_slug: req.market_slug.clone(),
        token_id: req.token_id.clone().unwrap_or_default(),
        market_price,
        yes_best_bid: req
            .best_bid
            .and_then(|b| Decimal::from_str(&format!("{:.4}", b)).ok()),
        yes_best_ask: req
            .best_ask
            .and_then(|a| Decimal::from_str(&format!("{:.4}", a)).ok()),
    }
}

fn build_grok_decision_response(
    request_id: Uuid,
    decision: GrokDecision,
    duration_ms: u32,
) -> GrokDecisionResponse {
    match decision {
        GrokDecision::Trade {
            fair_value,
            own_fair_value,
            edge,
            confidence,
            reasoning,
            risk_factors,
            ..
        } => GrokDecisionResponse {
            request_id: request_id.to_string(),
            decision: "trade".to_string(),
            fair_value: Some(fair_value),
            own_fair_value: Some(own_fair_value),
            edge: Some(edge),
            confidence: Some(confidence),
            reasoning,
            risk_factors,
            query_duration_ms: duration_ms,
        },
        GrokDecision::Pass { reasoning, .. } => GrokDecisionResponse {
            request_id: request_id.to_string(),
            decision: "pass".to_string(),
            fair_value: None,
            own_fair_value: None,
            edge: None,
            confidence: None,
            reasoning,
            risk_factors: Vec::new(),
            query_duration_ms: duration_ms,
        },
    }
}

/// Persist a sidecar-originated Grok decision to the database for audit trail.
async fn persist_sidecar_decision(
    pool: &sqlx::PgPool,
    request_id: &Uuid,
    req: &UnifiedDecisionRequest,
    decision: &GrokDecision,
    prompt: &str,
    raw_response: &str,
    duration_ms: u32,
) {
    let (decision_str, fair_value, edge, confidence, reasoning, risk_factors) = match decision {
        GrokDecision::Trade {
            fair_value,
            edge,
            confidence,
            reasoning,
            risk_factors,
            ..
        } => (
            "trade",
            Some(*fair_value),
            Some(*edge),
            Some(*confidence),
            reasoning.as_str(),
            Some(serde_json::to_value(risk_factors).unwrap_or_default()),
        ),
        GrokDecision::Pass { reasoning, .. } => {
            ("pass", None, None, None, reasoning.as_str(), None)
        }
    };

    let result = sqlx::query(
        r#"
        INSERT INTO grok_unified_decisions (
            request_id, account_id, agent_id,
            espn_game_id, home_team, away_team,
            trailing_team, trailing_abbrev,
            deficit, quarter, clock, score,
            trigger_type,
            comeback_rate, adjusted_win_prob, statistical_edge,
            market_slug, token_id, market_price,
            best_bid, best_ask,
            decision, decision_fair_value, decision_edge,
            decision_confidence, decision_reasoning, decision_risk_factors,
            raw_prompt, raw_response, query_duration_ms,
            order_submitted
        ) VALUES (
            $1, 'sidecar', 'sidecar',
            $2, $3, $4,
            $5, $6,
            $7, $8, $9, $10,
            $11,
            $12, $13, $14,
            $15, $16, $17,
            $18, $19,
            $20, $21, $22,
            $23, $24, $25,
            $26, $27, $28,
            FALSE
        )
        "#,
    )
    .bind(request_id)
    .bind(&req.game.espn_game_id)
    .bind(&req.game.home_team)
    .bind(&req.game.away_team)
    .bind(&req.trailing_team)
    .bind(&req.trailing_abbrev)
    .bind(req.deficit)
    .bind(req.game.quarter as i32)
    .bind(&req.game.clock)
    .bind(format!(
        "{} {} - {} {}",
        req.game.away_team, req.game.away_score, req.game.home_team, req.game.home_score
    ))
    .bind(format!("{}", req.trigger))
    .bind(req.comeback.as_ref().map(|c| c.comeback_rate))
    .bind(req.comeback.as_ref().map(|c| c.adjusted_win_prob))
    .bind(req.comeback.as_ref().map(|c| c.statistical_edge))
    .bind(&req.market.market_slug)
    .bind(&req.market.token_id)
    .bind(
        req.market
            .market_price
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0),
    )
    .bind(
        req.market
            .yes_best_bid
            .map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)),
    )
    .bind(
        req.market
            .yes_best_ask
            .map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)),
    )
    .bind(decision_str)
    .bind(fair_value)
    .bind(edge)
    .bind(confidence)
    .bind(reasoning)
    .bind(risk_factors)
    .bind(prompt)
    .bind(raw_response)
    .bind(duration_ms as i32)
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(error = %e, "failed to persist sidecar grok decision (non-fatal)");
    }
}
