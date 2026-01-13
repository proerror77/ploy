use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use tracing::{error, info};

use crate::api::state::AppState;

/// WebSocket handler
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
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
