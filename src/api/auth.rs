use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn admin_auth_required() -> bool {
    match std::env::var("PLOY_API_ADMIN_AUTH_REQUIRED") {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => true,
    }
}

fn expected_admin_token() -> Option<String> {
    std::env::var("PLOY_API_ADMIN_TOKEN")
        .or_else(|_| std::env::var("PLOY_ADMIN_TOKEN"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn extract_bearer_token(raw: &str) -> Option<&str> {
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(str::trim)
}

pub fn ensure_admin_authorized(
    headers: &HeaderMap,
) -> std::result::Result<(), (StatusCode, String)> {
    let expected = expected_admin_token();
    if expected.is_none() && !admin_auth_required() {
        return Ok(());
    }
    let Some(expected) = expected else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "admin auth is required but PLOY_API_ADMIN_TOKEN is not configured".to_string(),
        ));
    };

    let token = headers
        .get("x-ploy-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(extract_bearer_token)
        });

    match token {
        Some(v) if v == expected => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            "admin auth failed (missing/invalid token)".to_string(),
        )),
    }
}
