use crate::client::ControlPlaneClient;

pub fn render_system_status(client: &ControlPlaneClient) -> Result<String, String> {
    let status = client.system_snapshot()?;
    Ok(format!(
        "status={} uptime={}s version={} db_connected={} ws_connected={} errors_1h={} last_trade_time={} last_claim_time={} degraded_claim_accounts={} pending_redeemable_count={} pending_redeemable_notional={} live_reconcile_failures={} next_live_reconcile_at={} last_live_reconcile_error={}",
        status.status,
        status.uptime_seconds,
        status.version,
        status.database_connected,
        status.websocket_connected,
        status.error_count_1h,
        status
            .last_trade_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_claim_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status.degraded_claim_accounts,
        status.pending_redeemable_count,
        status.pending_redeemable_notional,
        status.live_reconcile_failures,
        status
            .next_live_reconcile_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_live_reconcile_error
            .unwrap_or_else(|| "-".to_string()),
    ))
}

pub fn render_system_metrics(client: &ControlPlaneClient) -> Result<String, String> {
    let metrics = client.system_metrics()?;
    Ok(format!(
        "deployments_total={} deployments_running={} deployments_degraded={} deployments_failed={} live_deployments={} paper_deployments={} claim_accounts_total={} claim_accounts_degraded={} pending_intents={} active_orders={} open_positions={} gross_exposure={} reserved_order_exposure={} total_gross_exposure={} active_alert_count={} warning_alert_count={} critical_alert_count={}",
        metrics.deployments_total,
        metrics.deployments_running,
        metrics.deployments_degraded,
        metrics.deployments_failed,
        metrics.live_deployments,
        metrics.paper_deployments,
        metrics.claim_accounts_total,
        metrics.claim_accounts_degraded,
        metrics.pending_intents,
        metrics.active_orders,
        metrics.open_positions,
        metrics.gross_exposure,
        metrics.reserved_order_exposure,
        metrics.total_gross_exposure,
        metrics.active_alert_count,
        metrics.warning_alert_count,
        metrics.critical_alert_count,
    ))
}

pub fn render_system_alerts(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .system_alerts()?
        .into_iter()
        .map(|alert| {
            format!(
                "{} severity={} kind={} source={} resource={}:{} {}",
                alert.alert_id,
                format!("{:?}", alert.severity).to_lowercase(),
                alert.kind,
                alert.source,
                alert.resource_type,
                alert.resource_id.unwrap_or_else(|| "-".to_string()),
                alert.message,
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
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
    use super::{
        render_audit_log, render_system_alerts, render_system_metrics, render_system_status,
    };
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
                "last_claim_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0,
                "degraded_claim_accounts": 0,
                "pending_redeemable_count": 0,
                "pending_redeemable_notional": "0",
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null
            })
            .to_string(),
        )
        .expect("write status");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_status(&client).expect("system status");
        assert!(output.contains("running"));
        assert!(output.contains("live_reconcile_failures=0"));
        assert!(output.contains("degraded_claim_accounts=0"));
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

    #[test]
    fn renders_metrics_from_snapshot() {
        let runtime_root = temp_dir("metrics");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-metrics.json"),
            serde_json::json!({
                "deployments_total": 2,
                "deployments_running": 1,
                "deployments_degraded": 1,
                "deployments_failed": 0,
                "live_deployments": 1,
                "paper_deployments": 1,
                "claim_accounts_total": 1,
                "claim_accounts_degraded": 1,
                "pending_intents": 2,
                "active_orders": 3,
                "open_positions": 1,
                "gross_exposure": "10.5",
                "reserved_order_exposure": "2.0",
                "total_gross_exposure": "12.5",
                "active_alert_count": 2,
                "warning_alert_count": 1,
                "critical_alert_count": 1
            })
            .to_string(),
        )
        .expect("write metrics");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_metrics(&client).expect("system metrics");
        assert!(output.contains("deployments_degraded=1"));
        assert!(output.contains("critical_alert_count=1"));
    }

    #[test]
    fn renders_alerts_from_snapshot() {
        let runtime_root = temp_dir("alerts");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-alerts.json"),
            serde_json::json!([
                {
                    "alert_id": "system_degraded",
                    "severity": "critical",
                    "kind": "system_degraded",
                    "source": "ployd",
                    "resource_type": "system",
                    "resource_id": null,
                    "message": "platform runtime is degraded",
                    "first_seen_at": Utc::now(),
                    "last_seen_at": Utc::now()
                }
            ])
            .to_string(),
        )
        .expect("write alerts");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_alerts(&client).expect("system alerts");
        assert!(output.contains("system_degraded"));
        assert!(output.contains("severity=critical"));
    }
}
