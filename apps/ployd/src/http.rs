use crate::events::EventBroker;
use crate::runtime::{next_paper_intent_id, PloyDaemon};
use chrono::Utc;
use hmac::{Hmac, Mac};
use ploy_operator_contracts::{
    AuditLogEntry, ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest,
    DeploymentSnapshotEvent, IntentPurpose, OperatorEvent, OrderReplaceRequest, PaperIntentRequest,
    StatusUpdate, SystemSnapshotEvent, SystemStatus, TradingSnapshotEvent,
};
use ploy_trading::{TradeSide, TradingIntent};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::config::PlatformConfig;
#[cfg(test)]
use std::path::Path;

#[derive(Debug)]
pub struct AppState {
    pub daemon: Arc<Mutex<PloyDaemon>>,
    pub events: Arc<EventBroker>,
}

const ADMIN_SESSION_COOKIE_NAME: &str = "ploy_admin_session";
const AUDIT_LOG_TAIL_LIMIT: usize = 200;
type HmacSha256 = Hmac<Sha256>;
static REQUEST_RATE_LIMITER: OnceLock<Mutex<RateLimiter>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthLevel {
    None,
    Sidecar,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequiredAccess {
    Public,
    ReadOnly,
    Admin,
}

#[derive(Debug, Default)]
struct RateLimiter {
    requests: BTreeMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    fn allow(&mut self, key: &str, limit_per_minute: u32) -> bool {
        if limit_per_minute == 0 {
            return true;
        }

        let now = Instant::now();
        let cutoff = now - Duration::from_secs(60);
        let bucket = self.requests.entry(key.to_string()).or_default();
        while matches!(bucket.front(), Some(timestamp) if *timestamp < cutoff) {
            bucket.pop_front();
        }
        if bucket.len() >= limit_per_minute as usize {
            return false;
        }
        bucket.push_back(now);
        true
    }
}

pub fn render_status(status: &SystemStatus) -> String {
    format!(
        "status={} uptime={}s version={}",
        status.status, status.uptime_seconds, status.version
    )
}

fn json_error(status: u16, error: &str, message: impl Into<Option<String>>) -> (u16, String) {
    (
        status,
        serde_json::to_string(&ControlPlaneErrorResponse {
            error: error.to_string(),
            message: message.into(),
        })
        .unwrap_or_else(|_| {
            "{\"error\":\"serialization_failed\",\"message\":\"failed to serialize control-plane error\"}".to_string()
        }),
    )
}

fn status_text(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn submit_intent_error_response(err: io::Error, deployment_id: &str) -> (u16, String) {
    match err.kind() {
        io::ErrorKind::NotFound => json_error(
            404,
            "deployment_not_found",
            Some(format!("deployment `{deployment_id}` was not found")),
        ),
        io::ErrorKind::InvalidInput => json_error(400, "invalid_request", Some(err.to_string())),
        io::ErrorKind::InvalidData => {
            json_error(503, "live_execution_misconfigured", Some(err.to_string()))
        }
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::TimedOut
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe => {
            json_error(503, "live_execution_unavailable", Some(err.to_string()))
        }
        _ => json_error(500, "submit_failed", Some(err.to_string())),
    }
}

fn claim_action_error_response(err: io::Error, account_id: &str) -> (u16, String) {
    match err.kind() {
        io::ErrorKind::NotFound => json_error(
            404,
            "account_not_found",
            Some(format!("account `{account_id}` was not found")),
        ),
        io::ErrorKind::InvalidInput => json_error(400, "invalid_request", Some(err.to_string())),
        io::ErrorKind::InvalidData => {
            json_error(503, "claim_gateway_misconfigured", Some(err.to_string()))
        }
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::TimedOut
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe => {
            json_error(503, "claim_gateway_unavailable", Some(err.to_string()))
        }
        _ => json_error(500, "claim_action_failed", Some(err.to_string())),
    }
}

fn account_claim_action_account_id<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    let account_id = path
        .trim_start_matches("/api/accounts/")
        .trim_end_matches(suffix)
        .trim_end_matches('/');
    if account_id.is_empty() {
        None
    } else {
        Some(account_id)
    }
}

#[cfg(test)]
pub fn route_request(path: &str, runtime_root: &Path) -> (u16, String) {
    let target = match path {
        "/health" | "/api/system/status" => runtime_root.join("system-status.json"),
        "/api/deployments" => runtime_root.join("deployments.json"),
        "/api/trading/state" => runtime_root.join("trading-state.json"),
        _ => {
            return json_error(404, "not_found", None);
        }
    };

    match fs::read_to_string(target) {
        Ok(body) => (200, body),
        Err(_) => json_error(503, "snapshot_unavailable", None),
    }
}

#[cfg(test)]
pub fn handle_api_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    config: &PlatformConfig,
) -> (u16, String) {
    match (method, path) {
        ("GET", "/health")
        | ("GET", "/api/system/status")
        | ("GET", "/api/deployments")
        | ("GET", "/api/trading/state") => route_request(path, &config.runtime_root),
        ("GET", "/api/accounts/claims") => match PloyDaemon::boot(config) {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.claim_statuses())
                    .unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(err) => json_error(500, "claim_status_failed", Some(err.to_string())),
        },
        ("GET", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims") => {
            let account_id = path
                .trim_start_matches("/api/accounts/")
                .trim_end_matches("/claims")
                .trim_end_matches('/');
            match PloyDaemon::boot(config)
                .ok()
                .and_then(|daemon| daemon.inspect_account_claims(account_id))
            {
                Some(detail) => (
                    200,
                    serde_json::to_string(&detail).unwrap_or_else(|_| "{}".to_string()),
                ),
                None => json_error(
                    404,
                    "account_not_found",
                    Some(format!("account `{account_id}` was not found")),
                ),
            }
        }
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            match PloyDaemon::boot(config)
                .ok()
                .and_then(|daemon| daemon.inspect_deployment(deployment_id))
            {
                Some(record) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                None => (404, "{\"error\":\"deployment_not_found\"}".to_string()),
            }
        }
        ("PUT", _) if path.starts_with("/api/deployments/") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: DeploymentApplyRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            if request.deployment_id != deployment_id {
                return (400, "{\"error\":\"deployment_id_mismatch\"}".to_string());
            }
            match PloyDaemon::boot(config).and_then(|mut daemon| daemon.apply_deployment(request)) {
                Ok(record) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(_) => (500, "{\"error\":\"apply_failed\"}".to_string()),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/control") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/control")
                .trim_end_matches('/');
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: DeploymentControlRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            match PloyDaemon::boot(config)
                .and_then(|mut daemon| daemon.control_deployment(deployment_id, request))
            {
                Ok(Some(record)) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                Ok(None) => (404, "{\"error\":\"deployment_not_found\"}".to_string()),
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => (
                    400,
                    format!("{{\"error\":\"invalid_request\",\"message\":\"{}\"}}", err),
                ),
                Err(_) => (500, "{\"error\":\"control_failed\"}".to_string()),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/intents") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/intents")
                .trim_end_matches('/');
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: PaperIntentRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            let side = match trade_side_from_wire(&request.side) {
                Ok(side) => side,
                Err(error) => return json_error(400, &error.error, error.message),
            };

            match PloyDaemon::boot(config) {
                Ok(mut daemon) => {
                    let response = daemon.submit_intent(TradingIntent {
                        intent_id: next_paper_intent_id(deployment_id),
                        deployment_id: deployment_id.to_string(),
                        market_id: request.market_id,
                        token_id: request.token_id,
                        side,
                        quantity: request.quantity,
                        limit_price: request.limit_price,
                        purpose: intent_purpose_from_wire(request.purpose),
                        created_at: chrono::Utc::now(),
                    });
                    match daemon.write_runtime_snapshots() {
                        Ok(()) => {}
                        Err(err) => {
                            return json_error(500, "snapshot_write_failed", Some(err.to_string()));
                        }
                    }
                    match response {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(err) => submit_intent_error_response(err, deployment_id),
            }
        }
        _ => (404, "{\"error\":\"not_found\"}".to_string()),
    }
}

