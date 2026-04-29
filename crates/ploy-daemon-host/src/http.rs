use crate::events::EventBroker;
use crate::runtime::{next_paper_intent_id, PloyDaemon};
use chrono::Utc;
use hmac::{Hmac, Mac};
use ploy_operator_contracts::{
    compute_oversight_report, AgentRunRecord, AlertSnapshotEvent, AuditLogEntry,
    ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest,
    DeploymentDiagnosticsReport, DeploymentSnapshotEvent, DiagnosticsEvidence, DiagnosticsFinding,
    DryRunPerformanceReport, IntentPurpose, MetricsSnapshotEvent, OperatorEvent,
    OrderReplaceRequest, OversightSnapshotEvent, PaperIntentRequest, PlatformDiagnosticsReport,
    ProposalCreateRequest, ProposalDecisionRequest, ProposalSnapshotEvent, StatusUpdate,
    SystemSnapshotEvent, SystemStatus, TradingSnapshotEvent,
};
use ploy_trading::{TradeSide, TradingIntent};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::config::PlatformConfig;

#[derive(Debug)]
pub struct AppState {
    pub daemon: Arc<Mutex<PloyDaemon>>,
    pub events: Arc<EventBroker>,
}

const ADMIN_SESSION_COOKIE_NAME: &str = "ploy_admin_session";
const AUDIT_LOG_TAIL_LIMIT: usize = 200;
const MAX_HTTP_REQUEST_BYTES: usize = 1024 * 1024;
type HmacSha256 = Hmac<Sha256>;
static REQUEST_RATE_LIMITER: OnceLock<Mutex<RateLimiter>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthLevel {
    None,
    Sidecar,
    Operator,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequiredAccess {
    Public,
    ReadOnly,
    Operator,
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

fn host_root_from_runtime_root(runtime_root: &Path) -> PathBuf {
    runtime_root
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn query_param(raw_path: &str, key: &str) -> Option<String> {
    let (_, query) = raw_path.split_once('?')?;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == key && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn build_strategy_report_html(state: &Arc<AppState>, since: Option<&str>) -> (u16, String) {
    let host_root = match state.daemon.lock() {
        Ok(daemon) => host_root_from_runtime_root(&daemon.config.runtime_root),
        Err(_) => return html_error(503, "daemon lock poisoned"),
    };

    let script_path = host_root.join("scripts/report_strategy.py");
    if !script_path.exists() {
        return html_error(
            500,
            &format!(
                "strategy report script not found: {}",
                script_path.display()
            ),
        );
    }

    let report_path = host_root.join("reports/strategy_report.html");
    if let Some(parent) = report_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            return html_error(500, &format!("failed to create report directory: {err}"));
        }
    }

    let mut command = Command::new("python3");
    command
        .arg(&script_path)
        .arg("--host")
        .arg("local")
        .current_dir(&host_root)
        .env("PLOY_RESEARCH_HOST", "local");
    if let Some(since) = since {
        command.arg("--since").arg(since);
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(err) => {
            return html_error(
                500,
                &format!("failed to start strategy report generator: {err}"),
            );
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("report generator exited with status {}", output.status)
        };
        return html_error(500, &message);
    }

    match fs::read_to_string(&report_path) {
        Ok(body) => (200, body),
        Err(err) => html_error(
            500,
            &format!(
                "strategy report was generated but could not be read from {}: {err}",
                report_path.display()
            ),
        ),
    }
}

fn build_market_data_health_json(state: &Arc<AppState>) -> (u16, String) {
    let host_root = match state.daemon.lock() {
        Ok(daemon) => host_root_from_runtime_root(&daemon.config.runtime_root),
        Err(_) => return json_error(503, "daemon_lock_poisoned", None),
    };

    let script_path = host_root.join("scripts/report_market_data_health.py");
    if !script_path.exists() {
        return json_error(
            500,
            "market_data_health_script_missing",
            Some(format!(
                "market data health script not found: {}",
                script_path.display()
            )),
        );
    }

    let output = match Command::new("python3")
        .arg(&script_path)
        .current_dir(&host_root)
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            return json_error(
                500,
                "market_data_health_script_failed",
                Some(format!("failed to start market data health script: {err}")),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!(
                "market data health script exited with status {}",
                output.status
            )
        };
        return json_error(500, "market_data_health_unavailable", Some(message));
    }

    let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if body.is_empty() {
        return json_error(
            500,
            "market_data_health_empty",
            Some("market data health script returned an empty response".to_string()),
        );
    }

    (200, body)
}

fn build_dry_run_summary_json(state: &Arc<AppState>) -> (u16, String) {
    let host_root = match state.daemon.lock() {
        Ok(daemon) => host_root_from_runtime_root(&daemon.config.runtime_root),
        Err(_) => return json_error(503, "daemon_lock_poisoned", None),
    };

    let script_path = host_root.join("scripts/report_dryrun_summary.py");
    if !script_path.exists() {
        return json_error(
            500,
            "dry_run_summary_script_missing",
            Some(format!(
                "dry-run summary script not found: {}",
                script_path.display()
            )),
        );
    }

    let output = match Command::new("python3")
        .arg(&script_path)
        .current_dir(&host_root)
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            return json_error(
                500,
                "dry_run_summary_script_failed",
                Some(format!("failed to start dry-run summary script: {err}")),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!(
                "dry-run summary script exited with status {}",
                output.status
            )
        };
        return json_error(500, "dry_run_summary_unavailable", Some(message));
    }

    let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if body.is_empty() {
        return json_error(
            500,
            "dry_run_summary_empty",
            Some("dry-run summary script returned an empty response".to_string()),
        );
    }

    let report: DryRunPerformanceReport = match serde_json::from_str(&body) {
        Ok(report) => report,
        Err(err) => {
            return json_error(
                500,
                "dry_run_summary_invalid",
                Some(format!(
                    "dry-run summary script returned payload outside the operator contract: {err}"
                )),
            );
        }
    };

    (
        200,
        serde_json::to_string(&report).unwrap_or_else(|_| body.to_string()),
    )
}

