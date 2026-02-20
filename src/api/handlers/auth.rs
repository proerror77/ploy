use axum::{
    extract::State,
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::api::{
    auth::{
        admin_auth_required, build_admin_logout_cookie, build_admin_session_cookie,
        ensure_admin_authorized, expected_admin_token, is_valid_admin_token,
    },
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct AdminLoginRequest {
    pub admin_token: String,
}

#[derive(Debug, Serialize)]
pub struct AuthSessionResponse {
    pub authenticated: bool,
    pub auth_required: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthMutationResponse {
    pub success: bool,
}

/// GET /api/auth/session
pub async fn get_auth_session(
    headers: HeaderMap,
) -> std::result::Result<Json<AuthSessionResponse>, (StatusCode, String)> {
    let authenticated = ensure_admin_authorized(&headers).is_ok();
    Ok(Json(AuthSessionResponse {
        authenticated,
        auth_required: admin_auth_required(),
    }))
}

/// POST /api/auth/login
pub async fn login_admin(
    State(_state): State<AppState>,
    Json(req): Json<AdminLoginRequest>,
) -> std::result::Result<(HeaderMap, Json<AuthMutationResponse>), (StatusCode, String)> {
    if req.admin_token.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "admin_token is required".to_string(),
        ));
    }
    let Some(expected) = expected_admin_token() else {
        if !admin_auth_required() {
            return Ok((
                HeaderMap::new(),
                Json(AuthMutationResponse { success: true }),
            ));
        }
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "admin auth is required but PLOY_API_ADMIN_TOKEN is not configured".to_string(),
        ));
    };
    if !is_valid_admin_token(&req.admin_token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "admin auth failed (missing/invalid token)".to_string(),
        ));
    }

    let mut headers = HeaderMap::new();
    let cookie = build_admin_session_cookie(&expected);
    let cookie_value = HeaderValue::from_str(&cookie).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to build auth cookie: {}", e),
        )
    })?;
    headers.insert(SET_COOKIE, cookie_value);

    Ok((headers, Json(AuthMutationResponse { success: true })))
}

/// POST /api/auth/logout
pub async fn logout_admin(
    State(_state): State<AppState>,
) -> std::result::Result<(HeaderMap, Json<AuthMutationResponse>), (StatusCode, String)> {
    let mut headers = HeaderMap::new();
    let cookie = build_admin_logout_cookie();
    let cookie_value = HeaderValue::from_str(&cookie).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to clear auth cookie: {}", e),
        )
    })?;
    headers.insert(SET_COOKIE, cookie_value);
    Ok((headers, Json(AuthMutationResponse { success: true })))
}