pub fn spawn_server(state: Arc<AppState>) -> io::Result<thread::JoinHandle<()>> {
    let listen_addr = state
        .daemon
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?
        .config
        .listen_addr
        .clone();
    let listener = TcpListener::bind(&listen_addr)?;
    Ok(thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = state.clone();
            thread::spawn(move || {
                let _ = handle_connection(stream, &state);
            });
        }
    }))
}

fn handle_connection(mut stream: TcpStream, state: &Arc<AppState>) -> io::Result<()> {
    let mut request = [0_u8; 2048];
    let bytes = stream.read(&mut request)?;
    if bytes == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&request[..bytes]);
    let mut request_line = request.lines().next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("GET");
    let path = request_line.next().unwrap_or("/");
    let client_addr = stream.peer_addr().ok().map(|addr| addr.to_string());
    let (configured_token, sidecar_token, cookie_secret) = match configured_auth(state) {
        Ok(auth) => auth,
        Err(response) => return write_json_response(stream, response),
    };
    let auth_level = request_auth_level(
        &request,
        configured_token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(|token| token.as_str()),
        sidecar_token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(|token| token.as_str()),
        cookie_secret.expose_secret(),
    );
    let required_access = required_access(method, path);
    if let Some(response) =
        rate_limit_response(method, path, client_addr.as_deref(), auth_level, state)
    {
        audit_request(
            state,
            method,
            path,
            client_addr.as_deref(),
            auth_level,
            required_access,
            response.0,
            "rate_limited",
            response_message(&response.1),
        );
        return write_json_response(stream, response);
    }
    if method == "GET" && path == "/api/events/stream" {
        if !access_allowed(
            auth_level,
            required_access,
            configured_token.is_some(),
            sidecar_token.is_some(),
        ) {
            let response = auth_error_response(
                required_access,
                configured_token.is_some(),
                sidecar_token.is_some(),
            );
            audit_request(
                state,
                method,
                path,
                client_addr.as_deref(),
                auth_level,
                required_access,
                response.0,
                "denied",
                response_message(&response.1),
            );
            return write_json_response(stream, response);
        }
        audit_request(
            state,
            method,
            path,
            client_addr.as_deref(),
            auth_level,
            required_access,
            200,
            "allowed",
            None,
        );
        return handle_event_stream(stream, state);
    }
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .filter(|body| !body.is_empty());
    let response = handle_authenticated_runtime_request(
        method,
        path,
        body,
        auth_level,
        configured_token.is_some(),
        sidecar_token.is_some(),
        state,
    );
    let headers = response_headers(
        method,
        path,
        response.0,
        configured_token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(|token| token.as_str()),
        cookie_secret.expose_secret(),
    );
    audit_request(
        state,
        method,
        path,
        client_addr.as_deref(),
        auth_level,
        required_access,
        response.0,
        if response.0 < 400 {
            "allowed"
        } else {
            "denied"
        },
        response_message(&response.1),
    );
    write_json_response_with_headers(stream, response, &headers)
}