fn html_error(status_code: u16, message: &str) -> (u16, String) {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    (
        status_code,
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Strategy Report Error</title></head><body><h1>Strategy Report Error</h1><p>{escaped}</p></body></html>"
        ),
    )
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
    let request = read_http_request(&mut stream)?;
    if request.is_empty() {
        return Ok(());
    }

    let mut request_line = request.lines().next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("GET");
    let raw_path = request_line.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or(raw_path);
    let client_addr = stream.peer_addr().ok().map(|addr| addr.to_string());
    let (configured_token, operator_token, sidecar_token, cookie_secret) =
        match configured_auth(state) {
            Ok(auth) => auth,
            Err(response) => return write_json_response(stream, response),
        };
    let auth_level = request_auth_level(
        &request,
        configured_token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(|token| token.as_str()),
        operator_token
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
            operator_token.is_some(),
            sidecar_token.is_some(),
        ) {
            let response = auth_error_response(
                required_access,
                configured_token.is_some(),
                operator_token.is_some(),
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
    if method == "GET" && path == "/reports/strategy" {
        if !access_allowed(
            auth_level,
            required_access,
            configured_token.is_some(),
            operator_token.is_some(),
            sidecar_token.is_some(),
        ) {
            let response = auth_error_response(
                required_access,
                configured_token.is_some(),
                operator_token.is_some(),
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

        let response = build_strategy_report_html(state, query_param(raw_path, "since").as_deref());
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
            if response.0 < 400 {
                None
            } else {
                Some("strategy report generation failed".to_string())
            },
        );
        return write_html_response(stream, response);
    }
    let body = request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .filter(|body| !body.is_empty());
    let response = handle_authenticated_runtime_request(
        method,
        path,
        body,
        auth_level,
        configured_token.is_some(),
        operator_token.is_some(),
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

fn read_http_request(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];

    loop {
        let bytes = stream.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..bytes]);
        if request.len() > MAX_HTTP_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request exceeded maximum request size",
            ));
        }

        if let Some(header_end) = header_end_offset(&request) {
            let expected_body_bytes = content_length(&request[..header_end])?;
            if request.len() >= header_end + 4 + expected_body_bytes {
                break;
            }
            if expected_body_bytes == 0 {
                break;
            }
        }
    }

    Ok(String::from_utf8_lossy(&request).into_owned())
}

fn header_end_offset(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> io::Result<usize> {
    let headers = String::from_utf8_lossy(headers);
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let length = value.trim().parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid Content-Length header: {error}"),
                )
            })?;
            if length > MAX_HTTP_REQUEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP request body exceeded maximum request size",
                ));
            }
            return Ok(length);
        }
    }
    Ok(0)
}

fn write_json_response(stream: TcpStream, response: (u16, String)) -> io::Result<()> {
    write_json_response_with_headers(stream, response, &[])
}

fn write_html_response(stream: TcpStream, response: (u16, String)) -> io::Result<()> {
    write_response_with_headers(stream, response, "text/html; charset=utf-8", &[])
}

fn write_response_with_headers(
    mut stream: TcpStream,
    response: (u16, String),
    content_type: &str,
    headers: &[(String, String)],
) -> io::Result<()> {
    let (status_code, body) = response;
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n{}\
         \r\n{}",
        status_code,
        status_text(status_code),
        body.len(),
        content_type,
        extra_headers,
        body
    )
}

fn write_json_response_with_headers(
    stream: TcpStream,
    response: (u16, String),
    headers: &[(String, String)],
) -> io::Result<()> {
    write_response_with_headers(stream, response, "application/json", headers)
}

fn configured_auth(
    state: &Arc<AppState>,
) -> Result<
    (
        Option<SecretString>,
        Option<SecretString>,
        Option<SecretString>,
        SecretString,
    ),
    (u16, String),
> {
    state
        .daemon
        .lock()
        .map(|daemon| {
            (
                daemon.config.admin_token.clone(),
                daemon.config.operator_token.clone(),
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
        | ("GET", "/api/system/metrics")
        | ("GET", "/api/system/alerts")
        | ("GET", "/api/market-data/health")
        | ("GET", "/api/reports/dry-run")
        | ("GET", "/api/deployments")
        | ("GET", "/api/trading/state")
        | ("GET", "/reports/strategy")
        | ("GET", "/api/events/stream") => RequiredAccess::ReadOnly,
        ("GET", "/api/audit/logs") => RequiredAccess::Admin,
        ("GET", "/api/system/diagnose") => RequiredAccess::ReadOnly,
        ("GET", "/api/agent/runs") => RequiredAccess::ReadOnly,
        ("GET", _) if path.starts_with("/api/agent/runs/") => RequiredAccess::ReadOnly,
        ("GET", "/api/proposals") | ("POST", "/api/proposals") => RequiredAccess::ReadOnly,
        ("GET", _) if path.starts_with("/api/proposals/") => RequiredAccess::ReadOnly,
        ("POST", _) if path.starts_with("/api/proposals/") => RequiredAccess::Operator,
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            RequiredAccess::ReadOnly
        }
        ("GET", _) if path.starts_with("/api/trading/diagnose/") => RequiredAccess::ReadOnly,
        _ => RequiredAccess::Operator,
    }
}

fn auth_level_name(auth_level: AuthLevel) -> &'static str {
    match auth_level {
        AuthLevel::None => "none",
        AuthLevel::Sidecar => "sidecar",
        AuthLevel::Operator => "operator",
        AuthLevel::Admin => "admin",
    }
}

