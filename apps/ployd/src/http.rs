use crate::config::PlatformConfig;
use crate::runtime::PloyDaemon;
use ploy_operator_contracts::{DeploymentApplyRequest, DeploymentControlRequest, SystemStatus};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::path::PathBuf;
use std::thread;

pub fn render_status(status: &SystemStatus) -> String {
    format!(
        "status={} uptime={}s version={}",
        status.status, status.uptime_seconds, status.version
    )
}

pub fn route_request(path: &str, runtime_root: &Path) -> (u16, String) {
    let target = match path {
        "/health" | "/api/system/status" => runtime_root.join("system-status.json"),
        "/api/deployments" => runtime_root.join("deployments.json"),
        _ => {
            return (404, "{\"error\":\"not_found\"}".to_string());
        }
    };

    match fs::read_to_string(target) {
        Ok(body) => (200, body),
        Err(_) => (503, "{\"error\":\"snapshot_unavailable\"}".to_string()),
    }
}

pub fn handle_api_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    config: &PlatformConfig,
) -> (u16, String) {
    match (method, path) {
        ("GET", "/health") | ("GET", "/api/system/status") | ("GET", "/api/deployments") => {
            route_request(path, &config.runtime_root)
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
        _ => (404, "{\"error\":\"not_found\"}".to_string()),
    }
}

pub fn spawn_server(config: PlatformConfig) -> io::Result<thread::JoinHandle<()>> {
    let listen_addr = config.listen_addr.clone();
    let listener = TcpListener::bind(&listen_addr)?;
    Ok(thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = handle_connection(stream, &config);
        }
    }))
}

fn handle_connection(mut stream: TcpStream, config: &PlatformConfig) -> io::Result<()> {
    let mut request = [0_u8; 2048];
    let bytes = stream.read(&mut request)?;
    if bytes == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&request[..bytes]);
    let mut request_line = request.lines().next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("GET");
    let path = request_line.next().unwrap_or("/");
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .filter(|body| !body.is_empty());
    let (status_code, body) = handle_api_request(method, path, body, config);
    let status_text = match status_code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Service Unavailable",
    };

    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        status_code,
        status_text,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use super::{handle_api_request, route_request};
    use std::fs;
    use std::path::PathBuf;
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
}
