//! Sidecar REST endpoints — bridge between Claude Agent SDK and Rust trading core
//!
//! These endpoints are called by the TypeScript sidecar (ploy-sidecar) which uses
//! Claude Agent SDK + MCP tools for research, then routes order decisions through
//! Grok and the Coordinator.
//!
//! Endpoints:
//! - POST /api/sidecar/grok/decision — Unified Grok decision with full context
//! - POST /api/sidecar/intents      — Unified intent ingress (OpenClaw/RPC/scripts)
//! - POST /api/sidecar/orders       — Submit order through Coordinator
//! - GET  /api/sidecar/positions     — Current positions from DB
//! - GET  /api/sidecar/risk          — Risk state from Coordinator

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::{
    auth::ensure_sidecar_authorized,
    state::AppState,
};
use crate::control_plane::{
    MarketSelector, StrategyDeployment, StrategyLifecycleStage, StrategyProductType, Timeframe,
};
use crate::domain::market::Side;
use crate::platform::{Domain, OrderIntent, OrderPriority};
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};
use crate::strategy::nba_comeback::grok_decision::{
    ComebackSnapshot, DecisionTrigger, GrokDecision, MarketSnapshot, RiskMetrics,
    UnifiedDecisionRequest, build_unified_prompt, parse_decision_response,
};
use crate::strategy::nba_comeback::grok_intel::{
    GrokGameIntel, InjuryImpact, InjuryUpdate, MomentumDirection,
};

mod ingress;
mod read_side;

#[cfg(test)]
use ingress::ensure_deployment_accepts_live_ingress;
use ingress::{
    apply_deployment_metadata, broadcast_sidecar_activity, clamp_external_priority,
    deployment_default_priority, ensure_agent_authorized, ensure_domain_allowed,
    map_coordinator_submit_error, parse_binary_side, parse_is_buy, parse_order_priority,
    parse_sidecar_domain, resolve_intent_deployment, resolve_request_account_scope,
    sidecar_orders_live_enabled, table_has_account_scope, validate_account_scope,
    validate_deployment_binding,
};
pub use read_side::{sidecar_get_positions, sidecar_get_risk};

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
    pub account_id: Option<String>,
    pub deployment_id: Option<String>,
    pub domain: Option<String>, // "crypto" | "sports" | "politics" | "economics"
    pub market_slug: String,
    pub token_id: String,
    pub side: Option<String>, // "up"/"down" or "YES"/"NO"
    pub is_buy: Option<bool>, // defaults to true
    pub shares: u64,
    pub price: f64,
    pub idempotency_key: Option<String>,
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

/// POST /api/sidecar/intents — request body (OpenClaw/RPC ingress)
#[derive(Debug, Deserialize)]
pub struct SidecarIntentRequest {
    pub intent_id: Option<String>,
    pub account_id: Option<String>,
    pub deployment_id: String,
    pub agent_id: Option<String>,
    pub domain: Option<String>,
    pub market_slug: String,
    pub token_id: String,
    pub side: Option<String>,       // "UP"/"DOWN" or "YES"/"NO"
    pub order_side: Option<String>, // "BUY"/"SELL"
    pub is_buy: Option<bool>,
    pub size: u64,
    pub price_limit: f64,
    pub idempotency_key: Option<String>,
    pub reason: Option<String>,
    pub confidence: Option<f64>,
    pub edge: Option<f64>,
    pub priority: Option<String>, // "high" | "normal" | "low" (critical gated by env)
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    pub dry_run: Option<bool>,
}

/// POST /api/sidecar/intents — response
#[derive(Debug, Serialize)]
pub struct SidecarIntentResponse {
    pub success: bool,
    pub intent_id: String,
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
    pub risk_state: String,
    pub daily_pnl_usd: f64,
    pub daily_loss_limit_usd: f64,
    pub current_drawdown_usd: f64,
    pub max_drawdown_observed_usd: f64,
    pub drawdown_limit_usd: Option<f64>,
    pub queue_depth: usize,
    pub positions: Vec<SidecarRiskPosition>,
    pub circuit_breaker_events: Vec<SidecarCircuitBreakerEvent>,
}

