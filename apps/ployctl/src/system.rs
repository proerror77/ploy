use crate::client::ControlPlaneClient;

pub fn render_system_status(client: &ControlPlaneClient) -> String {
    client.system_status()
}

#[cfg(test)]
mod tests {
    use super::render_system_status;
    use crate::client::ControlPlaneClient;
    use std::fs;
    use std::path::PathBuf;
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
        let output = render_system_status(&client);
        assert!(output.contains("running"));
    }
}
