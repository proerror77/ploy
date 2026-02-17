//! Sidecar REST endpoints — bridge between Claude Agent SDK and Rust trading core
//!
//! These endpoints are called by the TypeScript sidecar (ploy-sidecar) which uses
//! Claude Agent SDK + MCP tools for research, then routes order decisions through
//! Grok and the Coordinator.
//!
//! Endpoints:
//! - POST /api/sidecar/grok/decision — Unified Grok decision with full context
//! - POST /api/sidecar/orders       — Submit order through Coordinator
//! - GET  /api/sidecar/positions     — Current positions from DB
//! - GET  /api/sidecar/risk          — Risk state from Coordinator

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::state::AppState;
use crate::domain::market::Side;
use crate::platform::{Domain, OrderIntent, OrderPriority};
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};
use crate::strategy::nba_comeback::grok_decision::{
    build_unified_prompt, parse_decision_response, ComebackSnapshot, DecisionTrigger,
    GrokDecision, MarketSnapshot, RiskMetrics, UnifiedDecisionRequest,
};
use crate::strategy::nba_comeback::grok_intel::{
    GrokGameIntel, InjuryImpact, InjuryUpdate, MomentumDirection,
};

// ── Custom deserializer ──────────────────────────────────────────

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
            // Try to parse the string as JSON first
            if let Ok(v) = serde_json::from_str::<Vec<SidecarInjuryUpdate>>(&s) {
                return Ok(Some(v));
            }
            // Otherwise store as a single "text" injury update for the prompt
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

// ── Request / Response types ─────────────────────────────────────

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

#[derive(Debug, Deserialize)]
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

/// POST /api/sidecar/orders — request body
#[derive(Debug, Deserialize)]
pub struct SidecarOrderRequest {
    pub strategy: String,
    pub market_slug: String,
    pub token_id: String,
    pub side: Option<String>,       // "up"/"down" or "YES"/"NO"
    pub is_buy: Option<bool>,       // defaults to true
    pub shares: u64,
    pub price: f64,
    pub dry_run: Option<bool>,
    #[serde(alias = "grok_decision_id")]
    pub decision_request_id: Option<String>,
    #[serde(alias = "reasoning")]
    pub decision_reasoning: Option<String>,
    /// Extra metadata fields from sidecar (edge, confidence, etc.)
    pub edge: Option<f64>,
    pub confidence: Option<f64>,
}

/// POST /api/sidecar/orders — response
#[derive(Debug, Serialize)]
pub struct SidecarOrderResponse {
    pub success: bool,
    pub intent_id: Option<String>,
    pub message: String,
    pub dry_run: bool,
}

/// GET /api/sidecar/positions — response item
#[derive(Debug, Serialize)]
pub struct SidecarPosition {
    pub id: i64,
    pub market_slug: String,
    pub token_id: String,
    pub side: String,
    pub shares: i64,
    pub avg_price: f64,
    pub current_value: Option<f64>,
    pub pnl: Option<f64>,
    pub status: String,
    pub opened_at: String,
}

/// GET /api/sidecar/risk — response
#[derive(Debug, Serialize)]
pub struct SidecarRiskState {
    pub coordinator_running: bool,
    pub agents: Vec<SidecarAgentInfo>,
    pub total_exposure_usd: f64,
    pub queue_depth: usize,
}