#[derive(Debug, Serialize)]
pub struct SidecarRiskPosition {
    pub market: String,
    pub side: String,
    pub size: f64,
    pub pnl_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct SidecarCircuitBreakerEvent {
    pub timestamp: String,
    pub reason: String,
    pub state: String,
}

// ── Handlers ─────────────────────────────────────────────────────

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
    let comeback = match (
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
    let market_price =
        Decimal::from_str(&format!("{:.4}", req.market_price)).unwrap_or_else(|_| Decimal::ZERO);
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
            Err((StatusCode::BAD_GATEWAY, format!("Grok query failed: {}", e)))
        }
    }
}

/// POST /api/sidecar/orders
///
/// Submit an order through the Coordinator pipeline (risk gate → queue → execution).
pub async fn sidecar_submit_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SidecarOrderRequest>,
) -> std::result::Result<Json<SidecarOrderResponse>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;
    validate_account_scope(&state, req.account_id.as_deref())?;

    let dry_run = req.dry_run.unwrap_or(true);

    // Validate price range
    let price = Decimal::from_str(&format!("{:.4}", req.price))
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid price format".to_string()))?;

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
            format!("Order cost ${:.2} exceeds sidecar limit $50", order_cost),
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

    if !sidecar_orders_live_enabled() {
        return Err((
            StatusCode::CONFLICT,
            "live /api/sidecar/orders is disabled; route live intents to /api/sidecar/intents"
                .to_string(),
        ));
    }

    // Live order — requires Coordinator
    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;

    let deployment_id = req
        .deployment_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "deployment_id is required for live /api/sidecar/orders".to_string(),
            )
        })?
        .to_string();
    let deployment = resolve_intent_deployment(&state, &deployment_id).await?;

    let domain_default = deployment
        .as_ref()
        .map(|d| d.domain)
        .unwrap_or(Domain::Sports);
    let domain = parse_sidecar_domain(req.domain.as_deref(), domain_default)?;
    ensure_domain_allowed(&state, domain, "sidecar order domain")?;
    let side = parse_binary_side(req.side.as_deref())?;
    let is_buy = parse_is_buy(None, req.is_buy)?;

    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "sidecar".to_string());
    metadata.insert("strategy".to_string(), req.strategy.clone());
    metadata.insert("deployment_id".to_string(), deployment_id);
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
    if let Some(idem) = req
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("idempotency_key".to_string(), idem.to_string());
    }
    metadata.insert("domain".to_string(), domain.to_string());
    metadata.insert("account_id".to_string(), state.account_id.clone());
    if let Some(dep) = deployment.as_ref() {
        validate_deployment_binding(dep, domain, &req.market_slug, &metadata)?;
        apply_deployment_metadata(&mut metadata, dep);
    }

    let mut intent = OrderIntent::new(
        "sidecar",
        domain,
        &req.market_slug,
        &req.token_id,
        side,
        is_buy,
        req.shares,
        price,
    );
    intent.priority = clamp_external_priority(
        deployment
            .as_ref()
            .map(deployment_default_priority)
            .unwrap_or(OrderPriority::Normal),
    );
    intent.metadata = metadata;

    let intent_id = intent.intent_id.to_string();

    ensure_agent_authorized(&state, "sidecar")?;
    coordinator
        .submit_order(intent)
        .await
        .map_err(|e| map_coordinator_submit_error("Failed to submit order", e))?;

    broadcast_sidecar_activity(
        &state,
        &intent_id,
        &req.market_slug,
        &req.token_id,
        side,
        req.shares,
        price,
    );

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

