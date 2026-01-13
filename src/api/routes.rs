use axum::{
    routing::{get, post, put},
    Router,
};
use tower_http::cors::{Any, CorsLayer};

use crate::api::{
    handlers,
    state::AppState,
    websocket::websocket_handler,
};

pub fn create_router(state: AppState) -> Router {
    // CORS configuration
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
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
        // Config endpoints
        .route("/api/config", get(handlers::get_config))
        .route("/api/config", put(handlers::update_config))
        // Security endpoints
        .route("/api/security/events", get(handlers::get_security_events))
        // WebSocket endpoint
        .route("/ws", get(websocket_handler))
        // Add state and CORS
        .with_state(state)
        .layer(cors)
}
