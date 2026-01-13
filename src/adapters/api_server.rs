use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

use crate::adapters::PostgresStore;
use crate::api::{create_router, AppState};
use crate::api::state::StrategyConfigState;
use crate::error::Result;

/// Start the API server
pub async fn start_api_server(
    store: Arc<PostgresStore>,
    port: u16,
    config: StrategyConfigState,
) -> Result<()> {
    let app_state = AppState::new(store, config);

    let app = create_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("🚀 API server listening on http://{}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Start the API server in the background
pub async fn start_api_server_background(
    store: Arc<PostgresStore>,
    port: u16,
    config: StrategyConfigState,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let handle = tokio::spawn(async move {
        start_api_server(store, port, config).await
    });

    Ok(handle)
}
