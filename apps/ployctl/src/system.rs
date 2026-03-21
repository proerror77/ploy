use crate::client::ControlPlaneClient;

pub fn render_system_status(client: &ControlPlaneClient) -> Result<String, String> {
    let status = client.system_snapshot()?;
    Ok(format!(
        "status={} uptime={}s version={}",
        status.status, status.uptime_seconds, status.version
    ))
}

pub fn render_audit_log(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .audit_logs()?
        .into_iter()
        .map(|entry| {
            format!(
                "{} {} {} status={} auth={} required={} outcome={} client={} {}",
                entry.timestamp.to_rfc3339(),
                entry.method,
                entry.path,
                entry.status_code,
                entry.auth_level,
                entry.required_access,
                entry.outcome,
                entry.client_addr.unwrap_or_else(|| "-".to_string()),
                entry.message.unwrap_or_else(|| "-".to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{render_audit_log, render_system_status};
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-system-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_system_status() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "running",
                "uptime_seconds": 3,
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

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_status(&client).expect("system status");
        assert!(output.contains("running"));
    }

    #[test]
    fn renders_audit_log_lines_from_http() {
        let runtime_root = temp_dir("audit");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!([
                {
                    "timestamp": Utc::now(),
                    "method": "POST",
                    "path": "/api/deployments/example.paper/control",
                    "client_addr": "127.0.0.1:9000",
                    "auth_level": "admin",
                    "required_access": "admin",
                    "status_code": 200,
                    "outcome": "allowed",
                    "message": "deployment paused"
                }
            ])
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let output = render_audit_log(&client).expect("audit log");
        assert!(output.contains("/api/deployments/example.paper/control"));
        assert!(output.contains("deployment paused"));
        assert!(output.contains("auth=admin"));
    }
}