#[derive(Debug, Serialize)]
pub struct SidecarAgentInfo {
    pub agent_id: String,
    pub domain: String,
    pub status: String,
    pub last_heartbeat: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────────

/// POST /api/sidecar/grok/decision
///
/// The sidecar sends all research data (game state, market, X.com intel).
/// We construct a UnifiedDecisionRequest, query Grok, and return the decision.
pub async fn sidecar_grok_decision(
    State(state): State<AppState>,
    Json(req): Json<GrokDecisionRequest>,
) -> std::result::Result<Json<GrokDecisionResponse>, (StatusCode, String)> {
    let grok = state.grok_client.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Grok client not configured (GROK_API_KEY missing)".to_string(),
        )
    })?;

    let request_id = Uuid::new_v4();
    let start = std::time::Instant::now();

    // Build LiveGame from sidecar data
    let game = LiveGame {
        espn_game_id: req.game_id.clone(),
        home_team: req.home_team.clone(),
        away_team: req.away_team.clone(),
        home_abbrev: req.home_abbrev.clone().unwrap_or_default(),
        away_abbrev: req.away_abbrev.clone().unwrap_or_default(),
        home_score: req.home_score,
        away_score: req.away_score,
        quarter: req.quarter,
        clock: req.clock.clone(),
        time_remaining_mins: 0.0, // not critical for prompt
        status: GameStatus::InProgress,
        home_quarter_scores: Vec::new(),
        away_quarter_scores: Vec::new(),
    };

    // Build comeback snapshot (if sidecar provided stats)
    let comeback = match (req.comeback_rate, req.adjusted_win_prob, req.statistical_edge) {
        (Some(rate), Some(prob), Some(edge)) => Some(ComebackSnapshot {
            comeback_rate: rate,
            adjusted_win_prob: prob,
            statistical_edge: edge,
        }),
        _ => None,
    };

    // Build Grok intel from X.com research (if sidecar provided it)
    let grok_intel = if req.x_sentiment_home.is_some() || req.momentum_narrative.is_some() {
        let momentum_dir = match req.momentum_direction.as_deref() {
            Some("home_surge") => MomentumDirection::HomeTeamSurge,
            Some("away_surge") => MomentumDirection::AwayTeamSurge,
            _ => MomentumDirection::Neutral,
        };

        let injuries = req
            .injury_updates
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
            momentum_narrative: req.momentum_narrative.unwrap_or_default(),
            momentum_direction: momentum_dir,
            home_sentiment_score: req.x_sentiment_home.unwrap_or(0.0),
            away_sentiment_score: req.x_sentiment_away.unwrap_or(0.0),
            grok_home_win_prob: None, // sidecar doesn't have this pre-computed
            grok_confidence: 0.5,     // default
            key_factors: Vec::new(),
            raw_response: req.research_summary.unwrap_or_default(),
        })
    } else {
        None
    };

    // Market snapshot
    let market_price = Decimal::from_str(&format!("{:.4}", req.market_price))
        .unwrap_or_else(|_| Decimal::ZERO);
    let market = MarketSnapshot {
        market_slug: req.market_slug.clone(),
        token_id: req.token_id.clone().unwrap_or_default(),
        market_price,
        yes_best_bid: req
            .best_bid
            .and_then(|b| Decimal::from_str(&format!("{:.4}", b)).ok()),
        yes_best_ask: req
            .best_ask
            .and_then(|a| Decimal::from_str(&format!("{:.4}", a)).ok()),
    };

    // Compute risk metrics
    let fair_value_estimate = req.adjusted_win_prob.unwrap_or(req.market_price);
    let risk_metrics = RiskMetrics::calculate(fair_value_estimate, req.market_price);

    // Build unified decision request
    let unified_req = UnifiedDecisionRequest {
        request_id,
        trigger: DecisionTrigger::EspnComeback, // sidecar triggers treated as ESPN comeback
        game,
        trailing_team: req.trailing_team,
        trailing_abbrev: req.trailing_abbrev,
        deficit: req.deficit,
        comeback,
        grok_intel,
        market,
        risk_metrics,
    };

    // Query Grok
    let prompt = build_unified_prompt(&unified_req);
    let grok_result = grok.chat(&prompt).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match grok_result {
        Ok(raw_response) => {
            let decision = parse_decision_response(request_id, &raw_response);

            info!(
                request_id = %request_id,
                game_id = %req.game_id,
                decision = match &decision {
                    GrokDecision::Trade { .. } => "trade",
                    GrokDecision::Pass { .. } => "pass",
                },
                duration_ms,
                "sidecar grok decision completed"
            );

            // Persist to grok_unified_decisions table
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

            let response = match decision {
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
            };

            Ok(Json(response))
        }
        Err(e) => {
            warn!(request_id = %request_id, error = %e, "sidecar grok decision failed");
            Err((
                StatusCode::BAD_GATEWAY,
                format!("Grok query failed: {}", e),
            ))
        }
    }
}