fn write_json_response(stream: TcpStream, response: (u16, String)) -> io::Result<()> {
    write_json_response_with_headers(stream, response, &[])
}

fn write_json_response_with_headers(
    mut stream: TcpStream,
    response: (u16, String),
    headers: &[(String, String)],
) -> io::Result<()> {
    let (status_code, body) = response;
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{}\
         \r\n{}",
        status_code,
        status_text(status_code),
        body.len(),
        extra_headers,
        body
    )
}

fn configured_auth(
    state: &Arc<AppState>,
) -> Result<(Option<SecretString>, Option<SecretString>, SecretString), (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| {
            (
                daemon.config.admin_token.clone(),
                daemon.config.sidecar_token.clone(),
                daemon.config.auth_cookie_secret.clone(),
            )
        })
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn request_rate_limiter() -> &'static Mutex<RateLimiter> {
    REQUEST_RATE_LIMITER.get_or_init(|| Mutex::new(RateLimiter::default()))
}

fn request_rate_limit_per_minute(state: &Arc<AppState>) -> Result<u32, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.request_rate_limit_per_minute)
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn rate_limit_response(
    method: &str,
    path: &str,
    client_addr: Option<&str>,
    auth_level: AuthLevel,
    state: &Arc<AppState>,
) -> Option<(u16, String)> {
    if path == "/health" {
        return None;
    }
    let limit = match request_rate_limit_per_minute(state) {
        Ok(limit) => limit,
        Err(response) => return Some(response),
    };
    let key = format!(
        "{}|{}|{}|{}",
        client_addr.unwrap_or("unknown"),
        auth_level_name(auth_level),
        method,
        path
    );
    let allowed = request_rate_limiter()
        .lock()
        .map(|mut limiter| limiter.allow(&key, limit))
        .unwrap_or(true);
    if allowed {
        None
    } else {
        Some(json_error(
            429,
            "rate_limited",
            Some(format!(
                "request rate limit exceeded for client `{}`",
                client_addr.unwrap_or("unknown")
            )),
        ))
    }
}

fn required_access(method: &str, path: &str) -> RequiredAccess {
    match (method, path) {
        ("GET", "/health")
        | ("GET", "/auth/session")
        | ("POST", "/auth/login")
        | ("POST", "/auth/logout") => RequiredAccess::Public,
        ("GET", "/api/system/status")
        | ("GET", "/api/deployments")
        | ("GET", "/api/trading/state")
        | ("GET", "/api/accounts/claims")
        | ("GET", "/api/events/stream") => RequiredAccess::ReadOnly,
        ("GET", "/api/audit/logs") => RequiredAccess::Admin,
        ("GET", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims") => {
            RequiredAccess::ReadOnly
        }
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            RequiredAccess::ReadOnly
        }
        _ => RequiredAccess::Admin,
    }
}

fn auth_level_name(auth_level: AuthLevel) -> &'static str {
    match auth_level {
        AuthLevel::None => "none",
        AuthLevel::Sidecar => "sidecar",
        AuthLevel::Admin => "admin",
    }
}

fn required_access_name(required_access: RequiredAccess) -> &'static str {
    match required_access {
        RequiredAccess::Public => "public",
        RequiredAccess::ReadOnly => "read_only",
        RequiredAccess::Admin => "admin",
    }
}

fn response_message(body: &str) -> Option<String> {
    serde_json::from_str::<ControlPlaneErrorResponse>(body)
        .ok()
        .and_then(|error| error.message.or(Some(error.error)))
}

fn audit_log_path(state: &Arc<AppState>) -> Result<PathBuf, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.audit_log_file.clone())
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn audit_request(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    client_addr: Option<&str>,
    auth_level: AuthLevel,
    required_access: RequiredAccess,
    status_code: u16,
    outcome: &str,
    message: Option<String>,
) {
    if path == "/health" {
        return;
    }

    let Ok(audit_log_file) = audit_log_path(state) else {
        return;
    };
    let entry = AuditLogEntry {
        timestamp: Utc::now(),
        method: method.to_string(),
        path: path.to_string(),
        client_addr: client_addr.map(str::to_string),
        auth_level: auth_level_name(auth_level).to_string(),
        required_access: required_access_name(required_access).to_string(),
        status_code,
        outcome: outcome.to_string(),
        message,
    };
    let _ = append_audit_entry(&audit_log_file, &entry);
}

fn append_audit_entry(path: &PathBuf, entry: &AuditLogEntry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let body = serde_json::to_string(entry)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    writeln!(file, "{body}")
}

fn read_recent_audit_entries(path: &PathBuf, limit: usize) -> io::Result<Vec<AuditLogEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let mut entries = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<AuditLogEntry>(line).ok())
        .collect::<Vec<_>>();
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }
    Ok(entries)
}

fn extract_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            if header_name.eq_ignore_ascii_case(name) {
                Some(value.trim())
            } else {
                None
            }
        })
}

