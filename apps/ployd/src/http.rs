use crate::events::EventBroker;
use crate::runtime::{next_paper_intent_id, PloyDaemon};
use ploy_operator_contracts::{
    ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest,
    DeploymentSnapshotEvent, IntentPurpose, OperatorEvent, PaperIntentRequest, StatusUpdate,
    SystemSnapshotEvent, SystemStatus, TradingSnapshotEvent,
};
use ploy_trading::{TradeSide, TradingIntent};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(test)]
use crate::config::PlatformConfig;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;

#[derive(Debug)]
pub struct AppState {
    pub daemon: Arc<Mutex<PloyDaemon>>,
    pub events: Arc<EventBroker>,
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
        io::ErrorKind::InvalidInput => {
            json_error(400, "invalid_request", Some(err.to_string()))
        }
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
            match PloyDaemon::boot(config).and_then(|mut daemon| {
                daemon.set_desired_state(deployment_id, request.desired_state)
            }) {
                Ok(Some(record)) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                Ok(None) => (404, "{\"error\":\"deployment_not_found\"}".to_string()),
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
    if method == "GET" && path == "/api/events/stream" {
        return handle_event_stream(stream, state);
    }
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .filter(|body| !body.is_empty());
    let (status_code, body) = handle_runtime_request(method, path, body, state);
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        status_code,
        status_text(status_code),
        body.len(),
        body
    )
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
                Ok(mut daemon) => match daemon
                    .set_desired_state(deployment_id, request.desired_state)
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
                    Err(err) => json_error(500, "control_failed", Some(err.to_string())),
                },
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
        handle_api_request, handle_runtime_request, route_request, snapshot_events, AppState,
    };
    use crate::events::EventBroker;
    use ploy_connectivity::StaticExecutionGateway;
    use ploy_operator_contracts::PaperIntentRequest;
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
                "error_count_1h": 0
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
                        "gross_exposure": "0"
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