/// POST /api/sidecar/orders
///
/// Submit an order through the Coordinator pipeline (risk gate → queue → execution).
pub async fn sidecar_submit_order(
    State(state): State<AppState>,
    Json(req): Json<SidecarOrderRequest>,
) -> std::result::Result<Json<SidecarOrderResponse>, (StatusCode, String)> {
    let dry_run = req.dry_run.unwrap_or(true);

    // Validate price range
    let price = Decimal::from_str(&format!("{:.4}", req.price)).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid price format".to_string(),
        )
    })?;

    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Err((
            StatusCode::BAD_REQUEST,
            "Price must be between 0 and 1 (exclusive)".to_string(),
        ));
    }

    // Max order size check ($50)
    let order_cost = req.shares as f64 * req.price;
    if order_cost > 50.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Order cost ${:.2} exceeds sidecar limit $50",
                order_cost
            ),
        ));
    }

    if dry_run {
        info!(
            market = %req.market_slug,
            shares = req.shares,
            price = %price,
            strategy = %req.strategy,
            "sidecar dry-run order (not submitted)"
        );

        // Log to audit for observability
        let _ = sqlx::query(
            r#"
            INSERT INTO security_audit_log (event_type, severity, details, metadata)
            VALUES ('SIDECAR_DRY_RUN', 'LOW', $1, $2)
            "#,
        )
        .bind(format!(
            "Sidecar dry-run: {} shares @ {} on {}",
            req.shares, price, req.market_slug
        ))
        .bind(serde_json::json!({
            "strategy": req.strategy,
            "market_slug": req.market_slug,
            "shares": req.shares,
            "price": req.price,
            "decision_request_id": req.decision_request_id,
        }))
        .execute(state.store.pool())
        .await;

        return Ok(Json(SidecarOrderResponse {
            success: true,
            intent_id: None,
            message: format!(
                "Dry-run: would buy {} shares @ ${} on {}",
                req.shares, price, req.market_slug
            ),
            dry_run: true,
        }));
    }

    // Live order — requires Coordinator
    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;

    let side = match req.side.as_deref() {
        Some("down") | Some("NO") | Some("no") => Side::Down,
        _ => Side::Up, // "up", "YES", "yes", or default
    };

    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "sidecar".to_string());
    metadata.insert("strategy".to_string(), req.strategy.clone());
    if let Some(ref dec_id) = req.decision_request_id {
        metadata.insert("decision_request_id".to_string(), dec_id.clone());
    }
    if let Some(ref reasoning) = req.decision_reasoning {
        metadata.insert("decision_reasoning".to_string(), reasoning.clone());
    }
    if let Some(edge) = req.edge {
        metadata.insert("edge".to_string(), format!("{:.4}", edge));
    }
    if let Some(conf) = req.confidence {
        metadata.insert("confidence".to_string(), format!("{:.2}", conf));
    }

    let mut intent = OrderIntent::new(
        "sidecar",
        Domain::Sports,
        &req.market_slug,
        &req.token_id,
        side,
        req.is_buy.unwrap_or(true),
        req.shares,
        price,
    );
    intent.priority = OrderPriority::Normal;
    intent.metadata = metadata;

    let intent_id = intent.intent_id.to_string();

    coordinator.submit_order(intent).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to submit order: {}", e),
        )
    })?;

    info!(
        intent_id = %intent_id,
        market = %req.market_slug,
        shares = req.shares,
        price = %price,
        "sidecar order submitted to coordinator"
    );

    Ok(Json(SidecarOrderResponse {
        success: true,
        intent_id: Some(intent_id),
        message: "Order submitted to coordinator pipeline".to_string(),
        dry_run: false,
    }))
}

/// GET /api/sidecar/positions
///
/// Returns current open positions from the database.
pub async fn sidecar_get_positions(
    State(state): State<AppState>,
) -> std::result::Result<Json<Vec<SidecarPosition>>, (StatusCode, String)> {
    let rows = sqlx::query_as::<_, (i64, String, String, String, i64, f64, Option<f64>, Option<f64>, String, chrono::DateTime<Utc>)>(
        r#"
        SELECT
            id,
            COALESCE(market_slug, '') as market_slug,
            COALESCE(token_id, '') as token_id,
            COALESCE(side, 'up') as side,
            COALESCE(shares, 0) as shares,
            COALESCE(avg_price, 0.0) as avg_price,
            current_value,
            pnl,
            COALESCE(status, 'open') as status,
            COALESCE(opened_at, NOW()) as opened_at
        FROM positions
        WHERE status = 'open'
        ORDER BY opened_at DESC
        LIMIT 100
        "#,
    )
    .fetch_all(state.store.pool())
    .await
    .map_err(|e| {
        warn!(error = %e, "failed to fetch positions for sidecar");
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e))
    })?;

    let positions: Vec<SidecarPosition> = rows
        .into_iter()
        .map(|(id, market_slug, token_id, side, shares, avg_price, current_value, pnl, status, opened_at)| {
            SidecarPosition {
                id,
                market_slug,
                token_id,
                side,
                shares,
                avg_price,
                current_value,
                pnl,
                status,
                opened_at: opened_at.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(positions))
}

/// GET /api/sidecar/risk
///
/// Returns risk state from the Coordinator's GlobalState.
pub async fn sidecar_get_risk(
    State(state): State<AppState>,
) -> std::result::Result<Json<SidecarRiskState>, (StatusCode, String)> {
    match state.coordinator.as_ref() {
        Some(coordinator) => {
            let global = coordinator.read_state().await;

            let agents: Vec<SidecarAgentInfo> = global
                .agents
                .iter()
                .map(|(id, snap)| SidecarAgentInfo {
                    agent_id: id.clone(),
                    domain: format!("{:?}", snap.domain),
                    status: format!("{:?}", snap.status),
                    last_heartbeat: Some(snap.last_heartbeat.to_rfc3339()),
                })
                .collect();

            Ok(Json(SidecarRiskState {
                coordinator_running: true,
                agents,
                total_exposure_usd: global
                    .portfolio
                    .total_exposure
                    .to_string()
                    .parse()
                    .unwrap_or(0.0),
                queue_depth: global.queue_stats.current_size,
            }))
        }
        None => Ok(Json(SidecarRiskState {
            coordinator_running: false,
            agents: Vec::new(),
            total_exposure_usd: 0.0,
            queue_depth: 0,
        })),
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Persist a sidecar-originated Grok decision to the database for audit trail
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
    .bind(req.market.market_price.to_string().parse::<f64>().unwrap_or(0.0))
    .bind(req.market.yes_best_bid.map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)))
    .bind(req.market.yes_best_ask.map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)))
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
