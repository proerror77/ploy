use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::api::auth::{ensure_admin_authorized, is_valid_admin_token};
use crate::api::state::AppState;

#[derive(Deserialize)]
pub struct WsAuth {
    token: Option<String>,
}

fn websocket_admin_authorized(headers: &HeaderMap, auth: &WsAuth) -> bool {
    ensure_admin_authorized(headers).is_ok()
        || auth
            .token
            .as_deref()
            .is_some_and(is_valid_admin_token)
}

/// WebSocket handler — accepts the normal admin auth surface (cookie/header),
/// with `?token=` kept only as a compatibility fallback.
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(auth): Query<WsAuth>,
    State(state): State<AppState>,
) -> std::result::Result<impl IntoResponse, StatusCode> {
    if websocket_admin_authorized(&headers, &auth) {
        return Ok(ws.on_upgrade(|socket| handle_socket(socket, state)));
    }

    warn!(
        used_query_token = auth.token.is_some(),
        "WebSocket connection rejected: missing or invalid admin auth"
    );
    Err(StatusCode::UNAUTHORIZED)
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
            if sender.send(Message::Text(json.into())).await.is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::{build_admin_session_cookie, ADMIN_SESSION_COOKIE};
    use axum::http::{
        header::{AUTHORIZATION, COOKIE},
        HeaderValue,
    };
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn set_env_var(key: &str, value: Option<&str>) {
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    fn with_auth_env<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let saved = vars
            .iter()
            .map(|(key, _)| ((*key).to_string(), std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in vars {
            set_env_var(key, *value);
        }
        let result = f();
        for (key, value) in saved {
            set_env_var(&key, value.as_deref());
        }
        result
    }

    #[test]
    fn websocket_admin_authorized_accepts_header_cookie_and_query_fallback() {
        with_auth_env(
            &[
                ("PLOY_API_ADMIN_TOKEN", Some("super-secret-admin-token")),
                ("PLOY_API_AUTH_COOKIE_SECRET", Some("cookie-secret")),
            ],
            || {
                let mut header_auth = HeaderMap::new();
                header_auth.insert(
                    AUTHORIZATION,
                    HeaderValue::from_static("Bearer super-secret-admin-token"),
                );
                assert!(websocket_admin_authorized(&header_auth, &WsAuth { token: None }));

                let mut cookie_auth = HeaderMap::new();
                cookie_auth.insert(
                    COOKIE,
                    HeaderValue::from_str(&build_admin_session_cookie("super-secret-admin-token"))
                        .expect("cookie header"),
                );
                assert!(websocket_admin_authorized(&cookie_auth, &WsAuth { token: None }));

                let empty_headers = HeaderMap::new();
                assert!(websocket_admin_authorized(
                    &empty_headers,
                    &WsAuth {
                        token: Some("super-secret-admin-token".to_string())
                    }
                ));

                assert!(!websocket_admin_authorized(
                    &empty_headers,
                    &WsAuth {
                        token: Some("wrong-token".to_string())
                    }
                ));

                let legacy_cookie = format!(
                    "{}={}",
                    ADMIN_SESSION_COOKIE,
                    crate::api::auth::admin_token_fingerprint("super-secret-admin-token")
                );
                let mut legacy_cookie_headers = HeaderMap::new();
                legacy_cookie_headers.insert(
                    COOKIE,
                    HeaderValue::from_str(&legacy_cookie).expect("legacy cookie header"),
                );
                assert!(websocket_admin_authorized(
                    &legacy_cookie_headers,
                    &WsAuth { token: None }
                ));
            },
        );
    }
}
