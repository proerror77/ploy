use ploy_operator_contracts::SystemStatus;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::net::{TcpListener, TcpStream};
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

pub fn spawn_server(listen_addr: String, runtime_root: PathBuf) -> io::Result<thread::JoinHandle<()>> {
    let listener = TcpListener::bind(&listen_addr)?;
    Ok(thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = handle_connection(stream, &runtime_root);
        }
    }))
}

fn handle_connection(mut stream: TcpStream, runtime_root: &Path) -> io::Result<()> {
    let mut request = [0_u8; 2048];
    let bytes = stream.read(&mut request)?;
    if bytes == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&request[..bytes]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status_code, body) = route_request(path, runtime_root);
    let status_text = match status_code {
        200 => "OK",
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
    use super::route_request;
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

        let (deployments_code, deployments_body) =
            route_request("/api/deployments", &runtime_root);
        assert_eq!(deployments_code, 200);
        assert!(deployments_body.contains("\"deployment_id\":\"example.paper\""));
    }
}
