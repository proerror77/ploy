use ploy_operator_contracts::{AuditLogEntry, DiagnosticsEvidence, OperatorEvent};

pub fn likely_causes(kind: &str) -> Vec<String> {
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

pub fn audit_entry_to_evidence(entry: &AuditLogEntry) -> DiagnosticsEvidence {
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

pub fn event_to_evidence(event: &OperatorEvent) -> Option<DiagnosticsEvidence> {
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
        OperatorEvent::AlertSnapshot(event) => event.alerts.first().map(|alert| DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "alert_snapshot".to_string(),
            detail: format!(
                "{} {} {}",
                format!("{:?}", alert.severity).to_lowercase(),
                format!("{:?}", alert.kind).to_lowercase(),
                alert.message
            ),
            observed_at: Some(alert.triggered_at.to_rfc3339()),
        }),
        OperatorEvent::OversightSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "oversight_snapshot".to_string(),
            detail: format!(
                "platform_status={} signal_count={} deployments_reviewed={}",
                event.oversight.platform_status,
                event.oversight.signal_count,
                event.oversight.deployments_reviewed,
            ),
            observed_at: Some(event.oversight.timestamp.clone()),
        }),
        OperatorEvent::DeploymentSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "deployment_snapshot".to_string(),
            detail: format!("deployments={}", event.deployments.len()),
            observed_at: None,
        }),
        OperatorEvent::ProposalSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "proposal_snapshot".to_string(),
            detail: format!("proposals={}", event.proposals.len()),
            observed_at: event
                .proposals
                .last()
                .map(|proposal| proposal.created_at.to_rfc3339()),
        }),
        OperatorEvent::TradingSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "trading_snapshot".to_string(),
            detail: format!("deployments={}", event.trading.len()),
            observed_at: None,
        }),
        OperatorEvent::Trade(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: format!("trade:{}", event.token_id),
            detail: format!(
                "side={} shares={} status={} pnl={}",
                event.side,
                event.shares,
                event.status,
                event
                    .pnl
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            observed_at: Some(event.timestamp.to_rfc3339()),
        }),
        OperatorEvent::Position(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: format!("position:{}", event.token_id),
            detail: format!(
                "side={} shares={} unrealized_pnl={}",
                event.side, event.shares, event.unrealized_pnl
            ),
            observed_at: Some(event.entry_time.to_rfc3339()),
        }),
        OperatorEvent::Market(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: format!("market:{}", event.token_id),
            detail: format!(
                "bid={} ask={} last={} spread={}",
                event.best_bid, event.best_ask, event.last_price, event.spread
            ),
            observed_at: Some(event.timestamp.to_rfc3339()),
        }),
    }
}
