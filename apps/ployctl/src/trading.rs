use crate::client::ControlPlaneClient;
use crate::diagnostics::{audit_entry_to_evidence, event_to_evidence, likely_causes};
use ploy_operator_contracts::{
    compute_oversight_report, DeploymentDiagnosticsMetrics, DeploymentDiagnosticsReport,
    DiagnosticsEvidence, DiagnosticsFinding, OperatorEvent, OrderReplaceRequest,
};
use std::collections::BTreeSet;

pub fn render_trading_state(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .trading_state()?
        .into_iter()
        .map(|state| {
            format!(
                "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} reserved_exposure={} total_exposure={} net_pnl={}",
                state.deployment_id,
                state.runtime_mode,
                state.intents.len(),
                state.orders.len(),
                state.fills.len(),
                state.positions.len(),
                state.risk.active_orders,
                state.risk.gross_exposure,
                state.risk.reserved_order_exposure,
                state.risk.total_gross_exposure,
                state.pnl.net_pnl,
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_one_trading_state(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
    client.inspect_trading_state(deployment_id).map(|state| {
        format!(
            "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} reserved_exposure={} total_exposure={} net_pnl={}",
            state.deployment_id,
            state.runtime_mode,
            state.intents.len(),
            state.orders.len(),
            state.fills.len(),
            state.positions.len(),
            state.risk.active_orders,
            state.risk.gross_exposure,
            state.risk.reserved_order_exposure,
            state.risk.total_gross_exposure,
            state.pnl.net_pnl,
        )
    })
}

pub fn cancel_order(
    client: &ControlPlaneClient,
    deployment_id: &str,
    order_id: &str,
) -> Result<String, String> {
    client
        .cancel_order(deployment_id, order_id)
        .map(|response| {
            format!(
                "{} order={} state={} filled_qty={} venue_order_id={}",
                response.deployment_id,
                response.order_id,
                response.state,
                response.filled_qty,
                response.venue_order_id.unwrap_or_else(|| "-".to_string()),
            )
        })
}

pub fn replace_order(
    client: &ControlPlaneClient,
    deployment_id: &str,
    order_id: &str,
    quantity: rust_decimal::Decimal,
    limit_price: Option<rust_decimal::Decimal>,
) -> Result<String, String> {
    client
        .replace_order(
            deployment_id,
            order_id,
            &OrderReplaceRequest {
                quantity,
                limit_price,
            },
        )
        .map(|response| {
            format!(
                "{} order={} state={} revision={} filled_qty={} requested_qty={} limit_price={} venue_order_id={} history={}",
                response.deployment_id,
                response.order_id,
                response.state,
                response.revision,
                response.filled_qty,
                response.requested_qty,
                response
                    .limit_price
                    .map(|price| price.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                response.venue_order_id.unwrap_or_else(|| "-".to_string()),
                if response.venue_order_history.is_empty() {
                    "-".to_string()
                } else {
                    response.venue_order_history.join(",")
                },
            )
        })
}

pub fn render_deployment_diagnostics(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
    let system = client.system_snapshot()?;
    let deployments = client.deployment_summaries()?;
    let trading = client.trading_state()?;
    let deployment = deployments
        .iter()
        .find(|item| item.deployment_id == deployment_id)
        .ok_or_else(|| format!("deployment `{deployment_id}` was not found"))?;
    let state = trading
        .iter()
        .find(|item| item.deployment_id == deployment_id)
        .ok_or_else(|| format!("trading state for `{deployment_id}` was not found"))?;
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_evidence = client
        .audit_logs()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry.path.contains(deployment_id)
                || entry
                    .message
                    .as_deref()
                    .map(|message| message.contains(deployment_id))
                    .unwrap_or(false)
        })
        .rev()
        .take(4)
        .map(|entry| audit_entry_to_evidence(&entry));
    let event_evidence = client
        .recent_events(12)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| event_mentions_deployment(event, deployment_id))
        .filter_map(|event| event_to_evidence(&event));

    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for signal in oversight
        .signals
        .iter()
        .filter(|signal| signal.deployment_id.as_deref() == Some(deployment_id))
    {
        let key = format!("{}:{}", signal.kind, signal.message);
        if seen.insert(key) {
            let action = oversight.recommended_actions.iter().find(|action| {
                action.target == deployment_id && action.kind == signal.recommended_action
            });
            findings.push(DiagnosticsFinding {
                severity: signal.severity.clone(),
                kind: signal.kind.clone(),
                message: signal.message.clone(),
                first_observed_at: Some(oversight.timestamp.clone()),
                likely_causes: likely_causes(&signal.kind),
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
    }

    let mut recent_evidence = vec![DiagnosticsEvidence {
        source: "current_snapshot".to_string(),
        label: "trading_state".to_string(),
        detail: format!(
            "pending_intents={} active_orders={} open_positions={} total_gross_exposure={} net_pnl={}",
            state.risk.pending_intents,
            state.risk.active_orders,
            state.risk.open_positions,
            state.risk.total_gross_exposure,
            state.pnl.net_pnl,
        ),
        observed_at: Some(oversight.timestamp.clone()),
    }];
    recent_evidence.extend(audit_evidence);
    recent_evidence.extend(event_evidence);
    recent_evidence.truncate(8);

    let primary_diagnosis = findings
        .first()
        .map(|finding| finding.kind.clone())
        .unwrap_or_else(|| "stable".to_string());
    let state_mismatch = format!("{:?}", deployment.desired_state).to_lowercase()
        != format!("{:?}", deployment.observed_state).to_lowercase();
    let first_diverged_metric = if state_mismatch {
        Some("state_mismatch".to_string())
    } else {
        findings.first().map(|finding| finding.kind.clone())
    };

    serde_json::to_string_pretty(&DeploymentDiagnosticsReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        deployment_id: deployment.deployment_id.clone(),
        bundle_id: deployment.bundle_id.clone(),
        runtime_mode: deployment.runtime_mode.clone(),
        account_id: deployment.account_id.clone(),
        desired_state: format!("{:?}", deployment.desired_state).to_lowercase(),
        observed_state: format!("{:?}", deployment.observed_state).to_lowercase(),
        max_gross_exposure: deployment.max_gross_exposure,
        primary_diagnosis,
        first_diverged_metric,
        metrics: DeploymentDiagnosticsMetrics {
            pending_intents: state.risk.pending_intents,
            active_orders: state.risk.active_orders,
            open_positions: state.risk.open_positions,
            fills: state.fills.len(),
            positions: state.positions.len(),
            gross_exposure: state.risk.gross_exposure,
            reserved_order_exposure: state.risk.reserved_order_exposure,
            total_gross_exposure: state.risk.total_gross_exposure,
            net_pnl: state.pnl.net_pnl,
        },
        findings,
        recent_evidence,
    })
    .map_err(|err| format!("serialize deployment diagnostics: {err}"))
}

fn event_mentions_deployment(event: &OperatorEvent, deployment_id: &str) -> bool {
    match event {
        OperatorEvent::Log(log) => {
            log.component.contains(deployment_id)
                || log.message.contains(deployment_id)
                || log
                    .metadata
                    .as_ref()
                    .map(|value| value.to_string().contains(deployment_id))
                    .unwrap_or(false)
        }
        OperatorEvent::DeploymentSnapshot(event) => event
            .deployments
            .iter()
            .any(|deployment| deployment.deployment_id == deployment_id),
        OperatorEvent::TradingSnapshot(event) => event
            .trading
            .iter()
            .any(|snapshot| snapshot.deployment_id == deployment_id),
        OperatorEvent::OversightSnapshot(event) => {
            event
                .oversight
                .signals
                .iter()
                .any(|signal| signal.deployment_id.as_deref() == Some(deployment_id))
                || event
                    .oversight
                    .recommended_actions
                    .iter()
                    .any(|action| action.target == deployment_id)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cancel_order, render_deployment_diagnostics, render_one_trading_state,
        render_trading_state, replace_order,
    };
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::{OrderControlResponse, TradingStateSnapshot};
    use rust_decimal::Decimal;
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
        std::env::temp_dir().join(format!("ployctl-trading-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_trading_state() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                ..TradingStateSnapshot::default()
            }])
            .expect("trading json"),
        )
        .expect("write trading state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_trading_state(&client).expect("trading state");
        assert!(output.contains("example.paper"));
        assert!(output.contains("net_pnl=0"));
        assert!(render_one_trading_state(&client, "example.paper").is_ok());
    }

    #[test]
    fn renders_order_cancel_response() {
        let runtime_root = temp_dir("cancel");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "canceled".to_string(),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                requested_qty: Decimal::ZERO,
                limit_price: None,
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: None,
            operator_token: None,
            sidecar_token: None,
            runtime_root,
        };
        let output = cancel_order(&client, "example.live", "order-1").expect("cancel output");
        assert!(output.contains("state=canceled"));
        assert!(output.contains("venue_order_id=venue-1"));
    }

    #[test]
    fn renders_order_replace_response() {
        let runtime_root = temp_dir("replace");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "acknowledged".to_string(),
                venue_order_id: Some("venue-2".to_string()),
                venue_order_history: vec!["venue-1".to_string()],
                revision: 1,
                requested_qty: Decimal::new(250, 2),
                limit_price: Some(Decimal::new(57, 2)),
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: None,
            operator_token: None,
            sidecar_token: None,
            runtime_root,
        };
        let output = replace_order(
            &client,
            "example.live",
            "order-1",
            Decimal::new(250, 2),
            Some(Decimal::new(57, 2)),
        )
        .expect("replace output");
        assert!(output.contains("revision=1"));
        assert!(output.contains("requested_qty=2.50"));
        assert!(output.contains("limit_price=0.57"));
        assert!(output.contains("history=venue-1"));
    }

    #[test]
    fn renders_deployment_diagnostics_from_snapshot_and_oversight() {
        let runtime_root = temp_dir("diagnostics");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "running",
                "uptime_seconds": 3,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": true,
                "database_connected": true,
                "error_count_1h": 0,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null,
                "active_alert_count": 0,
                "stale_source_count": 0,
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
                    "pending_intents": 4,
                    "active_orders": 0,
                    "open_positions": 0,
                    "gross_exposure": "0",
                    "reserved_order_exposure": "0",
                    "total_gross_exposure": "0"
                }
            }])
            .to_string(),
        )
        .expect("write trading state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_deployment_diagnostics(&client, "example.paper")
            .expect("deployment diagnostics");
        let value: serde_json::Value =
            serde_json::from_str(&output).expect("parse deployment diagnostics json");
        assert_eq!(value["deployment_id"], serde_json::json!("example.paper"));
        assert_eq!(
            value["primary_diagnosis"],
            serde_json::json!("state_mismatch")
        );
        assert_eq!(value["metrics"]["pending_intents"], serde_json::json!(4));
        assert!(value["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| finding["kind"] == serde_json::json!("state_mismatch")));
    }
}
