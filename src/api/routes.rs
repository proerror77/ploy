use axum::{
    http::{header, HeaderValue, Method},
    routing::{get, post, put},
    Router,
};
use tower_http::cors::CorsLayer;

use crate::api::{handlers, state::AppState, websocket::websocket_handler};

fn build_cors_layer() -> CorsLayer {
    let mut origins: Vec<HeaderValue> = std::env::var("PLOY_API_CORS_ALLOWED_ORIGINS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .filter_map(|v| HeaderValue::from_str(v).ok())
                .collect()
        })
        .unwrap_or_default();

    if origins.is_empty() {
        origins.push(HeaderValue::from_static("http://localhost:5173"));
        origins.push(HeaderValue::from_static("http://127.0.0.1:5173"));
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-ploy-admin-token"),
            header::HeaderName::from_static("x-ploy-sidecar-token"),
        ])
}

pub fn create_router(state: AppState) -> Router {
    // CORS configuration
    let cors = build_cors_layer();

    Router::new()
        // Health check (top-level, used by docker/scripts for readiness probes)
        .route("/health", get(handlers::health_handler))
        // Auth endpoints
        .route("/api/auth/session", get(handlers::get_auth_session))
        .route("/api/auth/login", post(handlers::login_admin))
        .route("/api/auth/logout", post(handlers::logout_admin))
        // Stats endpoints
        .route("/api/stats/today", get(handlers::get_today_stats))
        .route("/api/stats/pnl", get(handlers::get_pnl_history))
        // Trade endpoints
        .route("/api/trades", get(handlers::get_trades))
        .route("/api/trades/:id", get(handlers::get_trade_by_id))
        // Position endpoints
        .route("/api/positions", get(handlers::get_positions))
        // System endpoints
        .route("/api/system/status", get(handlers::get_system_status))
        .route("/api/system/start", post(handlers::start_system))
        .route("/api/system/stop", post(handlers::stop_system))
        .route("/api/system/restart", post(handlers::restart_system))
        .route("/api/system/pause", post(handlers::pause_system))
        .route("/api/system/resume", post(handlers::resume_system))
        .route("/api/system/halt", post(handlers::halt_system))
        // Config endpoints
        .route("/api/config", get(handlers::get_config))
        .route("/api/config", put(handlers::update_config))
        // Strategy status endpoints
        .route(
            "/api/strategies/running",
            get(handlers::get_running_strategies),
        )
        // Strategy deployment matrix (control-plane first-class resource)
        .route(
            "/api/deployments",
            get(handlers::list_deployments).put(handlers::upsert_deployments),
        )
        .route(
            "/api/deployments/:id",
            get(handlers::get_deployment).delete(handlers::delete_deployment),
        )
        .route(
            "/api/deployments/:id/enable",
            post(handlers::enable_deployment),
        )
        .route(
            "/api/deployments/:id/disable",
            post(handlers::disable_deployment),
        )
        // Account-level governance policy (OpenClaw control-plane)
        .route(
            "/api/governance/status",
            get(handlers::get_governance_status),
        )
        .route(
            "/api/governance/policy",
            get(handlers::get_governance_policy).put(handlers::put_governance_policy),
        )
        .route(
            "/api/governance/policy/history",
            get(handlers::get_governance_policy_history),
        )
        // Security endpoints
        .route("/api/security/events", get(handlers::get_security_events))
        // Sidecar endpoints (Claude Agent SDK → Rust backend)
        .route(
            "/api/sidecar/grok/decision",
            post(handlers::sidecar_grok_decision),
        )
        .route(
            "/api/sidecar/intents",
            post(handlers::sidecar_submit_intent),
        )
        .route("/api/sidecar/orders", post(handlers::sidecar_submit_order))
        .route(
            "/api/sidecar/positions",
            get(handlers::sidecar_get_positions),
        )
        .route("/api/sidecar/risk", get(handlers::sidecar_get_risk))
        // WebSocket endpoint
        .route("/ws", get(websocket_handler))
        // Add state and CORS
        .with_state(state)
        .layer(cors)
}