fn required_access_name(required_access: RequiredAccess) -> &'static str {
    match required_access {
        RequiredAccess::Public => "public",
        RequiredAccess::ReadOnly => "read_only",
        RequiredAccess::Operator => "operator",
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

fn agent_runs_path(state: &Arc<AppState>) -> Result<PathBuf, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.agent_runs_file.clone())
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn read_agent_runs(path: &PathBuf) -> io::Result<Vec<AgentRunRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let mut runs = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<AgentRunRecord>(line).ok())
        .collect::<Vec<_>>();
    runs.reverse();
    Ok(runs)
}

fn read_agent_run(path: &PathBuf, run_id: &str) -> io::Result<Option<AgentRunRecord>> {
    Ok(read_agent_runs(path)?
        .into_iter()
        .find(|run| run.run_id == run_id))
}

fn build_platform_diagnostics_report(
    daemon: &PloyDaemon,
    state: &Arc<AppState>,
) -> io::Result<PlatformDiagnosticsReport> {
    let system = daemon.control_plane.system.status();
    let metrics = daemon.platform_metrics();
    let alerts = daemon.active_alerts();
    let deployments = daemon.control_plane.deployments.summaries();
    let trading = daemon.trading_state();
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_entries = audit_log_path(state)
        .ok()
        .and_then(|path| read_recent_audit_entries(&path, 4).ok())
        .unwrap_or_default();
    let event_entries = recent_snapshot_evidence(daemon, 8, None);
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();

    for alert in alerts {
        let key = format!("{:?}:{}", alert.kind, alert.message);
        if seen.insert(key) {
            findings.push(DiagnosticsFinding {
                severity: format!("{:?}", alert.severity).to_lowercase(),
                kind: format!("{:?}", alert.kind).to_lowercase(),
                message: alert.message.clone(),
                first_observed_at: Some(alert.triggered_at.to_rfc3339()),
                likely_causes: likely_causes_from_kind(&format!("{:?}", alert.kind).to_lowercase()),
                operator_command: Some("ployctl system audit".to_string()),
                evidence: vec![DiagnosticsEvidence {
                    source: "system_alert".to_string(),
                    label: alert.source_id.clone(),
                    detail: format!("alert_id={}", alert.alert_id),
                    observed_at: Some(alert.triggered_at.to_rfc3339()),
                }],
            });
        }
    }

    for signal in oversight.signals {
        // Include deployment_id in the dedupe key so findings from different
        // deployments with the same kind/message are not collapsed into one.
        let key = format!(
            "{}:{}:{}",
            signal.kind,
            signal.message,
            signal.deployment_id.as_deref().unwrap_or("platform")
        );
        if seen.insert(key) {
            // Reuse the pre-built operator_command from recommended_actions so
            // the command shown here always matches what build_operator_command
            // produces (correct deployment id, config hints, etc.).
            let target = signal.deployment_id.as_deref().unwrap_or("platform");
            let operator_command = oversight
                .recommended_actions
                .iter()
                .find(|a| a.kind == signal.recommended_action && a.target == target)
                .map(|a| a.operator_command.clone())
                .or_else(|| {
                    // Fallback for signals that have no matching action entry.
                    Some(format!("ployctl system status"))
                });
            findings.push(DiagnosticsFinding {
                severity: signal.severity.clone(),
                kind: signal.kind.clone(),
                message: signal.message.clone(),
                first_observed_at: Some(oversight.timestamp.clone()),
                likely_causes: likely_causes_from_kind(&signal.kind),
                operator_command,
                evidence: signal
                    .evidence
                    .iter()
                    .map(|detail| DiagnosticsEvidence {
                        source: "oversight_signal".to_string(),
                        label: signal.recommended_action.clone(),
                        detail: detail.clone(),
                        observed_at: Some(oversight.timestamp.clone()),
                    })
                    .collect(),
            });
        }
    }

    if metrics.live_reconcile_failures > 0 {
        findings.push(DiagnosticsFinding {
            severity: "warning".to_string(),
            kind: "live_reconcile_failures".to_string(),
            message: format!(
                "live reconcile reported {} consecutive failures",
                metrics.live_reconcile_failures
            ),
            first_observed_at: metrics
                .last_live_reconcile_success_at
                .map(|value| value.to_rfc3339()),
            likely_causes: vec!["reconcile_loop_stalled".to_string()],
            operator_command: Some("ployctl system audit".to_string()),
            evidence: vec![DiagnosticsEvidence {
                source: "system_metrics".to_string(),
                label: "live_reconcile_failures".to_string(),
                detail: format!(
                    "live_reconcile_failures={}",
                    metrics.live_reconcile_failures
                ),
                observed_at: metrics
                    .last_live_reconcile_success_at
                    .map(|value| value.to_rfc3339()),
            }],
        });
    }

    let mut recent_evidence = audit_entries
        .iter()
        .map(audit_entry_to_evidence)
        .collect::<Vec<_>>();
    recent_evidence.extend(event_entries);
    recent_evidence.truncate(8);

    Ok(PlatformDiagnosticsReport {
        generated_at: Utc::now().to_rfc3339(),
        platform_status: system.status.clone(),
        first_diverged_metric: if system.active_alert_count > 0 {
            Some("active_alerts".to_string())
        } else if system.stale_source_count > 0 {
            Some("stale_sources".to_string())
        } else if system.error_count_1h > 0 {
            Some("error_count_1h".to_string())
        } else if metrics.live_reconcile_failures > 0 {
            Some("live_reconcile_failures".to_string())
        } else {
            findings.first().map(|finding| finding.kind.clone())
        },
        findings,
        recent_evidence,
    })
}

