use axum::http::{header::AUTHORIZATION, header::COOKIE, HeaderMap, StatusCode};
use hmac::{Hmac, Mac};
use rand::{rngs::SysRng, TryRng};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const ADMIN_SESSION_COOKIE: &str = "ploy_admin_auth";
const ADMIN_SESSION_COOKIE_V2_PREFIX: &str = "v2:";

type HmacSha256 = Hmac<Sha256>;

static GENERATED_ADMIN_COOKIE_SECRET: OnceLock<String> = OnceLock::new();

/// Constant-time string comparison to prevent timing side-channel attacks.
/// The length check leaks length information, but for fixed-format bearer tokens
/// this is acceptable — the critical protection is against byte-by-byte guessing.
fn ct_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

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

pub fn expected_sidecar_token() -> Option<String> {
    std::env::var("PLOY_SIDECAR_AUTH_TOKEN")
        .or_else(|_| std::env::var("PLOY_API_SIDECAR_AUTH_TOKEN"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn sidecar_auth_required() -> bool {
    let explicit = [
        "PLOY_SIDECAR_AUTH_REQUIRED",
        "PLOY_GATEWAY_ONLY",
        "PLOY_ENFORCE_GATEWAY_ONLY",
        "PLOY_ENFORCE_COORDINATOR_GATEWAY_ONLY",
    ]
    .iter()
    .find_map(|key| std::env::var(key).ok())
    .map(|raw| parse_boolish(&raw));

    match explicit {
        Some(v) => v,
        None => {
            // Default to true for safety; log once so operators notice.
            tracing::warn!("No PLOY_SIDECAR_AUTH_REQUIRED env set — defaulting to required");
            true
        }
    }
}

fn auth_cookie_secure() -> bool {
    match std::env::var("PLOY_API_AUTH_COOKIE_SECURE") {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => true, // Secure by default; set to false explicitly for local dev
    }
}

fn auth_cookie_max_age_secs() -> i64 {
    std::env::var("PLOY_API_AUTH_COOKIE_MAX_AGE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(8 * 60 * 60)
        .max(60)
}

fn expected_admin_cookie_secret() -> Option<String> {
    std::env::var("PLOY_API_AUTH_COOKIE_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn generated_admin_cookie_secret() -> &'static str {
    GENERATED_ADMIN_COOKIE_SECRET.get_or_init(|| {
        let mut bytes = [0u8; 32];
        SysRng
            .try_fill_bytes(&mut bytes)
            .expect("SysRng should provide admin cookie entropy");
        hex::encode(bytes)
    })
}

fn admin_cookie_secret() -> String {
    expected_admin_cookie_secret().unwrap_or_else(|| generated_admin_cookie_secret().to_string())
}

pub fn admin_token_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn signed_admin_session_value(token: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(admin_cookie_secret().as_bytes())
        .expect("HMAC accepts arbitrary key lengths");
    mac.update(token.as_bytes());
    format!(
        "{}{}",
        ADMIN_SESSION_COOKIE_V2_PREFIX,
        hex::encode(mac.finalize().into_bytes())
    )
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
        signed_admin_session_value(token),
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

fn is_valid_admin_session_cookie(cookie: &str, expected_token: &str) -> bool {
    if let Some(signature) = cookie.strip_prefix(ADMIN_SESSION_COOKIE_V2_PREFIX) {
        let expected = signed_admin_session_value(expected_token);
        if let Some(expected_signature) = expected.strip_prefix(ADMIN_SESSION_COOKIE_V2_PREFIX) {
            if ct_eq(signature, expected_signature) {
                return true;
            }
        }
    }

    let expected_fp = admin_token_fingerprint(expected_token);
    ct_eq(cookie, &expected_fp) || ct_eq(cookie, expected_token)
}

pub fn is_valid_admin_token(provided: &str) -> bool {
    expected_admin_token()
        .map(|expected| ct_eq(provided.trim(), &expected))
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

    if token.is_some_and(|v| ct_eq(v, &expected)) {
        return Ok(());
    }

    let cookie = extract_cookie(headers, ADMIN_SESSION_COOKIE);
    if cookie
        .as_deref()
        .is_some_and(|cookie| is_valid_admin_session_cookie(cookie, &expected))
    {
        return Ok(());
    }

    Err((
        StatusCode::UNAUTHORIZED,
        "admin auth failed (missing/invalid token)".to_string(),
    ))
}

pub fn ensure_sidecar_authorized(
    headers: &HeaderMap,
) -> std::result::Result<(), (StatusCode, String)> {
    let expected = expected_sidecar_token();
    if expected.is_none() && !sidecar_auth_required() {
        return Ok(());
    }
    let Some(expected) = expected else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "sidecar auth is required but token is not configured".to_string(),
        ));
    };

    let token = headers
        .get("x-ploy-sidecar-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(extract_bearer_token)
        });

    match token {
        Some(provided) if ct_eq(provided, &expected) => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            "sidecar auth failed (missing/invalid token)".to_string(),
        )),
    }
}

