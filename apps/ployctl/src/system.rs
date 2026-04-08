use crate::client::ControlPlaneClient;
use crate::diagnostics::{audit_entry_to_evidence, event_to_evidence, likely_causes};
use ploy_operator_contracts::{
    compute_oversight_report, AlertKind, AlertSeverity, DiagnosticsEvidence, DiagnosticsFinding,
    HeartbeatState, PlatformDiagnosticsReport,
};
use std::collections::BTreeSet;

pub fn render_system_status(client: &ControlPlaneClient) -> Result<String, String> {
    let status = client.system_snapshot()?;
    Ok(format!(
        "status={} uptime={}s version={} db_connected={} ws_connected={} errors_1h={} active_alerts={} stale_sources={} last_trade_time={} live_reconcile_failures={} next_live_reconcile_at={} last_live_reconcile_error={} last_live_reconcile_success_at={}",
        status.status,
        status.uptime_seconds,
        status.version,
        status.database_connected,
        status.websocket_connected,
        status.error_count_1h,
        status.active_alert_count,
        status.stale_source_count,
        status
            .last_trade_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status.live_reconcile_failures,
        status
            .next_live_reconcile_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_live_reconcile_error
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_live_reconcile_success_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
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

pub fn render_system_metrics(client: &ControlPlaneClient) -> Result<String, String> {
    let metrics = client.system_metrics()?;
    let heartbeat_summary = if metrics.heartbeats.is_empty() {
        "-".to_string()
    } else {
        metrics
            .heartbeats
            .iter()
            .map(|heartbeat| {
                format!(
                    "{}:{}:{}",
                    heartbeat.source_kind,
                    heartbeat.source_id,
                    heartbeat_state_name(heartbeat.state)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };

    Ok(format!(
        "deployments_total={} deployments_live={} deployments_degraded={} active_alerts={} stale_sources={} live_reconcile_failures={} last_trade_time={} last_live_reconcile_success_at={} heartbeats={}",
        metrics.total_deployments,
        metrics.live_deployments,
        metrics.degraded_deployments,
        metrics.active_alerts,
        metrics.stale_sources,
        metrics.live_reconcile_failures,
        metrics
            .last_trade_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .last_live_reconcile_success_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        heartbeat_summary,
    ))
}

pub fn render_system_alerts(client: &ControlPlaneClient) -> Result<String, String> {
    let alerts = client.system_alerts()?;
    if alerts.is_empty() {
        return Ok("none".to_string());
    }

    Ok(alerts
        .into_iter()
        .map(|alert| {
            format!(
                "{} {} {} {} {}",
                alert.triggered_at.to_rfc3339(),
                alert_severity_name(alert.severity),
                alert_kind_name(alert.kind),
                alert.source_id,
                alert.message,
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_system_diagnostics(client: &ControlPlaneClient) -> Result<String, String> {
    let system = client.system_snapshot()?;
    let metrics = client.system_metrics().ok();
    let alerts = client.system_alerts().unwrap_or_default();
    let deployments = client.deployment_summaries().unwrap_or_default();
    let trading = client.trading_state().unwrap_or_default();
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_lines = client
        .audit_logs()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(4)
        .map(|entry| audit_entry_to_evidence(&entry));
    let recent_events = client
        .recent_events(8)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| event_to_evidence(&event));

    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();

    for alert in alerts {
        let key = format!(
            "alert:{}:{}",
            format!("{:?}", alert.kind).to_lowercase(),
            alert.message
        );
        if seen.insert(key) {
            findings.push(DiagnosticsFinding {
                severity: alert_severity_name(alert.severity).to_string(),
                kind: alert_kind_name(alert.kind).to_string(),
                message: alert.message.clone(),
                first_observed_at: Some(alert.triggered_at.to_rfc3339()),
                likely_causes: likely_causes(alert_kind_name(alert.kind)),
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
        let key = format!("oversight:{}:{}", signal.kind, signal.message);
        if seen.insert(key) {
            findings.push(DiagnosticsFinding {
                severity: signal.severity.clone(),
                kind: signal.kind.clone(),
                message: signal.message.clone(),
                first_observed_at: Some(oversight.timestamp.clone()),
                likely_causes: likely_causes(&signal.kind),
                operator_command: Some(format!(
                    "ployctl {}",
                    match signal.recommended_action.as_str() {
                        "human_follow_up" => "system audit".to_string(),
                        other => format!("research {other}"),
                    }
                )),
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

    if let Some(metrics) = metrics.as_ref() {
        if metrics.live_reconcile_failures > 0
            && seen.insert(format!(
                "metrics:live_reconcile_failures:{}",
                metrics.live_reconcile_failures
            ))
        {
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
    }

    let recent_evidence = audit_lines.chain(recent_events).take(8).collect();
    let first_diverged_metric = if system.active_alert_count > 0 {
        Some("active_alerts".to_string())
    } else if system.stale_source_count > 0 {
        Some("stale_sources".to_string())
    } else if system.error_count_1h > 0 {
        Some("error_count_1h".to_string())
    } else if metrics
        .as_ref()
        .map(|value| value.live_reconcile_failures > 0)
        .unwrap_or(false)
    {
        Some("live_reconcile_failures".to_string())
    } else {
        findings.first().map(|finding| finding.kind.clone())
    };

    serde_json::to_string_pretty(&PlatformDiagnosticsReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        platform_status: system.status,
        first_diverged_metric,
        findings,
        recent_evidence,
    })
    .map_err(|err| format!("serialize system diagnostics: {err}"))
}

fn heartbeat_state_name(state: HeartbeatState) -> &'static str {
    match state {
        HeartbeatState::Healthy => "healthy",
        HeartbeatState::Stale => "stale",
    }
}

fn alert_severity_name(severity: AlertSeverity) -> &'static str {
    match severity {
        AlertSeverity::Warning => "warning",
        AlertSeverity::Critical => "critical",
    }
}

fn alert_kind_name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::SourceStale => "source_stale",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        render_audit_log, render_system_alerts, render_system_diagnostics, render_system_metrics,
        render_system_status,
    };
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
    use ploy_operator_contracts::{ActiveAlert, AlertKind, AlertSeverity};
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
                "error_count_1h": 0,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null,
                "active_alert_count": 1,
                "stale_source_count": 2,
                "last_live_reconcile_success_at": null
            })
            .to_string(),
        )
        .expect("write status");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_status(&client).expect("system status");
        assert!(output.contains("running"));
        assert!(output.contains("live_reconcile_failures=0"));
        assert!(output.contains("active_alerts=1"));
        assert!(output.contains("stale_sources=2"));
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
    fn renders_metrics_from_http() {
        let runtime_root = temp_dir("metrics");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "total_deployments": 3,
                "live_deployments": 2,
                "degraded_deployments": 1,
                "active_alerts": 2,
                "stale_sources": 1,
                "live_reconcile_failures": 4,
                "last_trade_time": null,
                "last_live_reconcile_success_at": null,
                "heartbeats": [{
                    "source_id": "live_reconcile",
                    "source_kind": "live_reconcile",
                    "state": "stale",
                    "last_seen_at": null,
                    "stale_after_seconds": 15,
                    "message": "live reconcile loop exceeded stale threshold"
                }]
            })
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

        let output = render_system_metrics(&client).expect("metrics");
        assert!(output.contains("deployments_total=3"));
        assert!(output.contains("active_alerts=2"));
        assert!(output.contains("heartbeats=live_reconcile:live_reconcile:stale"));
    }

    #[test]
    fn renders_alerts_from_http() {
        let runtime_root = temp_dir("alerts");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&vec![ActiveAlert {
                alert_id: "source-stale:live_reconcile".to_string(),
                kind: AlertKind::SourceStale,
                severity: AlertSeverity::Critical,
                source_id: "live_reconcile".to_string(),
                message: "live reconcile loop exceeded stale threshold".to_string(),
                triggered_at: Utc::now(),
            }])
            .expect("body");
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

        let output = render_system_alerts(&client).expect("alerts");
        assert!(output.contains("critical"));
        assert!(output.contains("source_stale"));
        assert!(output.contains("live_reconcile"));
    }

    #[test]
    fn renders_no_alerts_message_when_alert_list_is_empty() {
        let runtime_root = temp_dir("alerts-empty");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = "[]";
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

        let output = render_system_alerts(&client).expect("alerts");
        assert_eq!(output, "none");
    }

    #[test]
    fn renders_system_diagnostics_from_snapshot_and_oversight() {
        let runtime_root = temp_dir("diagnostics");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "degraded",
                "uptime_seconds": 3,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": true,
                "error_count_1h": 2,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null,
                "active_alert_count": 1,
                "stale_source_count": 1,
                "last_live_reconcile_success_at": null
            })
            .to_string(),
        )
        .expect("write status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([{
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "acct-paper",
                "max_gross_exposure": "5.00",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "degraded"
            }])
            .to_string(),
        )
        .expect("write deployments");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::json!([{
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
                    "net_pnl": "-2.50"
                },
                "risk": {
                    "pending_intents": 0,
                    "active_orders": 0,
                    "open_positions": 0,
                    "gross_exposure": "0",
                    "reserved_order_exposure": "0",
                    "total_gross_exposure": "0"
                }
            }])
            .to_string(),
        )
        .expect("write trading");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_system_diagnostics(&client).expect("system diagnostics");
        let value: serde_json::Value =
            serde_json::from_str(&output).expect("parse system diagnostics json");
        assert_eq!(value["platform_status"], serde_json::json!("degraded"));
        assert_eq!(
            value["first_diverged_metric"],
            serde_json::json!("active_alerts")
        );
        assert!(value["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .any(|finding| finding["kind"] == serde_json::json!("system_errors")));
    }
}