fn build_deployment_diagnostics_report(
    daemon: &PloyDaemon,
    state: &Arc<AppState>,
    deployment_id: &str,
) -> io::Result<DeploymentDiagnosticsReport> {
    let system = daemon.control_plane.system.status();
    let deployments = daemon.control_plane.deployments.summaries();
    let deployment = daemon.inspect_deployment(deployment_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("deployment `{deployment_id}` was not found"),
        )
    })?;
    let trading = daemon.trading_state();
    let state_snapshot = trading
        .iter()
        .find(|item| item.deployment_id == deployment_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("trading state for `{deployment_id}` was not found"),
            )
        })?;
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_entries = audit_log_path(state)
        .ok()
        .and_then(|path| read_recent_audit_entries(&path, 8).ok())
        .unwrap_or_default();
    let mut recent_evidence = vec![DiagnosticsEvidence {
        source: "current_snapshot".to_string(),
        label: "trading_state".to_string(),
        detail: format!(
            "pending_intents={} active_orders={} open_positions={} total_gross_exposure={} net_pnl={}",
            state_snapshot.risk.pending_intents,
            state_snapshot.risk.active_orders,
            state_snapshot.risk.open_positions,
            state_snapshot.risk.total_gross_exposure,
            state_snapshot.pnl.net_pnl,
        ),
        observed_at: Some(oversight.timestamp.clone()),
    }];
    recent_evidence.extend(
        audit_entries
            .iter()
            .filter(|entry| {
                entry.path.contains(deployment_id)
                    || entry
                        .message
                        .as_deref()
                        .map(|message| message.contains(deployment_id))
                        .unwrap_or(false)
            })
            .map(audit_entry_to_evidence),
    );
    recent_evidence.extend(recent_snapshot_evidence(daemon, 12, Some(deployment_id)));
    recent_evidence.truncate(8);

    let mut findings = Vec::new();
    for signal in oversight
        .signals
        .iter()
        .filter(|signal| signal.deployment_id.as_deref() == Some(deployment_id))
    {
        let action = oversight.recommended_actions.iter().find(|action| {
            action.target == deployment_id && action.kind == signal.recommended_action
        });
        findings.push(DiagnosticsFinding {
            severity: signal.severity.clone(),
            kind: signal.kind.clone(),
            message: signal.message.clone(),
            first_observed_at: Some(oversight.timestamp.clone()),
            likely_causes: likely_causes_from_kind(&signal.kind),
            operator_command: action.map(|item| item.operator_command.clone()),
            evidence: signal
                .evidence
                .iter()
                .map(|detail| DiagnosticsEvidence {
                    source: "oversight_signal".to_string(),
                    label: signal.recommended_action.clone(),
                    detail: detail.clone(),
                    observed_at: Some(oversight.timestamp.clone()),
                })
                .collect(),
        });
    }

    let desired_state = format!("{:?}", deployment.desired_state).to_lowercase();
    let observed_state = format!("{:?}", deployment.observed_state).to_lowercase();
    let primary_diagnosis = findings
        .first()
        .map(|finding| finding.kind.clone())
        .unwrap_or_else(|| {
            if desired_state != observed_state {
                "state_mismatch".to_string()
            } else {
                "stable".to_string()
            }
        });

    Ok(DeploymentDiagnosticsReport {
        generated_at: Utc::now().to_rfc3339(),
        deployment_id: deployment.deployment_id.clone(),
        bundle_id: deployment.bundle_id.clone(),
        runtime_mode: deployment.runtime_mode.clone(),
        account_id: deployment.account_id.clone(),
        desired_state: desired_state.clone(),
        observed_state: observed_state.clone(),
        max_gross_exposure: deployment.max_gross_exposure,
        primary_diagnosis,
        first_diverged_metric: if desired_state != observed_state {
            Some("state_mismatch".to_string())
        } else {
            findings.first().map(|finding| finding.kind.clone())
        },
        metrics: ploy_operator_contracts::DeploymentDiagnosticsMetrics {
            pending_intents: state_snapshot.risk.pending_intents,
            active_orders: state_snapshot.risk.active_orders,
            open_positions: state_snapshot.risk.open_positions,
            fills: state_snapshot.fills.len(),
            positions: state_snapshot.positions.len(),
            gross_exposure: state_snapshot.risk.gross_exposure,
            reserved_order_exposure: state_snapshot.risk.reserved_order_exposure,
            total_gross_exposure: state_snapshot.risk.total_gross_exposure,
            net_pnl: state_snapshot.pnl.net_pnl,
        },
        findings,
        recent_evidence,
    })
}

