use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

use crate::adapters::PostgresStore;
use crate::agent::grok::GrokClient;
use crate::api::state::StrategyConfigState;
use crate::api::{create_router, AppState};
use crate::coordinator::CoordinatorHandle;
use crate::error::Result;

/// Start the API server
pub async fn start_api_server(
    store: Arc<PostgresStore>,
    port: u16,
    config: StrategyConfigState,
) -> Result<()> {
    let app_state = AppState::new(store, config);
    app_state.spawn_realtime_broadcast_loop();

    let app = create_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("🚀 API server listening on http://{}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Start the API server with platform services (coordinator + grok)
pub async fn start_api_server_with_platform(
    store: Arc<PostgresStore>,
    port: u16,
    config: StrategyConfigState,
    coordinator: Option<CoordinatorHandle>,
    grok_client: Option<Arc<GrokClient>>,
    account_id: String,
    dry_run: bool,
) -> Result<()> {
    let app_state = AppState::with_platform_services(
        store,
        config,
        coordinator,
        grok_client,
        account_id,
        dry_run,
    );
    app_state.spawn_realtime_broadcast_loop();

    let app = create_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("🚀 API server (platform mode) listening on http://{}", addr);

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
    let handle = tokio::spawn(async move { start_api_server(store, port, config).await });

    Ok(handle)
}

/// Start the API server with platform services in the background
pub async fn start_api_server_platform_background(
    store: Arc<PostgresStore>,
    port: u16,
    config: StrategyConfigState,
    coordinator: Option<CoordinatorHandle>,
    grok_client: Option<Arc<GrokClient>>,
    account_id: String,
    dry_run: bool,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let handle = tokio::spawn(async move {
        start_api_server_with_platform(
            store,
            port,
            config,
            coordinator,
            grok_client,
            account_id,
            dry_run,
        )
        .await
    });

    Ok(handle)
}
