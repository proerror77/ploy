use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::api::auth::is_valid_admin_token;
use crate::api::state::AppState;

#[derive(Deserialize)]
pub struct WsAuth {
    token: Option<String>,
}

/// WebSocket handler — requires valid admin token via ?token= query param.
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    Query(auth): Query<WsAuth>,
    State(state): State<AppState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    match auth.token {
        Some(ref t) if is_valid_admin_token(t) => {
            Ok(ws.on_upgrade(|socket| handle_socket(socket, state)))
        }
        _ => {
            warn!("WebSocket connection rejected: missing or invalid token");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to broadcast channel
    let mut rx = state.ws_tx.subscribe();

    // Spawn a task to forward broadcast messages to this WebSocket
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            // Serialize message to JSON
            let json = match serde_json::to_string(&msg) {
                Ok(json) => json,
                Err(e) => {
                    error!("Failed to serialize WebSocket message: {}", e);
                    continue;
                }
            };

            // Send to client
            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages (ping/pong) in the main task
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Ping(_) | Message::Pong(_) => {
                // Axum handles ping/pong automatically
            }
            Message::Close(_) => {
                break;
            }
            _ => {}
        }
    }

    // Abort the send task when connection closes
    send_task.abort();

    info!("WebSocket connection closed");
}