fn recent_snapshot_evidence(
    daemon: &PloyDaemon,
    limit: usize,
    deployment_id: Option<&str>,
) -> Vec<DiagnosticsEvidence> {
    snapshot_events(daemon)
        .into_iter()
        .filter_map(|event| event_to_evidence(&event, deployment_id))
        .take(limit)
        .collect()
}

fn event_to_evidence(
    event: &OperatorEvent,
    deployment_id: Option<&str>,
) -> Option<DiagnosticsEvidence> {
    match event {
        OperatorEvent::Log(log) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: format!("log:{}", log.component),
            detail: log.message.clone(),
            observed_at: Some(log.timestamp.to_rfc3339()),
        }),
        OperatorEvent::Status(status) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "status".to_string(),
            detail: format!("platform status={}", status.status),
            observed_at: None,
        }),
        OperatorEvent::SystemSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "system_snapshot".to_string(),
            detail: format!(
                "status={} errors_1h={} active_alerts={} stale_sources={}",
                event.system.status,
                event.system.error_count_1h,
                event.system.active_alert_count,
                event.system.stale_source_count,
            ),
            observed_at: event.system.last_trade_time.map(|value| value.to_rfc3339()),
        }),
        OperatorEvent::MetricsSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "metrics_snapshot".to_string(),
            detail: format!(
                "deployments_total={} degraded={} active_alerts={} stale_sources={} live_reconcile_failures={}",
                event.metrics.total_deployments,
                event.metrics.degraded_deployments,
                event.metrics.active_alerts,
                event.metrics.stale_sources,
                event.metrics.live_reconcile_failures,
            ),
            observed_at: event
                .metrics
                .last_live_reconcile_success_at
                .map(|value| value.to_rfc3339()),
        }),
        OperatorEvent::AlertSnapshot(event) => {
            event.alerts.first().map(|alert| DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "alert_snapshot".to_string(),
                detail: format!(
                    "{} {} {}",
                    format!("{:?}", alert.severity).to_lowercase(),
                    format!("{:?}", alert.kind).to_lowercase(),
                    alert.message
                ),
                observed_at: Some(alert.triggered_at.to_rfc3339()),
            })
        }
        OperatorEvent::OversightSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .oversight
                    .signals
                    .iter()
                    .any(|signal| signal.deployment_id.as_deref() == Some(target))
                    && !event
                        .oversight
                        .recommended_actions
                        .iter()
                        .any(|action| action.target == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "oversight_snapshot".to_string(),
                detail: format!(
                    "platform_status={} signal_count={} deployments_reviewed={}",
                    event.oversight.platform_status,
                    event.oversight.signal_count,
                    event.oversight.deployments_reviewed,
                ),
                observed_at: Some(event.oversight.timestamp.clone()),
            })
        }
        OperatorEvent::ProposalSnapshot(event) => {
            let count = deployment_id
                .map(|target| {
                    event
                        .proposals
                        .iter()
                        .filter(|proposal| proposal.target_deployment_id == target)
                        .count()
                })
                .unwrap_or(event.proposals.len());
            if count == 0 {
                return None;
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "proposal_snapshot".to_string(),
                detail: format!("proposals={count}"),
                observed_at: None,
            })
        }
        OperatorEvent::DeploymentSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .deployments
                    .iter()
                    .any(|deployment| deployment.deployment_id == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "deployment_snapshot".to_string(),
                detail: format!("deployments={}", event.deployments.len()),
                observed_at: None,
            })
        }
        OperatorEvent::TradingSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .trading
                    .iter()
                    .any(|snapshot| snapshot.deployment_id == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "trading_snapshot".to_string(),
                detail: format!("deployments={}", event.trading.len()),
                observed_at: None,
            })
        }
        _ => None,
    }
}

fn audit_entry_to_evidence(entry: &AuditLogEntry) -> DiagnosticsEvidence {
    DiagnosticsEvidence {
        source: "audit_log".to_string(),
        label: format!("{} {}", entry.method, entry.path),
        detail: format!(
            "status={} auth={} required={} outcome={} client={} {}",
            entry.status_code,
            entry.auth_level,
            entry.required_access,
            entry.outcome,
            entry.client_addr.as_deref().unwrap_or("-"),
            entry.message.as_deref().unwrap_or("-"),
        ),
        observed_at: Some(entry.timestamp.to_rfc3339()),
    }
}

