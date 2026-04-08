use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsEvidence {
    pub source: String,
    pub label: String,
    pub detail: String,
    #[serde(default)]
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsFinding {
    pub severity: String,
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub first_observed_at: Option<String>,
    #[serde(default)]
    pub likely_causes: Vec<String>,
    #[serde(default)]
    pub operator_command: Option<String>,
    #[serde(default)]
    pub evidence: Vec<DiagnosticsEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformDiagnosticsReport {
    pub generated_at: String,
    pub platform_status: String,
    #[serde(default)]
    pub first_diverged_metric: Option<String>,
    #[serde(default)]
    pub findings: Vec<DiagnosticsFinding>,
    #[serde(default)]
    pub recent_evidence: Vec<DiagnosticsEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentDiagnosticsMetrics {
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub fills: usize,
    pub positions: usize,
    pub gross_exposure: Decimal,
    pub reserved_order_exposure: Decimal,
    pub total_gross_exposure: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentDiagnosticsReport {
    pub generated_at: String,
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: String,
    pub account_id: String,
    pub desired_state: String,
    pub observed_state: String,
    #[serde(default)]
    pub max_gross_exposure: Option<Decimal>,
    #[serde(default)]
    pub primary_diagnosis: String,
    #[serde(default)]
    pub first_diverged_metric: Option<String>,
    pub metrics: DeploymentDiagnosticsMetrics,
    #[serde(default)]
    pub findings: Vec<DiagnosticsFinding>,
    #[serde(default)]
    pub recent_evidence: Vec<DiagnosticsEvidence>,
}

#[cfg(test)]
mod tests {
    use super::{
        DeploymentDiagnosticsMetrics, DeploymentDiagnosticsReport, DiagnosticsEvidence,
        DiagnosticsFinding, PlatformDiagnosticsReport,
    };
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn deployment_diagnostics_report_uses_stable_wire_shape() {
        let value = serde_json::to_value(DeploymentDiagnosticsReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: "paper".to_string(),
            account_id: "acct-paper".to_string(),
            desired_state: "running".to_string(),
            observed_state: "degraded".to_string(),
            max_gross_exposure: Some(Decimal::new(500, 2)),
            primary_diagnosis: "runaway_risk".to_string(),
            first_diverged_metric: Some("active_orders".to_string()),
            metrics: DeploymentDiagnosticsMetrics {
                pending_intents: 4,
                active_orders: 6,
                open_positions: 2,
                fills: 8,
                positions: 1,
                gross_exposure: Decimal::new(475, 2),
                reserved_order_exposure: Decimal::new(50, 2),
                total_gross_exposure: Decimal::new(525, 2),
                net_pnl: Decimal::new(-125, 2),
            },
            findings: vec![DiagnosticsFinding {
                severity: "warning".to_string(),
                kind: "order_buildup".to_string(),
                message: "pending intents elevated at 4".to_string(),
                first_observed_at: Some("2026-04-07T00:01:00Z".to_string()),
                likely_causes: vec!["fill_quality_deterioration".to_string()],
                operator_command: Some("ployctl research replay example.paper".to_string()),
                evidence: vec![DiagnosticsEvidence {
                    source: "oversight_signal".to_string(),
                    label: "pending_intents".to_string(),
                    detail: "pending_intents=4".to_string(),
                    observed_at: Some("2026-04-07T00:01:00Z".to_string()),
                }],
            }],
            recent_evidence: vec![DiagnosticsEvidence {
                source: "audit_log".to_string(),
                label: "deployment_control".to_string(),
                detail: "deployment paused".to_string(),
                observed_at: Some("2026-04-07T00:02:00Z".to_string()),
            }],
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "generated_at": "2026-04-07T00:00:00Z",
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "acct-paper",
                "desired_state": "running",
                "observed_state": "degraded",
                "max_gross_exposure": "5.00",
                "primary_diagnosis": "runaway_risk",
                "first_diverged_metric": "active_orders",
                "metrics": {
                    "pending_intents": 4,
                    "active_orders": 6,
                    "open_positions": 2,
                    "fills": 8,
                    "positions": 1,
                    "gross_exposure": "4.75",
                    "reserved_order_exposure": "0.50",
                    "total_gross_exposure": "5.25",
                    "net_pnl": "-1.25",
                },
                "findings": [{
                    "severity": "warning",
                    "kind": "order_buildup",
                    "message": "pending intents elevated at 4",
                    "first_observed_at": "2026-04-07T00:01:00Z",
                    "likely_causes": ["fill_quality_deterioration"],
                    "operator_command": "ployctl research replay example.paper",
                    "evidence": [{
                        "source": "oversight_signal",
                        "label": "pending_intents",
                        "detail": "pending_intents=4",
                        "observed_at": "2026-04-07T00:01:00Z",
                    }],
                }],
                "recent_evidence": [{
                    "source": "audit_log",
                    "label": "deployment_control",
                    "detail": "deployment paused",
                    "observed_at": "2026-04-07T00:02:00Z",
                }],
            })
        );
    }

    #[test]
    fn platform_diagnostics_report_uses_stable_wire_shape() {
        let value = serde_json::to_value(PlatformDiagnosticsReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            platform_status: "degraded".to_string(),
            first_diverged_metric: Some("active_alerts".to_string()),
            findings: vec![DiagnosticsFinding {
                severity: "critical".to_string(),
                kind: "source_stale".to_string(),
                message: "live reconcile loop exceeded stale threshold".to_string(),
                first_observed_at: Some("2026-04-07T00:01:00Z".to_string()),
                likely_causes: vec!["reconcile_loop_stalled".to_string()],
                operator_command: Some("ployctl system audit".to_string()),
                evidence: vec![],
            }],
            recent_evidence: vec![DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "oversight_snapshot".to_string(),
                detail: "platform_status=degraded signal_count=1".to_string(),
                observed_at: Some("2026-04-07T00:01:00Z".to_string()),
            }],
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "generated_at": "2026-04-07T00:00:00Z",
                "platform_status": "degraded",
                "first_diverged_metric": "active_alerts",
                "findings": [{
                    "severity": "critical",
                    "kind": "source_stale",
                    "message": "live reconcile loop exceeded stale threshold",
                    "first_observed_at": "2026-04-07T00:01:00Z",
                    "likely_causes": ["reconcile_loop_stalled"],
                    "operator_command": "ployctl system audit",
                    "evidence": [],
                }],
                "recent_evidence": [{
                    "source": "event_stream",
                    "label": "oversight_snapshot",
                    "detail": "platform_status=degraded signal_count=1",
                    "observed_at": "2026-04-07T00:01:00Z",
                }],
            })
        );
    }
}
