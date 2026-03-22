use ploy_operator_contracts::{
    AccountClaimStatus, AlertRecord, DeploymentSummary, OperatorEvent, SystemMetrics, SystemStatus,
    TradingStateSnapshot,
};
use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct DashboardSnapshot {
    pub system: SystemStatus,
    pub metrics: SystemMetrics,
    pub alerts: Vec<AlertRecord>,
    pub deployments: Vec<DeploymentSummary>,
    pub trading: Vec<TradingStateSnapshot>,
    pub claims: Vec<AccountClaimStatus>,
    pub recent_events: Vec<OperatorEvent>,
}

pub fn render_dashboard(snapshot: &DashboardSnapshot) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "PLOY TUI");
    let _ = writeln!(out, "========");
    let _ = writeln!(out);

    let _ = writeln!(out, "System");
    let _ = writeln!(
        out,
        "status={} uptime={}s version={} errors_1h={}",
        snapshot.system.status,
        snapshot.system.uptime_seconds,
        snapshot.system.version,
        snapshot.system.error_count_1h
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Metrics");
    let _ = writeln!(
        out,
        "deployments running/degraded/failed={}/{}/{} live/paper={}/{} claims total/degraded={}/{} orders={} positions={} gross={} reserved={} total={} alerts active/warn/critical={}/{}/{}",
        snapshot.metrics.deployments_running,
        snapshot.metrics.deployments_degraded,
        snapshot.metrics.deployments_failed,
        snapshot.metrics.live_deployments,
        snapshot.metrics.paper_deployments,
        snapshot.metrics.claim_accounts_total,
        snapshot.metrics.claim_accounts_degraded,
        snapshot.metrics.active_orders,
        snapshot.metrics.open_positions,
        snapshot.metrics.gross_exposure,
        snapshot.metrics.reserved_order_exposure,
        snapshot.metrics.total_gross_exposure,
        snapshot.metrics.active_alert_count,
        snapshot.metrics.warning_alert_count,
        snapshot.metrics.critical_alert_count,
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Active Alerts");
    if snapshot.alerts.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for alert in &snapshot.alerts {
            let _ = writeln!(
                out,
                "  {} severity={} resource={}:{} {}",
                alert.alert_id,
                format!("{:?}", alert.severity).to_lowercase(),
                alert.resource_type,
                alert.resource_id.clone().unwrap_or_else(|| "-".to_string()),
                alert.message,
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Deployments");
    if snapshot.deployments.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for deployment in &snapshot.deployments {
            let _ = writeln!(
                out,
                "  {} mode={} lifecycle={} desired={} observed={}",
                deployment.deployment_id,
                deployment.runtime_mode,
                state_name(&deployment.deployment_state),
                state_name(&deployment.desired_state),
                state_name(&deployment.observed_state)
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Trading");
    if snapshot.trading.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for trading in &snapshot.trading {
            let _ = writeln!(
                out,
                "  {} mode={} intents={} orders={} fills={} positions={} net_pnl={}",
                trading.deployment_id,
                trading.runtime_mode,
                trading.intents.len(),
                trading.orders.len(),
                trading.fills.len(),
                trading.positions.len(),
                trading.pnl.net_pnl
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Claims");
    if snapshot.claims.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for claim in &snapshot.claims {
            let _ = writeln!(
                out,
                "  {} enabled={} loop={} pending={} pending_notional={}",
                claim.account_id,
                claim.enabled,
                format!("{:?}", claim.loop_state).to_lowercase(),
                claim.pending_redeemable_count,
                claim.pending_redeemable_notional,
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Recent Events");
    if snapshot.recent_events.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for event in &snapshot.recent_events {
            let _ = writeln!(out, "  {}", render_event_line(event));
        }
    }

    out
}

pub fn render_event_line(event: &OperatorEvent) -> String {
    match event {
        OperatorEvent::SystemSnapshot(event) => format!(
            "system_snapshot status={} uptime={}s",
            event.system.status, event.system.uptime_seconds
        ),
        OperatorEvent::DeploymentSnapshot(event) => {
            format!("deployment_snapshot count={}", event.deployments.len())
        }
        OperatorEvent::MetricsSnapshot(event) => format!(
            "metrics_snapshot active_alerts={} active_orders={} open_positions={}",
            event.metrics.active_alert_count,
            event.metrics.active_orders,
            event.metrics.open_positions
        ),
        OperatorEvent::AlertSnapshot(event) => {
            format!("alert_snapshot count={}", event.alerts.len())
        }
        OperatorEvent::TradingSnapshot(event) => {
            format!("trading_snapshot count={}", event.trading.len())
        }
        OperatorEvent::Status(event) => format!("status {}", event.status),
        OperatorEvent::Log(event) => format!("log {} {}", event.level, event.message),
        OperatorEvent::Trade(event) => format!(
            "trade {} {} shares={} status={}",
            event.id, event.token_id, event.shares, event.status
        ),
        OperatorEvent::Position(event) => format!(
            "position {} {} shares={}",
            event.token_id, event.side, event.shares
        ),
        OperatorEvent::Market(event) => format!(
            "market {} bid={} ask={}",
            event.token_id, event.best_bid, event.best_ask
        ),
    }
}

fn state_name<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{render_dashboard, render_event_line, DashboardSnapshot};
    use chrono::Utc;
    use ploy_operator_contracts::{
        AlertSeverity, ClaimLoopState, DeploymentSnapshotEvent, DeploymentState, DeploymentSummary,
        DesiredState, MetricsSnapshotEvent, ObservedState, OperatorEvent, SystemMetrics,
        SystemSnapshotEvent, SystemStatus, TradingSnapshotEvent, TradingStateSnapshot,
    };
    use rust_decimal::Decimal;

    fn sample_snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            system: SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 42,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
                last_claim_time: None,
                degraded_claim_accounts: 0,
                pending_redeemable_count: 0,
                pending_redeemable_notional: rust_decimal::Decimal::ZERO,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
            },
            metrics: SystemMetrics {
                deployments_total: 1,
                deployments_running: 1,
                deployments_degraded: 0,
                deployments_failed: 0,
                live_deployments: 0,
                paper_deployments: 1,
                claim_accounts_total: 1,
                claim_accounts_degraded: 0,
                pending_intents: 0,
                active_orders: 0,
                open_positions: 0,
                gross_exposure: Decimal::ZERO,
                reserved_order_exposure: Decimal::ZERO,
                total_gross_exposure: Decimal::ZERO,
                active_alert_count: 1,
                warning_alert_count: 1,
                critical_alert_count: 0,
            },
            alerts: vec![ploy_operator_contracts::AlertRecord {
                alert_id: "claim_loop_degraded".to_string(),
                severity: AlertSeverity::Warning,
                kind: "claim_loop_degraded".to_string(),
                source: "ployd".to_string(),
                resource_type: "claims".to_string(),
                resource_id: None,
                message: "degraded claim accounts=1".to_string(),
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            }],
            deployments: vec![DeploymentSummary {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "acct-paper".to_string(),
                max_gross_exposure: None,
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Running,
            }],
            trading: vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                ..TradingStateSnapshot::default()
            }],
            claims: vec![ploy_operator_contracts::AccountClaimStatus {
                account_id: "acct-live".to_string(),
                enabled: true,
                runtime_mode: "live".to_string(),
                loop_state: ClaimLoopState::Running,
                last_scan_at: None,
                last_claim_at: None,
                last_error: None,
                consecutive_failures: 0,
                next_retry_at: None,
                pending_redeemable_count: 1,
                pending_redeemable_notional: Decimal::new(500, 2),
            }],
            recent_events: vec![
                OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
                    system: SystemStatus {
                        status: "running".to_string(),
                        uptime_seconds: 42,
                        version: "0.1.0".to_string(),
                        strategy: "platform".to_string(),
                        last_trade_time: None,
                        websocket_connected: false,
                        database_connected: false,
                        error_count_1h: 0,
                        last_claim_time: None,
                        degraded_claim_accounts: 0,
                        pending_redeemable_count: 0,
                        pending_redeemable_notional: rust_decimal::Decimal::ZERO,
                        live_reconcile_failures: 0,
                        next_live_reconcile_at: None,
                        last_live_reconcile_error: None,
                    },
                }),
                OperatorEvent::MetricsSnapshot(MetricsSnapshotEvent {
                    metrics: SystemMetrics {
                        deployments_total: 1,
                        deployments_running: 1,
                        deployments_degraded: 0,
                        deployments_failed: 0,
                        live_deployments: 0,
                        paper_deployments: 1,
                        claim_accounts_total: 1,
                        claim_accounts_degraded: 0,
                        pending_intents: 0,
                        active_orders: 0,
                        open_positions: 0,
                        gross_exposure: Decimal::ZERO,
                        reserved_order_exposure: Decimal::ZERO,
                        total_gross_exposure: Decimal::ZERO,
                        active_alert_count: 1,
                        warning_alert_count: 1,
                        critical_alert_count: 0,
                    },
                }),
                OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent {
                    deployments: vec![DeploymentSummary {
                        deployment_id: "example.paper".to_string(),
                        runtime_mode: "paper".to_string(),
                        account_id: "acct-paper".to_string(),
                        max_gross_exposure: None,
                        deployment_state: DeploymentState::Enabled,
                        desired_state: DesiredState::Running,
                        observed_state: ObservedState::Running,
                    }],
                }),
                OperatorEvent::TradingSnapshot(TradingSnapshotEvent {
                    trading: vec![TradingStateSnapshot {
                        deployment_id: "example.paper".to_string(),
                        runtime_mode: "paper".to_string(),
                        ..TradingStateSnapshot::default()
                    }],
                }),
            ],
        }
    }

    #[test]
    fn dashboard_render_includes_core_sections() {
        let output = render_dashboard(&sample_snapshot());

        assert!(output.contains("PLOY TUI"));
        assert!(output.contains("System"));
        assert!(output.contains("Deployments"));
        assert!(output.contains("Trading"));
        assert!(output.contains("Claims"));
        assert!(output.contains("Metrics"));
        assert!(output.contains("Active Alerts"));
        assert!(output.contains("Recent Events"));
        assert!(output.contains("example.paper"));
        assert!(output.contains("mode=paper"));
        assert!(output.contains("acct-live"));
        assert!(output.contains("running"));
        assert!(output.contains("claim_loop_degraded"));
    }

    #[test]
    fn event_render_labels_snapshot_types() {
        let output = render_event_line(&OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
            system: SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 1,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: Some(Utc::now()),
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
                last_claim_time: None,
                degraded_claim_accounts: 0,
                pending_redeemable_count: 0,
                pending_redeemable_notional: rust_decimal::Decimal::ZERO,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
            },
        }));

        assert!(output.contains("system_snapshot"));
        assert!(output.contains("running"));
    }
}