fn likely_causes_from_kind(kind: &str) -> Vec<String> {
    match kind {
        "system_errors" => vec!["control_plane_instability".to_string()],
        "state_mismatch" => vec!["worker_lifecycle_divergence".to_string()],
        "order_buildup" => vec!["fill_quality_deterioration".to_string()],
        "position_buildup" => vec!["exit_path_stalled".to_string()],
        "exposure_watch" => vec!["risk_budget_pressure".to_string()],
        "pnl_regression" => vec!["strategy_regime_shift".to_string()],
        "source_stale" => vec!["data_feed_staleness".to_string()],
        _ => Vec::new(),
    }
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
    operator_token: Option<&str>,
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

    if let Some(expected_token) = operator_token {
        if extract_header(request, "x-ploy-operator-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Operator;
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
    operator_configured: bool,
    sidecar_configured: bool,
) -> bool {
    if !admin_configured && !operator_configured && !sidecar_configured {
        return true;
    }

    match required_access {
        RequiredAccess::Public => true,
        RequiredAccess::ReadOnly => matches!(
            auth_level,
            AuthLevel::Admin | AuthLevel::Operator | AuthLevel::Sidecar
        ),
        RequiredAccess::Operator => matches!(auth_level, AuthLevel::Admin | AuthLevel::Operator),
        RequiredAccess::Admin => auth_level == AuthLevel::Admin,
    }
}

fn auth_error_response(
    required_access: RequiredAccess,
    admin_configured: bool,
    operator_configured: bool,
    sidecar_configured: bool,
) -> (u16, String) {
    let message = match required_access {
        RequiredAccess::Public => return (200, "{}".to_string()),
        RequiredAccess::ReadOnly
            if sidecar_configured && !operator_configured && !admin_configured =>
        {
            "control-plane sidecar, operator, or admin token is required".to_string()
        }
        RequiredAccess::ReadOnly => {
            "control-plane admin, operator, or sidecar token is required".to_string()
        }
        RequiredAccess::Operator if admin_configured || operator_configured => {
            "control-plane operator or admin token is required".to_string()
        }
        RequiredAccess::Operator => "control-plane operator token is required".to_string(),
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
    operator_configured: bool,
    sidecar_configured: bool,
    state: &Arc<AppState>,
) -> (u16, String) {
    let required_access = required_access(method, path);
    match (method, path) {
        ("GET", "/auth/session") => (
            200,
            serde_json::json!({
                "authenticated": auth_level == AuthLevel::Admin,
                "auth_required": admin_configured || operator_configured || sidecar_configured,
                "operator_authenticated": auth_level == AuthLevel::Operator,
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
            operator_configured,
            sidecar_configured,
        ) =>
        {
            auth_error_response(
                required_access,
                admin_configured,
                operator_configured,
                sidecar_configured,
            )
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
        ("GET", "/api/system/metrics") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.platform_metrics())
                    .unwrap_or_else(|_| "{}".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/system/alerts") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.active_alerts()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/market-data/health") => build_market_data_health_json(state),
        ("GET", "/api/reports/dry-run") => build_dry_run_summary_json(state),
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
        ("GET", "/api/system/diagnose") => match state.daemon.lock() {
            Ok(daemon) => match build_platform_diagnostics_report(&daemon, state) {
                Ok(report) => (
                    200,
                    serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(err) => json_error(500, "diagnostics_unavailable", Some(err.to_string())),
            },
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/agent/runs") => match agent_runs_path(state)
            .map_err(|response| response)
            .and_then(|path| {
                read_agent_runs(&path)
                    .map_err(|err| json_error(500, "agent_runs_unavailable", Some(err.to_string())))
            }) {
            Ok(runs) => (
                200,
                serde_json::to_string(&runs).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(response) => response,
        },
        ("GET", _) if path.starts_with("/api/agent/runs/") => {
            let run_id = path.trim_start_matches("/api/agent/runs/");
            match agent_runs_path(state)
                .map_err(|response| response)
                .and_then(|path| {
                    read_agent_run(&path, run_id).map_err(|err| {
                        json_error(500, "agent_runs_unavailable", Some(err.to_string()))
                    })
                }) {
                Ok(Some(run)) => (
                    200,
                    serde_json::to_string(&run).unwrap_or_else(|_| "{}".to_string()),
                ),
                Ok(None) => json_error(
                    404,
                    "agent_run_not_found",
                    Some(format!("agent run `{run_id}` was not found")),
                ),
                Err(response) => response,
            }
        }
        ("GET", "/api/proposals") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.proposals()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", _) if path.starts_with("/api/proposals/") => {
            let proposal_id = path.trim_start_matches("/api/proposals/");
            match state.daemon.lock() {
                Ok(daemon) => match daemon
                    .proposals()
                    .into_iter()
                    .find(|proposal| proposal.proposal_id == proposal_id)
                {
                    Some(proposal) => (
                        200,
                        serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    None => json_error(
                        404,
                        "proposal_not_found",
                        Some(format!("proposal `{proposal_id}` was not found")),
                    ),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", "/api/proposals") => {
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: ProposalCreateRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            match state.daemon.lock() {
                Ok(mut daemon) => match daemon.create_proposal(request).and_then(|proposal| {
                    daemon.write_runtime_snapshots()?;
                    publish_snapshot_events(&daemon, &state.events);
                    Ok(proposal)
                }) {
                    Ok(proposal) => (
                        200,
                        serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        json_error(404, "deployment_not_found", Some(err.to_string()))
                    }
                    Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                        json_error(400, "invalid_request", Some(err.to_string()))
                    }
                    Err(err) => json_error(500, "proposal_create_failed", Some(err.to_string())),
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
        ("GET", _) if path.starts_with("/api/trading/diagnose/") => {
            let deployment_id = path.trim_start_matches("/api/trading/diagnose/");
            match state.daemon.lock() {
                Ok(daemon) => {
                    match build_deployment_diagnostics_report(&daemon, state, deployment_id) {
                        Ok(report) => (
                            200,
                            serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "diagnostics_unavailable", Some(err.to_string()))
                        }
                    }
                }
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
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) => json_error(500, "control_failed", Some(err.to_string())),
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
        ("POST", _) if path.starts_with("/api/proposals/") && path.ends_with("/approve") => {
            let proposal_id = path
                .trim_start_matches("/api/proposals/")
                .trim_end_matches("/approve")
                .trim_end_matches('/');
            let request = body
                .map(|raw| serde_json::from_str::<ProposalDecisionRequest>(raw))
                .transpose()
                .map_err(|err| json_error(400, "invalid_json", Some(err.to_string())));
            let request = match request {
                Ok(Some(request)) => request,
                Ok(None) => ProposalDecisionRequest::default(),
                Err(response) => return response,
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .approve_proposal(proposal_id, request)
                        .and_then(|proposal| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(proposal)
                        }) {
                        Ok(Some(proposal)) => (
                            200,
                            serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "proposal_not_found",
                            Some(format!("proposal `{proposal_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "proposal_approve_failed", Some(err.to_string()))
                        }
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/proposals/") && path.ends_with("/reject") => {
            let proposal_id = path
                .trim_start_matches("/api/proposals/")
                .trim_end_matches("/reject")
                .trim_end_matches('/');
            let request = body
                .map(|raw| serde_json::from_str::<ProposalDecisionRequest>(raw))
                .transpose()
                .map_err(|err| json_error(400, "invalid_json", Some(err.to_string())));
            let request = match request {
                Ok(Some(request)) => request,
                Ok(None) => ProposalDecisionRequest::default(),
                Err(response) => return response,
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .reject_proposal(proposal_id, request)
                        .and_then(|proposal| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(proposal)
                        }) {
                        Ok(Some(proposal)) => (
                            200,
                            serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "proposal_not_found",
                            Some(format!("proposal `{proposal_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "proposal_reject_failed", Some(err.to_string()))
                        }
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
    let deployments = daemon.control_plane.deployments.summaries();
    let trading = daemon.trading_state();
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let proposals = daemon.proposals();
    vec![
        OperatorEvent::Status(StatusUpdate {
            status: system.status.clone(),
        }),
        OperatorEvent::SystemSnapshot(SystemSnapshotEvent { system }),
        OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent { deployments }),
        OperatorEvent::TradingSnapshot(TradingSnapshotEvent { trading }),
        OperatorEvent::MetricsSnapshot(MetricsSnapshotEvent {
            metrics: daemon.platform_metrics(),
        }),
        OperatorEvent::AlertSnapshot(AlertSnapshotEvent {
            alerts: daemon.active_alerts(),
        }),
        OperatorEvent::OversightSnapshot(OversightSnapshotEvent { oversight }),
        OperatorEvent::ProposalSnapshot(ProposalSnapshotEvent { proposals }),
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
        admin_session_cookie, append_audit_entry, content_length, handle_api_request,
        handle_authenticated_runtime_request, handle_runtime_request, header_end_offset,
        request_auth_level, response_headers, route_request, snapshot_events, AppState, AuthLevel,
        RateLimiter, ADMIN_SESSION_COOKIE_NAME,
    };
    use crate::events::EventBroker;
    use chrono::{Duration, Utc};
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
    fn request_reader_helpers_accept_large_content_length() {
        let headers = b"POST /api/proposals HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4096";
        assert_eq!(content_length(headers).expect("content length"), 4096);
    }

    #[test]
    fn request_reader_helpers_find_header_end_after_large_headers() {
        let request = format!(
            "POST /api/proposals HTTP/1.1\r\nX-Large: {}\r\n\r\n{{}}",
            "x".repeat(3000)
        );
        assert!(header_end_offset(request.as_bytes()).is_some());
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
            request_auth_level(&request, Some("secret-token"), None, None, "cookie-secret"),
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
            request_auth_level(&request, Some("other-token"), None, None, "cookie-secret"),
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
                None,
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
            false,
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
            false,
            true,
            &state,
        );
        assert_eq!(write_code, 401);
        assert!(write_body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn operator_token_grants_write_access_but_not_admin_access() {
        let request = "POST /api/deployments/example.paper/control HTTP/1.1\r\nx-ploy-operator-token: operator-secret\r\n\r\n";
        assert_eq!(
            request_auth_level(
                request,
                Some("admin-secret"),
                Some("operator-secret"),
                Some("sidecar-secret"),
                "cookie-secret"
            ),
            AuthLevel::Operator
        );

        let config = crate::config::PlatformConfig {
            admin_token: Some("admin-secret".to_string().into()),
            operator_token: Some("operator-secret".to_string().into()),
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

        let (write_code, write_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::Operator,
            true,
            true,
            true,
            &state,
        );
        assert_eq!(write_code, 404);
        assert!(write_body.contains("deployment_not_found"));

        let (audit_code, audit_body) = handle_authenticated_runtime_request(
            "GET",
            "/api/audit/logs",
            None,
            AuthLevel::Operator,
            true,
            true,
            true,
            &state,
        );
        assert_eq!(audit_code, 401);
        assert!(audit_body.contains("\"error\":\"unauthorized\""));
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
            trading_state_file: runtime_root.join("trading-state.json"),
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
    fn handle_runtime_request_serves_metrics_and_alert_snapshots() {
        let root = temp_dir("metrics-alerts");
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

        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        daemon.control_plane.system.note_source_failure(
            "live_reconcile",
            "live_reconcile",
            Duration::seconds(15),
            "live reconcile loop exceeded stale threshold".to_string(),
        );
        daemon.control_plane.system.note_source_failure(
            "venue:polymarket",
            "venue",
            Duration::seconds(15),
            "venue heartbeat exceeded stale threshold".to_string(),
        );
        daemon.control_plane.system.refresh_source_health();

        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (metrics_code, metrics_body) =
            handle_runtime_request("GET", "/api/system/metrics", None, &state);
        assert_eq!(metrics_code, 200);
        let metrics: ploy_operator_contracts::PlatformMetrics =
            serde_json::from_str(&metrics_body).expect("metrics json");
        assert!(metrics.active_alerts >= 1);
        assert!(metrics.stale_sources >= 1);

        let (alerts_code, alerts_body) =
            handle_runtime_request("GET", "/api/system/alerts", None, &state);
        assert_eq!(alerts_code, 200);
        let alerts: Vec<ploy_operator_contracts::ActiveAlert> =
            serde_json::from_str(&alerts_body).expect("alerts json");
        assert!(alerts
            .iter()
            .any(|alert| alert.kind == ploy_operator_contracts::AlertKind::SourceStale));
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id.contains("live_reconcile")));
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
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::MetricsSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::AlertSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::OversightSnapshot(_)
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

    #[test]
    fn handle_runtime_request_reads_single_agent_run_detail() {
        let root = temp_dir("runtime-agent-run-detail");
        let runtime_root = root.join("run/platform");
        let agent_runs_file = root.join("run/sidecar/agent-runs.jsonl");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(agent_runs_file.parent().expect("agent runs parent"))
            .expect("create agent runs dir");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("registry");
        fs::write(
            &agent_runs_file,
            serde_json::json!({
                "run_id": "run-operator-detail",
                "cycle_kind": "oversight",
                "status": "succeeded",
                "started_at": Utc::now(),
                "finished_at": Utc::now(),
                "session_id": "session-1",
                "model": "claude-sonnet-4.5",
                "platform_status": "running",
                "deployment_count": 1,
                "oversight_signal_count": 2,
                "oversight_playbook_count": 1,
                "total_cost_usd": 0.12,
                "tool_calls": [
                    { "name": "mcp__research__check_oversight", "status": "succeeded" }
                ],
                "research_reports": 1,
                "oversight_alerts": 1,
                "operator_recommendations": 1,
                "failure_reason": null,
                "runtime_context": {
                    "deployment_sample": ["example.paper paper example"],
                    "oversight_signal_summary": ["critical order_buildup"],
                    "oversight_playbook_summary": ["replay example.paper"],
                    "diagnostic_candidates": ["example.paper"]
                },
                "output_summary": {
                    "research_report_summaries": ["replay example.paper filled 12 events"],
                    "oversight_alert_summaries": ["critical order_buildup"],
                    "operator_recommendation_summaries": ["pause review example.paper"]
                },
                "evaluation": {
                    "usefulness": "high",
                    "research_reports": 1,
                    "oversight_alerts": 1,
                    "operator_recommendations": 1
                }
            })
            .to_string(),
        )
        .expect("agent runs");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            agent_runs_file,
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/agent/runs/run-operator-detail", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"run_id\":\"run-operator-detail\""));
        assert!(body.contains("\"diagnostic_candidates\":[\"example.paper\"]"));
    }

    #[test]
    fn handle_runtime_request_creates_and_lists_proposals() {
        let root = temp_dir("runtime-proposals-create");
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
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "pnl regression crossed threshold".to_string(),
            evidence: vec!["net_pnl=-2.50".to_string()],
            source_run_id: Some("run-123".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (create_code, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        assert_eq!(create_code, 200);
        assert!(create_response.contains("\"proposal_id\""));
        assert!(create_response.contains("\"action_kind\":\"pause_deployment\""));

        let (list_code, list_response) =
            handle_runtime_request("GET", "/api/proposals", None, &state);
        assert_eq!(list_code, 200);
        assert!(list_response.contains("\"proposal_id\""));
        assert!(list_response.contains("\"status\":\"pending\""));
    }

    #[test]
    fn handle_runtime_request_reads_single_proposal_detail() {
        let root = temp_dir("runtime-proposals-detail");
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
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "drift crossed threshold".to_string(),
            evidence: vec!["drawdown=3.20".to_string()],
            source_run_id: Some("run-detail-1".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (_, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        let proposal =
            serde_json::from_str::<ploy_operator_contracts::SafetyProposal>(&create_response)
                .expect("proposal json");

        let (status_code, body) = handle_runtime_request(
            "GET",
            &format!("/api/proposals/{}", proposal.proposal_id),
            None,
            &state,
        );
        assert_eq!(status_code, 200);
        assert!(body.contains(&proposal.proposal_id));
        assert!(body.contains("\"source_run_id\":\"run-detail-1\""));
    }

    #[test]
    fn handle_runtime_request_approves_pause_proposal_through_control_plane() {
        let root = temp_dir("runtime-proposals-approve");
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
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "orders accumulated too quickly".to_string(),
            evidence: vec!["active_orders=8".to_string()],
            source_run_id: Some("run-456".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (_, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        let proposal_id =
            serde_json::from_str::<ploy_operator_contracts::SafetyProposal>(&create_response)
                .expect("proposal json")
                .proposal_id;

        let approve_body =
            serde_json::to_string(&ploy_operator_contracts::ProposalDecisionRequest {
                decision_note: Some("approved in test".to_string()),
            })
            .expect("approve json");
        let (approve_code, approve_response) = handle_runtime_request(
            "POST",
            &format!("/api/proposals/{proposal_id}/approve"),
            Some(&approve_body),
            &state,
        );
        assert_eq!(approve_code, 200);
        assert!(approve_response.contains("\"status\":\"approved\""));

        let (deployment_code, deployment_response) =
            handle_runtime_request("GET", "/api/deployments/example.paper", None, &state);
        assert_eq!(deployment_code, 200);
        assert!(deployment_response.contains("\"desired_state\":\"paused\""));
    }
}
