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
#[cfg(test)]
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::{auth::ensure_sidecar_authorized, state::AppState};
#[cfg(test)]
use crate::control_plane::{
    MarketSelector, StrategyDeployment, StrategyLifecycleStage, StrategyProductType, Timeframe,
};
use crate::domain::market::Side;
use crate::platform::{Domain, OrderIntent, OrderPriority};

mod grok_decision;
mod ingress;
mod read_side;

pub use grok_decision::sidecar_grok_decision;
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

// ── Request / Response types ─────────────────────────────────────

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
