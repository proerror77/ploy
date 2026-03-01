use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode},
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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// WebSocket handler — requires valid admin token via Authorization header or `?token=`.
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(auth): Query<WsAuth>,
    State(state): State<AppState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    let header_token = bearer_token(&headers);
    let token = header_token.or(auth.token.as_deref());
    if header_token.is_none() && auth.token.is_some() {
        warn!("WebSocket auth via query token is deprecated; use Authorization: Bearer <token>");
    }
    match token {
        Some(t) if is_valid_admin_token(t) => {
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
        loop {
            match rx.recv().await {
                Ok(msg) => {
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
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        "WebSocket broadcast lagged; skipped {} messages for one client",
                        skipped
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
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

#[cfg(test)]
mod tests {
    use super::bearer_token;
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn bearer_token_extracts_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc123"),
        );

        assert_eq!(bearer_token(&headers), Some("abc123"));
    }

    #[test]
    fn bearer_token_rejects_non_bearer_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Token abc123"),
        );

        assert_eq!(bearer_token(&headers), None);
    }
}