pub fn ensure_sidecar_or_admin_authorized(
    headers: &HeaderMap,
) -> std::result::Result<(), (StatusCode, String)> {
    if ensure_sidecar_authorized(headers).is_ok() {
        return Ok(());
    }
    ensure_admin_authorized(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
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

    fn cookie_headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(cookie).expect("cookie header"),
        );
        headers
    }

    #[test]
    fn build_admin_session_cookie_emits_v2_hmac_value() {
        with_auth_env(
            &[
                ("PLOY_API_ADMIN_TOKEN", Some("super-secret-admin-token")),
                ("PLOY_API_AUTH_COOKIE_SECRET", Some("cookie-secret")),
                ("PLOY_API_AUTH_COOKIE_SECURE", Some("false")),
            ],
            || {
                let cookie = build_admin_session_cookie("super-secret-admin-token");
                assert!(
                    cookie.contains("ploy_admin_auth=v2:"),
                    "cookie should emit v2-signed value"
                );
                assert!(
                    !cookie.contains(&admin_token_fingerprint("super-secret-admin-token")),
                    "cookie should not fall back to plain sha256 fingerprint"
                );
            },
        );
    }

    #[test]
    fn ensure_admin_authorized_accepts_v2_cookie() {
        with_auth_env(
            &[
                ("PLOY_API_ADMIN_TOKEN", Some("super-secret-admin-token")),
                ("PLOY_API_AUTH_COOKIE_SECRET", Some("cookie-secret")),
            ],
            || {
                let cookie = build_admin_session_cookie("super-secret-admin-token");
                let headers = cookie_headers(&cookie);
                assert!(ensure_admin_authorized(&headers).is_ok());
            },
        );
    }

    #[test]
    fn ensure_admin_authorized_accepts_legacy_sha256_cookie_during_migration() {
        with_auth_env(
            &[
                ("PLOY_API_ADMIN_TOKEN", Some("super-secret-admin-token")),
                ("PLOY_API_AUTH_COOKIE_SECRET", Some("cookie-secret")),
            ],
            || {
                let legacy_cookie = format!(
                    "{}={}",
                    ADMIN_SESSION_COOKIE,
                    admin_token_fingerprint("super-secret-admin-token")
                );
                let headers = cookie_headers(&legacy_cookie);
                assert!(ensure_admin_authorized(&headers).is_ok());
            },
        );
    }

    #[test]
    fn ensure_admin_authorized_rejects_wrong_v2_cookie() {
        with_auth_env(
            &[
                ("PLOY_API_ADMIN_TOKEN", Some("super-secret-admin-token")),
                ("PLOY_API_AUTH_COOKIE_SECRET", Some("cookie-secret")),
            ],
            || {
                let headers = cookie_headers(&format!(
                    "{}=v2:not-the-right-signature",
                    ADMIN_SESSION_COOKIE
                ));
                let err =
                    ensure_admin_authorized(&headers).expect_err("wrong v2 cookie should fail");
                assert_eq!(err.0, StatusCode::UNAUTHORIZED);
            },
        );
    }
}