/// POST /api/sidecar/intents
///
/// Unified ingestion endpoint for external runtimes (OpenClaw/RPC/scripts).
/// Always routes through Coordinator (risk gate -> duplicate guard -> allocator -> execution).
pub async fn sidecar_submit_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SidecarIntentRequest>,
) -> std::result::Result<Json<SidecarIntentResponse>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;
    let requested_account =
        resolve_request_account_scope(req.account_id.as_deref(), Some(&req.metadata))?;
    validate_account_scope(&state, requested_account.as_deref())?;

    let dry_run = req.dry_run.unwrap_or(false);
    let price = Decimal::from_str(&format!("{:.6}", req.price_limit)).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid price_limit format".to_string(),
        )
    })?;
    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Err((
            StatusCode::BAD_REQUEST,
            "price_limit must be between 0 and 1 (exclusive)".to_string(),
        ));
    }
    if req.size == 0 {
        return Err((StatusCode::BAD_REQUEST, "size must be > 0".to_string()));
    }
    if req.deployment_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment_id is required".to_string(),
        ));
    }
    if req.market_slug.trim().is_empty() || req.token_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "market_slug and token_id are required".to_string(),
        ));
    }

    let deployment = resolve_intent_deployment(&state, &req.deployment_id).await?;

    let domain_default = deployment
        .as_ref()
        .map(|d| d.domain)
        .unwrap_or(Domain::Crypto);
    let domain = parse_sidecar_domain(req.domain.as_deref(), domain_default)?;
    ensure_domain_allowed(&state, domain, "sidecar intent domain")?;
    let side = parse_binary_side(req.side.as_deref())?;
    let is_buy = parse_is_buy(req.order_side.as_deref(), req.is_buy)?;
    let priority = if req.priority.as_deref().is_some() {
        parse_order_priority(req.priority.as_deref())?
    } else {
        deployment
            .as_ref()
            .map(deployment_default_priority)
            .unwrap_or(OrderPriority::Normal)
    };
    let priority = clamp_external_priority(priority);
    let agent_id = req
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("openclaw_rpc")
        .to_string();

    let mut metadata = req.metadata;
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "sidecar.intent_ingress".to_string());
    metadata.insert("deployment_id".to_string(), req.deployment_id.clone());
    metadata
        .entry("domain".to_string())
        .or_insert_with(|| domain.to_string());
    metadata.insert("account_id".to_string(), state.account_id.clone());
    if let Some(idem) = req
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("idempotency_key".to_string(), idem.to_string());
    }
    if let Some(reason) = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("intent_reason".to_string(), reason.to_string());
    }
    if let Some(edge) = req.edge {
        metadata.insert("signal_edge".to_string(), format!("{:.6}", edge));
    }
    if let Some(conf) = req.confidence {
        metadata.insert("signal_confidence".to_string(), format!("{:.6}", conf));
    }
    if let Some(dep) = deployment.as_ref() {
        validate_deployment_binding(dep, domain, &req.market_slug, &metadata)?;
        apply_deployment_metadata(&mut metadata, dep);
    }

    let mut intent = OrderIntent::new(
        &agent_id,
        domain,
        &req.market_slug,
        &req.token_id,
        side,
        is_buy,
        req.size,
        price,
    );
    intent.priority = priority;
    intent.metadata = metadata;
    if let Some(raw) = req
        .intent_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let parsed = Uuid::parse_str(raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "intent_id must be a UUID".to_string(),
            )
        })?;
        intent.intent_id = parsed;
    }
    let intent_id = intent.intent_id.to_string();

    if dry_run {
        return Ok(Json(SidecarIntentResponse {
            success: true,
            intent_id,
            message: "Dry-run: intent validated and skipped".to_string(),
            dry_run: true,
        }));
    }

    ensure_agent_authorized(&state, &agent_id)?;

    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;
    coordinator
        .submit_order(intent)
        .await
        .map_err(|e| map_coordinator_submit_error("Failed to submit intent", e))?;

    broadcast_sidecar_activity(
        &state,
        &intent_id,
        &req.market_slug,
        &req.token_id,
        side,
        req.size,
        price,
    );

    Ok(Json(SidecarIntentResponse {
        success: true,
        intent_id,
        message: "Intent submitted to coordinator pipeline".to_string(),
        dry_run: false,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::{StrategyLifecycleStage, StrategyProductType, Timeframe};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn sample_deployment(lifecycle_stage: StrategyLifecycleStage) -> StrategyDeployment {
        StrategyDeployment {
            id: "deploy.test.crypto".to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v2.1.0".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Static {
                symbol: None,
                series_id: None,
                market_slug: Some("btc-price-series-15m".to_string()),
            },
            timeframe: Timeframe::M15,
            enabled: true,
            allocator_profile: "balanced".to_string(),
            risk_profile: "default".to_string(),
            priority: 80,
            cooldown_secs: 30,
            lifecycle_stage,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: Some(Utc::now()),
            last_evaluation_score: Some(0.73),
        }
    }

    #[test]
    fn parse_domain_rejects_unknown_values() {
        assert!(parse_sidecar_domain(Some("crypto"), Domain::Sports).is_ok());
        assert!(parse_sidecar_domain(Some("sports"), Domain::Crypto).is_ok());
        assert!(parse_sidecar_domain(Some("custom:42"), Domain::Crypto).is_ok());
        assert!(parse_sidecar_domain(Some("bad-domain"), Domain::Crypto).is_err());
    }

    #[test]
    fn parse_side_rejects_unknown_values() {
        assert_eq!(parse_binary_side(Some("UP")).unwrap(), Side::Up);
        assert_eq!(parse_binary_side(Some("NO")).unwrap(), Side::Down);
        assert!(parse_binary_side(Some("LEFT")).is_err());
    }

    #[test]
    fn parse_order_side_rejects_unknown_values() {
        assert_eq!(parse_is_buy(Some("BUY"), None).unwrap(), true);
        assert_eq!(parse_is_buy(Some("SELL"), None).unwrap(), false);
        assert!(parse_is_buy(Some("HOLD"), None).is_err());
    }

    #[test]
    fn parse_order_side_rejects_conflicting_is_buy() {
        assert!(parse_is_buy(Some("BUY"), Some(false)).is_err());
        assert!(parse_is_buy(Some("SELL"), Some(true)).is_err());
        assert_eq!(parse_is_buy(Some("SELL"), Some(false)).unwrap(), false);
    }

    #[test]
    fn sidecar_auth_fails_closed_when_token_not_configured() {
        let _guard = ENV_LOCK.lock().unwrap();
        let keys = [
            "PLOY_SIDECAR_AUTH_TOKEN",
            "PLOY_API_SIDECAR_AUTH_TOKEN",
            "PLOY_SIDECAR_AUTH_REQUIRED",
            "PLOY_GATEWAY_ONLY",
            "PLOY_ENFORCE_GATEWAY_ONLY",
            "PLOY_ENFORCE_COORDINATOR_GATEWAY_ONLY",
        ];
        let prev: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();

        for k in keys {
            set_env(k, None);
        }

        let result = ensure_sidecar_authorized(&HeaderMap::new());
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(msg.contains("not configured"));

        for (k, v) in prev {
            set_env(&k, v.as_deref());
        }
    }

    #[test]
    fn sidecar_auth_accepts_valid_bearer_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_SIDECAR_AUTH_TOKEN";
        let prev = std::env::var(key).ok();
        set_env(key, Some("expected-token"));

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer expected-token"),
        );

        let result = ensure_sidecar_authorized(&headers);
        assert!(result.is_ok());

        set_env(key, prev.as_deref());
    }

    #[test]
    fn sidecar_auth_rejects_invalid_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_SIDECAR_AUTH_TOKEN";
        let prev = std::env::var(key).ok();
        set_env(key, Some("expected-token"));

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ploy-sidecar-token",
            axum::http::HeaderValue::from_static("wrong-token"),
        );

        let result = ensure_sidecar_authorized(&headers);
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(msg.contains("missing/invalid token"));

        set_env(key, prev.as_deref());
    }

    #[test]
    fn non_live_deployment_ingress_is_blocked_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS";
        let prev = std::env::var(key).ok();
        set_env(key, None);

        let deployment = sample_deployment(StrategyLifecycleStage::Paper);
        let err = ensure_deployment_accepts_live_ingress(&deployment)
            .expect_err("paper lifecycle should be blocked without override");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("lifecycle_stage=paper"));

        set_env(key, prev.as_deref());
    }

    #[test]
    fn non_live_deployment_ingress_can_be_enabled_for_migration() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS";
        let prev = std::env::var(key).ok();
        set_env(key, Some("true"));

        let deployment = sample_deployment(StrategyLifecycleStage::Backtest);
        assert!(ensure_deployment_accepts_live_ingress(&deployment).is_ok());

        set_env(key, prev.as_deref());
    }

    #[test]
    fn deployment_metadata_includes_strategy_contract_fields() {
        let deployment = sample_deployment(StrategyLifecycleStage::Live);
        let mut metadata = HashMap::new();
        apply_deployment_metadata(&mut metadata, &deployment);

        assert_eq!(
            metadata.get("strategy_version").map(String::as_str),
            Some("v2.1.0")
        );
        assert_eq!(
            metadata.get("lifecycle_stage").map(String::as_str),
            Some("live")
        );
        assert_eq!(
            metadata.get("product_type").map(String::as_str),
            Some("binary_option")
        );
    }
}