fn extract_cookie<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    let header = extract_header(request, "Cookie")?;
    header.split(';').find_map(|entry| {
        let (cookie_name, value) = entry.trim().split_once('=')?;
        if cookie_name == name {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn request_auth_level(
    request: &str,
    admin_token: Option<&str>,
    sidecar_token: Option<&str>,
    cookie_secret: &str,
) -> AuthLevel {
    if let Some(expected_token) = admin_token {
        if let Some(header) = extract_header(request, "Authorization") {
            if let Some(token) = header.strip_prefix("Bearer ") {
                if token.trim() == expected_token {
                    return AuthLevel::Admin;
                }
            }
        }

        if extract_header(request, "x-ploy-admin-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Admin;
        }

        if extract_cookie(request, ADMIN_SESSION_COOKIE_NAME)
            .map(|token| cookie_matches(token, expected_token, cookie_secret))
            .unwrap_or(false)
        {
            return AuthLevel::Admin;
        }
    }

    if let Some(expected_token) = sidecar_token {
        if extract_header(request, "x-ploy-sidecar-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Sidecar;
        }
    }

    AuthLevel::None
}

fn access_allowed(
    auth_level: AuthLevel,
    required_access: RequiredAccess,
    admin_configured: bool,
    sidecar_configured: bool,
) -> bool {
    if !admin_configured && !sidecar_configured {
        return true;
    }

    match required_access {
        RequiredAccess::Public => true,
        RequiredAccess::ReadOnly => matches!(auth_level, AuthLevel::Admin | AuthLevel::Sidecar),
        RequiredAccess::Admin => auth_level == AuthLevel::Admin,
    }
}

fn auth_error_response(
    required_access: RequiredAccess,
    admin_configured: bool,
    sidecar_configured: bool,
) -> (u16, String) {
    let message = match required_access {
        RequiredAccess::Public => return (200, "{}".to_string()),
        RequiredAccess::ReadOnly if sidecar_configured && !admin_configured => {
            "control-plane sidecar or admin token is required".to_string()
        }
        RequiredAccess::ReadOnly => "control-plane admin or sidecar token is required".to_string(),
        RequiredAccess::Admin => "control-plane admin token is required".to_string(),
    };
    json_error(401, "unauthorized", Some(message))
}

fn response_headers(
    method: &str,
    path: &str,
    status_code: u16,
    configured_token: Option<&str>,
    cookie_secret: &str,
) -> Vec<(String, String)> {
    if status_code != 200 {
        return Vec::new();
    }

    match (method, path, configured_token) {
        ("POST", "/auth/login", Some(token)) => {
            vec![(
                "Set-Cookie".to_string(),
                admin_session_cookie(token, cookie_secret),
            )]
        }
        ("POST", "/auth/logout", _) => vec![(
            "Set-Cookie".to_string(),
            clear_admin_session_cookie().to_string(),
        )],
        _ => Vec::new(),
    }
}

fn admin_session_cookie(token: &str, cookie_secret: &str) -> String {
    let signature = sign_admin_session(token, cookie_secret);
    format!("{ADMIN_SESSION_COOKIE_NAME}=v1.{signature}; HttpOnly; Path=/; SameSite=Strict")
}

fn clear_admin_session_cookie() -> &'static str {
    "ploy_admin_session=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0"
}

fn sign_admin_session(token: &str, cookie_secret: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(cookie_secret.as_bytes()).expect("hmac accepts arbitrary keys");
    mac.update(token.as_bytes());
    bytes_to_hex(&mac.finalize().into_bytes())
}

fn cookie_matches(cookie_value: &str, expected_token: &str, cookie_secret: &str) -> bool {
    cookie_value
        .strip_prefix("v1.")
        .map(|signature| signature == sign_admin_session(expected_token, cookie_secret))
        .unwrap_or(false)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn handle_authenticated_runtime_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    auth_level: AuthLevel,
    admin_configured: bool,
    sidecar_configured: bool,
    state: &Arc<AppState>,
) -> (u16, String) {
    let required_access = required_access(method, path);
    match (method, path) {
        ("GET", "/auth/session") => (
            200,
            serde_json::json!({
                "authenticated": auth_level == AuthLevel::Admin,
                "auth_required": admin_configured || sidecar_configured,
                "sidecar_authenticated": auth_level == AuthLevel::Sidecar,
            })
            .to_string(),
        ),
        ("POST", "/auth/login") => {
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let provided = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| {
                    value
                        .get("admin_token")
                        .and_then(|token| token.as_str())
                        .map(str::to_string)
                });
            if !admin_configured {
                return (200, serde_json::json!({ "success": true }).to_string());
            }

            match state.daemon.lock() {
                Ok(daemon) => match daemon
                    .config
                    .admin_token
                    .as_ref()
                    .map(ExposeSecret::expose_secret)
                {
                    Some(expected) if provided.as_deref() == Some(expected) => {
                        (200, serde_json::json!({ "success": true }).to_string())
                    }
                    Some(_) => json_error(
                        401,
                        "invalid_credentials",
                        Some(
                            "admin token did not match configured control-plane token".to_string(),
                        ),
                    ),
                    None => (200, serde_json::json!({ "success": true }).to_string()),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", "/auth/logout") => (200, serde_json::json!({ "success": true }).to_string()),
        _ if !access_allowed(
            auth_level,
            required_access,
            admin_configured,
            sidecar_configured,
        ) =>
        {
            auth_error_response(required_access, admin_configured, sidecar_configured)
        }
        _ => handle_runtime_request(method, path, body, state),
    }
}

fn handle_runtime_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    state: &Arc<AppState>,
) -> (u16, String) {
    match (method, path) {
        ("GET", "/health") | ("GET", "/api/system/status") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.control_plane.system.status())
                    .unwrap_or_else(|_| "{}".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/deployments") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.control_plane.deployments.summaries())
                    .unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/trading/state") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.trading_state()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/accounts/claims") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.claim_statuses())
                    .unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/audit/logs") => match audit_log_path(state)
            .map_err(|response| response)
            .and_then(|path| {
                read_recent_audit_entries(&path, AUDIT_LOG_TAIL_LIMIT)
                    .map_err(|err| json_error(500, "audit_log_unavailable", Some(err.to_string())))
            }) {
            Ok(entries) => (
                200,
                serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(response) => response,
        },
        ("GET", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims") => {
            let Some(account_id) = account_claim_action_account_id(path, "/claims") else {
                return json_error(404, "not_found", None);
            };
            match state.daemon.lock() {
                Ok(daemon) => match daemon.inspect_account_claims(account_id) {
                    Some(detail) => (
                        200,
                        serde_json::to_string(&detail).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    None => claim_action_error_response(
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("account `{account_id}` was not found"),
                        ),
                        account_id,
                    ),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            match state.daemon.lock() {
                Ok(daemon) => match daemon.inspect_deployment(deployment_id) {
                    Some(record) => (
                        200,
                        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    None => json_error(
                        404,
                        "deployment_not_found",
                        Some(format!("deployment `{deployment_id}` was not found")),
                    ),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("PUT", _) if path.starts_with("/api/deployments/") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: DeploymentApplyRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            if request.deployment_id != deployment_id {
                return json_error(
                    400,
                    "deployment_id_mismatch",
                    Some(format!(
                        "request deployment_id `{}` did not match path `{deployment_id}`",
                        request.deployment_id
                    )),
                );
            }
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon.apply_deployment(request).and_then(|record| {
                        daemon.write_runtime_snapshots()?;
                        publish_snapshot_events(&daemon, &state.events);
                        Ok(record)
                    }) {
                        Ok(record) => (
                            200,
                            serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => json_error(500, "apply_failed", Some(err.to_string())),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/control") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/control")
                .trim_end_matches('/');
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: DeploymentControlRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .control_deployment(deployment_id, request)
                        .and_then(|record| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(record)
                        }) {
                        Ok(Some(record)) => (
                            200,
                            serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "deployment_not_found",
                            Some(format!("deployment `{deployment_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) => json_error(500, "control_failed", Some(err.to_string())),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims/run") => {
            let Some(account_id) = account_claim_action_account_id(path, "/claims/run") else {
                return json_error(404, "not_found", None);
            };
            match state.daemon.lock() {
                Ok(mut daemon) => match daemon.run_account_claims(account_id).and_then(|response| {
                    daemon.write_runtime_snapshots()?;
                    publish_snapshot_events(&daemon, &state.events);
                    Ok(response)
                }) {
                    Ok(response) => (
                        200,
                        serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    Err(err) => claim_action_error_response(err, account_id),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims/rescan") => {
            let Some(account_id) = account_claim_action_account_id(path, "/claims/rescan") else {
                return json_error(404, "not_found", None);
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .rescan_account_claims(account_id)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => claim_action_error_response(err, account_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims/pause") => {
            let Some(account_id) = account_claim_action_account_id(path, "/claims/pause") else {
                return json_error(404, "not_found", None);
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .set_account_claim_enabled(account_id, false)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => claim_action_error_response(err, account_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/accounts/") && path.ends_with("/claims/resume") => {
            let Some(account_id) = account_claim_action_account_id(path, "/claims/resume") else {
                return json_error(404, "not_found", None);
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .set_account_claim_enabled(account_id, true)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => claim_action_error_response(err, account_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/cancel") => {
            let suffix = path.trim_start_matches("/api/deployments/");
            let Some((deployment_id, order_suffix)) = suffix.split_once("/orders/") else {
                return json_error(404, "not_found", None);
            };
            let order_id = order_suffix
                .trim_end_matches("/cancel")
                .trim_end_matches('/');
            if order_id.is_empty() {
                return json_error(404, "not_found", None);
            }

            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .cancel_order(deployment_id, order_id)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/replace") => {
            let suffix = path.trim_start_matches("/api/deployments/");
            let Some((deployment_id, order_suffix)) = suffix.split_once("/orders/") else {
                return json_error(404, "not_found", None);
            };
            let order_id = order_suffix
                .trim_end_matches("/replace")
                .trim_end_matches('/');
            if order_id.is_empty() {
                return json_error(404, "not_found", None);
            }
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: OrderReplaceRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };

            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .replace_order(deployment_id, order_id, request)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/intents") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/intents")
                .trim_end_matches('/');
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: PaperIntentRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            let side = match trade_side_from_wire(&request.side) {
                Ok(side) => side,
                Err(error) => return json_error(400, &error.error, error.message),
            };

            match state.daemon.lock() {
                Ok(mut daemon) => {
                    let response = daemon.submit_intent(TradingIntent {
                        intent_id: next_paper_intent_id(deployment_id),
                        deployment_id: deployment_id.to_string(),
                        market_id: request.market_id,
                        token_id: request.token_id,
                        side,
                        quantity: request.quantity,
                        limit_price: request.limit_price,
                        purpose: intent_purpose_from_wire(request.purpose),
                        created_at: chrono::Utc::now(),
                    });
                    match daemon.write_runtime_snapshots() {
                        Ok(()) => {
                            publish_snapshot_events(&daemon, &state.events);
                        }
                        Err(err) => {
                            return json_error(500, "snapshot_write_failed", Some(err.to_string()));
                        }
                    }
                    match response {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        _ => json_error(404, "not_found", None),
    }
}

fn trade_side_from_wire(side: &str) -> Result<TradeSide, ControlPlaneErrorResponse> {
    match side {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        _ => Err(ControlPlaneErrorResponse {
            error: "invalid_side".to_string(),
            message: Some(format!("unsupported side `{side}`")),
        }),
    }
}

fn intent_purpose_from_wire(purpose: IntentPurpose) -> ploy_trading::IntentPurpose {
    match purpose {
        IntentPurpose::Entry => ploy_trading::IntentPurpose::Entry,
        IntentPurpose::Exit => ploy_trading::IntentPurpose::Exit,
        IntentPurpose::Reduce => ploy_trading::IntentPurpose::Reduce,
        IntentPurpose::Hedge => ploy_trading::IntentPurpose::Hedge,
        IntentPurpose::Cancel => ploy_trading::IntentPurpose::Cancel,
    }
}

pub fn snapshot_events(daemon: &PloyDaemon) -> Vec<OperatorEvent> {
    let system = daemon.control_plane.system.status();
    vec![
        OperatorEvent::Status(StatusUpdate {
            status: system.status.clone(),
        }),
        OperatorEvent::SystemSnapshot(SystemSnapshotEvent { system }),
        OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent {
            deployments: daemon.control_plane.deployments.summaries(),
        }),
        OperatorEvent::TradingSnapshot(TradingSnapshotEvent {
            trading: daemon.trading_state(),
        }),
    ]
}

pub fn publish_snapshot_events(daemon: &PloyDaemon, broker: &EventBroker) {
    for event in snapshot_events(daemon) {
        broker.publish(event);
    }
}

fn handle_event_stream(mut stream: TcpStream, state: &Arc<AppState>) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
    )?;

    if let Ok(daemon) = state.daemon.lock() {
        for event in snapshot_events(&daemon) {
            write_sse_event(&mut stream, &event)?;
        }
    }

    let receiver = state.events.subscribe();
    loop {
        match receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(event) => write_sse_event(&mut stream, &event)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                write!(stream, ": keep-alive\n\n")?;
                stream.flush()?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn write_sse_event(stream: &mut TcpStream, event: &OperatorEvent) -> io::Result<()> {
    let body = serde_json::to_string(event)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    write!(stream, "data: {body}\n\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        admin_session_cookie, append_audit_entry, handle_api_request,
        handle_authenticated_runtime_request, handle_runtime_request, request_auth_level,
        response_headers, route_request, snapshot_events, AppState, AuthLevel, RateLimiter,
        ADMIN_SESSION_COOKIE_NAME,
    };
    use crate::events::EventBroker;
    use chrono::Utc;
    use ploy_connectivity::{CancellationOutcome, ReplaceOutcome, StaticExecutionGateway};
    use ploy_operator_contracts::AuditLogEntry;
    use ploy_operator_contracts::{OrderReplaceRequest, PaperIntentRequest};
    use ploy_trading::{IntentPurpose as TradingIntentPurpose, TradeSide, TradingIntent};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployd-http-{label}-{unique}"))
    }

    #[test]
    fn route_request_serves_system_and_deployment_snapshots() {
        let runtime_root = temp_dir("routes");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "running",
                "uptime_seconds": 7,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null
            })
            .to_string(),
        )
        .expect("write status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let (status_code, status_body) = route_request("/api/system/status", &runtime_root);
        assert_eq!(status_code, 200);
        assert!(status_body.contains("\"status\":\"running\""));

        let (deployments_code, deployments_body) = route_request("/api/deployments", &runtime_root);
        assert_eq!(deployments_code, 200);
        assert!(deployments_body.contains("\"deployment_id\":\"example.paper\""));
    }

    #[test]
    fn rate_limiter_denies_requests_after_limit_within_window() {
        let mut limiter = RateLimiter::default();
        assert!(limiter.allow("127.0.0.1|admin|GET|/api/deployments", 1));
        assert!(!limiter.allow("127.0.0.1|admin|GET|/api/deployments", 1));
    }

    #[test]
    fn handle_runtime_request_reads_recent_audit_entries() {
        let root = temp_dir("audit-read");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let audit_log_file = runtime_root.join("audit-log.jsonl");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            audit_log_file: audit_log_file.clone(),
            ..crate::config::PlatformConfig::default()
        };
        append_audit_entry(
            &audit_log_file,
            &AuditLogEntry {
                timestamp: Utc::now(),
                method: "POST".to_string(),
                path: "/api/deployments/example.paper/control".to_string(),
                client_addr: Some("127.0.0.1:9000".to_string()),
                auth_level: "admin".to_string(),
                required_access: "admin".to_string(),
                status_code: 200,
                outcome: "allowed".to_string(),
                message: Some("deployment paused".to_string()),
            },
        )
        .expect("append audit");

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) = handle_runtime_request("GET", "/api/audit/logs", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("/api/deployments/example.paper/control"));
        assert!(body.contains("deployment paused"));
    }

    #[test]
    fn auth_session_reports_auth_requirement_when_admin_token_is_configured() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "GET",
            "/auth/session",
            None,
            AuthLevel::None,
            true,
            false,
            &state,
        );

        assert_eq!(code, 200);
        assert!(body.contains("\"auth_required\":true"));
        assert!(body.contains("\"authenticated\":false"));
    }

    #[test]
    fn unauthorized_requests_are_rejected_when_admin_token_is_configured() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::None,
            true,
            false,
            &state,
        );

        assert_eq!(code, 401);
        assert!(body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn auth_login_accepts_matching_admin_token() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "POST",
            "/auth/login",
            Some("{\"admin_token\":\"secret-token\"}"),
            AuthLevel::None,
            true,
            false,
            &state,
        );

        assert_eq!(code, 200);
        assert!(body.contains("\"success\":true"));
    }

    #[test]
    fn request_auth_level_accepts_admin_session_cookie() {
        let cookie = admin_session_cookie("secret-token", "cookie-secret");
        let request = format!(
            "GET /api/events/stream HTTP/1.1\r\nHost: 127.0.0.1:8081\r\nCookie: {cookie}; theme=dark\r\n\r\n"
        );
        assert_eq!(
            request_auth_level(&request, Some("secret-token"), None, "cookie-secret"),
            AuthLevel::Admin
        );
    }

    #[test]
    fn auth_responses_emit_session_cookie_headers() {
        let login_headers = response_headers(
            "POST",
            "/auth/login",
            200,
            Some("secret-token"),
            "cookie-secret",
        );
        assert!(login_headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie"
                && value.contains(&format!("{ADMIN_SESSION_COOKIE_NAME}=v1."))
                && !value.contains("secret-token")));

        let logout_headers = response_headers(
            "POST",
            "/auth/logout",
            200,
            Some("secret-token"),
            "cookie-secret",
        );
        assert!(logout_headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie"
                && value.contains(&format!("{ADMIN_SESSION_COOKIE_NAME}="))
                && value.contains("Max-Age=0")));
    }

    #[test]
    fn signed_session_cookie_does_not_authenticate_other_tokens() {
        let cookie = admin_session_cookie("secret-token", "cookie-secret");
        let request = format!("GET / HTTP/1.1\r\nCookie: {cookie}\r\n\r\n");
        assert_eq!(
            request_auth_level(&request, Some("other-token"), None, "cookie-secret"),
            AuthLevel::None
        );
    }

    #[test]
    fn sidecar_token_grants_read_only_access_but_not_admin_access() {
        let request =
            "GET /api/deployments HTTP/1.1\r\nx-ploy-sidecar-token: sidecar-secret\r\n\r\n";
        assert_eq!(
            request_auth_level(
                request,
                Some("admin-secret"),
                Some("sidecar-secret"),
                "cookie-secret"
            ),
            AuthLevel::Sidecar
        );

        let config = crate::config::PlatformConfig {
            admin_token: Some("admin-secret".to_string().into()),
            sidecar_token: Some("sidecar-secret".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (read_code, _) = handle_authenticated_runtime_request(
            "GET",
            "/api/deployments",
            None,
            AuthLevel::Sidecar,
            true,
            true,
            &state,
        );
        assert_eq!(read_code, 200);

        let (write_code, write_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::Sidecar,
            true,
            true,
            &state,
        );
        assert_eq!(write_code, 401);
        assert!(write_body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn handle_api_request_applies_and_controls_deployments() {
        let root = temp_dir("apply");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("empty registry");

        let config = crate::config::PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            ..crate::config::PlatformConfig::default()
        };

        let apply_body = serde_json::json!({
            "deployment_id": "example.paper",
            "bundle_id": "example",
            "runtime_mode": "paper",
            "desired_state": "running"
        })
        .to_string();
        let (apply_code, apply_response) = handle_api_request(
            "PUT",
            "/api/deployments/example.paper",
            Some(&apply_body),
            &config,
        );
        assert_eq!(apply_code, 200);
        assert!(apply_response.contains("\"deployment_id\":\"example.paper\""));

        let (inspect_code, inspect_response) =
            handle_api_request("GET", "/api/deployments/example.paper", None, &config);
        assert_eq!(inspect_code, 200);
        assert!(inspect_response.contains("\"bundle_id\":\"example\""));

        let control_body = serde_json::json!({
            "desired_state": "paused"
        })
        .to_string();
        let (control_code, control_response) = handle_api_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some(&control_body),
            &config,
        );
        assert_eq!(control_code, 200);
        assert!(control_response.contains("\"desired_state\":\"paused\""));
    }

    #[test]
    fn handle_api_request_submits_paper_intent() {
        let root = temp_dir("submit-intent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let body = serde_json::to_string(&PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_api_request(
            "POST",
            "/api/deployments/example.paper/intents",
            Some(&body),
            &config,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"deployment_id\":\"example.paper\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        assert!(trading_body.contains("\"deployment_id\": \"example.paper\""));
    }

    #[test]
    fn route_request_serves_trading_state_snapshot() {
        let runtime_root = temp_dir("trading-routes");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "runtime_mode": "paper",
                    "intents": [],
                    "orders": [],
                    "fills": [],
                    "positions": [],
                    "pnl": {
                        "realized_pnl": "0",
                        "unrealized_pnl": "0",
                        "total_fees": "0",
                        "net_pnl": "0"
                    },
                    "risk": {
                        "pending_intents": 0,
                        "active_orders": 0,
                        "open_positions": 0,
                        "gross_exposure": "0",
                        "reserved_order_exposure": "0",
                        "total_gross_exposure": "0"
                    }
                }
            ])
            .to_string(),
        )
        .expect("write trading state");

        let (status_code, body) = route_request("/api/trading/state", &runtime_root);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"deployment_id\":\"example.paper\""));
    }

    #[test]
    fn snapshot_events_include_control_plane_and_trading_payloads() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let events = snapshot_events(&daemon);
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::SystemSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::DeploymentSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::TradingSnapshot(_)
        )));
    }

    #[test]
    fn handle_runtime_request_submits_live_intent_via_shared_daemon_state() {
        let root = temp_dir("runtime-live-intent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-http-1")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let body = serde_json::to_string(&PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&body),
            &state,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"state\":\"acknowledged\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        assert!(trading_body.contains("\"deployment_id\": \"example.live\""));
        assert!(trading_body.contains("\"venue_order_id\": \"venue-live-http-1\""));
    }

    #[test]
    fn handle_runtime_request_surfaces_live_gateway_transport_failure_as_503() {
        let root = temp_dir("runtime-live-intent-transport-error");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::failed(
                ploy_connectivity::ExecutionError::Transport("gateway offline".to_string()),
            )),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let body = serde_json::to_string(&PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&body),
            &state,
        );
        assert_eq!(submit_code, 503);
        assert!(submit_response.contains("\"error\":\"live_execution_unavailable\""));
        assert!(submit_response.contains("gateway offline"));

        let trading_body =
            fs::read_to_string(root.join("run/platform/trading-state.json")).expect("snapshot");
        assert!(trading_body.contains("\"state\": \"pending\""));
        assert!(trading_body.contains("\"deployment_id\": \"example.live\""));
    }

    #[test]
    fn handle_runtime_request_cancels_live_order_and_persists_snapshot() {
        let root = temp_dir("runtime-live-cancel");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-http-cancel-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        let daemon =
            crate::runtime::PloyDaemon::boot_with_live_execution(&config, Box::new(gateway))
                .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let submit_body = serde_json::to_string(&PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&submit_body),
            &state,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"order_id\":\"order-"));

        let order_id = submit_response
            .split("\"order_id\":\"")
            .nth(1)
            .and_then(|suffix| suffix.split('"').next())
            .expect("order id");

        let (cancel_code, cancel_response) = handle_runtime_request(
            "POST",
            &format!("/api/deployments/example.live/orders/{order_id}/cancel"),
            None,
            &state,
        );
        assert_eq!(cancel_code, 200);
        assert!(cancel_response.contains("\"state\":\"canceled\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        assert!(trading_body.contains("\"state\": \"canceled\""));
        assert!(trading_body.contains("\"venue_order_id\": \"venue-live-http-cancel-1\""));
    }

    #[test]
    fn handle_runtime_request_replaces_live_order_and_persists_revision_history() {
        let root = temp_dir("runtime-live-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-http-replace-1")
            .with_replace_result(Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-http-replace-2".to_string(),
            }));
        let daemon =
            crate::runtime::PloyDaemon::boot_with_live_execution(&config, Box::new(gateway))
                .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let submit_body = serde_json::to_string(&PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(55, 2)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&submit_body),
            &state,
        );
        assert_eq!(submit_code, 200);

        let order_id = submit_response
            .split("\"order_id\":\"")
            .nth(1)
            .and_then(|suffix| suffix.split('"').next())
            .expect("order id");

        let replace_body = serde_json::to_string(&OrderReplaceRequest {
            quantity: rust_decimal::Decimal::new(250, 2),
            limit_price: Some(rust_decimal::Decimal::new(57, 2)),
        })
        .expect("replace body");
        let (replace_code, replace_response) = handle_runtime_request(
            "POST",
            &format!("/api/deployments/example.live/orders/{order_id}/replace"),
            Some(&replace_body),
            &state,
        );
        assert_eq!(replace_code, 200);
        assert!(replace_response.contains("\"revision\":1"));
        assert!(replace_response.contains("\"venue_order_id\":\"venue-live-http-replace-2\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        assert!(trading_body.contains("\"venue_order_history\": ["));
        assert!(trading_body.contains("venue-live-http-replace-1"));
        assert!(trading_body.contains("\"revision\": 1"));
    }

    #[test]
    fn handle_runtime_request_reads_trading_state_from_shared_daemon() {
        let root = temp_dir("runtime-live-read");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-http-2")),
        )
        .expect("boot daemon");
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-http-2".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: rust_decimal::Decimal::ONE,
                limit_price: Some(rust_decimal::Decimal::ONE),
                purpose: TradingIntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");

        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) = handle_runtime_request("GET", "/api/trading/state", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"deployment_id\":\"example.live\""));
        assert!(body.contains("\"venue_order_id\":\"venue-live-http-2\""));
    }

    #[test]
    fn handle_runtime_request_reports_structured_not_found_error() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/deployments/missing.paper", None, &state);
        assert_eq!(status_code, 404);
        assert!(body.contains("\"error\":\"deployment_not_found\""));
        assert!(body.contains("\"message\""));
        assert!(body.contains("missing.paper"));
    }

    #[test]
    fn handle_runtime_request_reports_poisoned_lock_as_503() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let poisoned = Arc::new(Mutex::new(daemon));
        let poison_handle = poisoned.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison_handle.lock().expect("lock daemon");
            panic!("poison daemon lock for test");
        })
        .join();

        let state = Arc::new(AppState {
            daemon: poisoned,
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/deployments/missing.paper", None, &state);
        assert_eq!(status_code, 503);
        assert!(body.contains("\"error\":\"daemon_lock_poisoned\""));
    }
}
