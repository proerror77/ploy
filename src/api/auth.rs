use axum::http::{header::AUTHORIZATION, header::COOKIE, HeaderMap, StatusCode};
use sha2::{Digest, Sha256};

pub const ADMIN_SESSION_COOKIE: &str = "ploy_admin_auth";

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

pub fn admin_auth_required() -> bool {
    match std::env::var("PLOY_API_ADMIN_AUTH_REQUIRED") {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => true,
    }
}

pub fn expected_admin_token() -> Option<String> {
    std::env::var("PLOY_API_ADMIN_TOKEN")
        .or_else(|_| std::env::var("PLOY_ADMIN_TOKEN"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn auth_cookie_secure() -> bool {
    match std::env::var("PLOY_API_AUTH_COOKIE_SECURE") {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => false,
    }
}

fn auth_cookie_max_age_secs() -> i64 {
    std::env::var("PLOY_API_AUTH_COOKIE_MAX_AGE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(8 * 60 * 60)
        .max(60)
}

pub fn admin_token_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn extract_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?.trim();
        let value = parts.next()?.trim();
        if key == cookie_name {
            Some(value.to_string())
        } else {
            None
        }
    })
}

pub fn build_admin_session_cookie(token: &str) -> String {
    let secure = if auth_cookie_secure() { "; Secure" } else { "" };
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        ADMIN_SESSION_COOKIE,
        admin_token_fingerprint(token),
        auth_cookie_max_age_secs(),
        secure
    )
}

pub fn build_admin_logout_cookie() -> String {
    let secure = if auth_cookie_secure() { "; Secure" } else { "" };
    format!(
        "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        ADMIN_SESSION_COOKIE, secure
    )
}

fn extract_bearer_token(raw: &str) -> Option<&str> {
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(str::trim)
}

pub fn is_valid_admin_token(provided: &str) -> bool {
    expected_admin_token()
        .map(|expected| provided.trim() == expected)
        .unwrap_or(false)
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

    if token.is_some_and(|v| v == expected) {
        return Ok(());
    }

    let expected_fp = admin_token_fingerprint(&expected);
    let cookie = extract_cookie(headers, ADMIN_SESSION_COOKIE);
    if cookie
        .as_deref()
        .is_some_and(|v| v == expected_fp || v == expected)
    {
        return Ok(());
    }

    Err((
        StatusCode::UNAUTHORIZED,
        "admin auth failed (missing/invalid token)".to_string(),
    ))
}
