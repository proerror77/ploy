use axum::{
    routing::{get, post, put},
    Router,
};
use tower_http::cors::{Any, CorsLayer};

use crate::api::{handlers, state::AppState, websocket::websocket_handler};

pub fn create_router(state: AppState) -> Router {
    // CORS configuration
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Health check (top-level, used by docker/scripts for readiness probes)
        .route("/health", get(handlers::health_handler))
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
        // Security endpoints
        .route("/api/security/events", get(handlers::get_security_events))
        // Sidecar endpoints (Claude Agent SDK → Rust backend)
        .route(
            "/api/sidecar/grok/decision",
            post(handlers::sidecar_grok_decision),
        )
        .route("/api/sidecar/intents", post(handlers::sidecar_submit_intent))
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
