use crate::alerts::AlertRecord;
use crate::deployments::DeploymentSummary;
use crate::metrics::SystemMetrics;
use crate::system::SystemStatus;
use crate::trading::{MarketData, PositionResponse, TradeResponse, TradingStateSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub component: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemSnapshotEvent {
    pub system: SystemStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentSnapshotEvent {
    pub deployments: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingSnapshotEvent {
    pub trading: Vec<TradingStateSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertSnapshotEvent {
    pub alerts: Vec<AlertRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsSnapshotEvent {
    pub metrics: SystemMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OperatorEvent {
    #[serde(rename = "log")]
    Log(LogEntry),
    #[serde(rename = "trade")]
    Trade(TradeResponse),
    #[serde(rename = "position")]
    Position(PositionResponse),
    #[serde(rename = "market")]
    Market(MarketData),
    #[serde(rename = "status")]
    Status(StatusUpdate),
    #[serde(rename = "system_snapshot")]
    SystemSnapshot(SystemSnapshotEvent),
    #[serde(rename = "deployment_snapshot")]
    DeploymentSnapshot(DeploymentSnapshotEvent),
    #[serde(rename = "trading_snapshot")]
    TradingSnapshot(TradingSnapshotEvent),
    #[serde(rename = "alert_snapshot")]
    AlertSnapshot(AlertSnapshotEvent),
    #[serde(rename = "metrics_snapshot")]
    MetricsSnapshot(MetricsSnapshotEvent),
}

pub type WsMessage = OperatorEvent;

#[cfg(test)]
mod tests {
    use super::{
        AlertSnapshotEvent, DeploymentSnapshotEvent, MetricsSnapshotEvent, OperatorEvent,
        StatusUpdate, SystemSnapshotEvent, TradingSnapshotEvent,
    };
    use crate::{
        AlertRecord, AlertSeverity, DeploymentState, DeploymentSummary, DesiredState,
        ObservedState, SystemMetrics, SystemStatus, TradingStateSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn websocket_event_kind_serializes_as_status() {
        let value = serde_json::to_value(OperatorEvent::Status(StatusUpdate {
            status: "running".to_string(),
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "status",
                "data": {
                    "status": "running",
                }
            })
        );
    }

    #[test]
    fn deployment_snapshot_event_uses_stable_wire_shape() {
        let value =
            serde_json::to_value(OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent {
                deployments: vec![DeploymentSummary {
                    deployment_id: "example.paper".to_string(),
                    runtime_mode: "paper".to_string(),
                    account_id: "acct-paper".to_string(),
                    max_gross_exposure: Some(Decimal::new(500, 2)),
                    deployment_state: DeploymentState::Enabled,
                    desired_state: DesiredState::Running,
                    observed_state: ObservedState::Running,
                }],
            }))
            .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "deployment_snapshot",
                "data": {
                    "deployments": [{
                        "deployment_id": "example.paper",
                        "runtime_mode": "paper",
                        "account_id": "acct-paper",
                        "max_gross_exposure": "5.00",
                        "deployment_state": "enabled",
                        "desired_state": "running",
                        "observed_state": "running",
                    }]
                }
            })
        );
    }

    #[test]
    fn system_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
            system: SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 1,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
                last_claim_time: None,
                degraded_claim_accounts: 0,
                pending_redeemable_count: 0,
                pending_redeemable_notional: Decimal::ZERO,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
            },
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "system_snapshot",
                "data": {
                    "system": {
                        "status": "running",
                        "uptime_seconds": 1,
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
                        "last_live_reconcile_error": null,
                    }
                }
            })
        );
    }

    #[test]
    fn trading_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::TradingSnapshot(TradingSnapshotEvent {
            trading: vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                ..TradingStateSnapshot::default()
            }],
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "trading_snapshot",
                "data": {
                    "trading": [{
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
                            "net_pnl": "0",
                        },
                        "risk": {
                            "pending_intents": 0,
                            "active_orders": 0,
                            "open_positions": 0,
                            "gross_exposure": "0",
                            "reserved_order_exposure": "0",
                            "total_gross_exposure": "0",
                        },
                    }]
                }
            })
        );
    }

    #[test]
    fn alert_snapshot_event_uses_stable_wire_shape() {
        let now = Utc::now();
        let value = serde_json::to_value(OperatorEvent::AlertSnapshot(AlertSnapshotEvent {
            alerts: vec![AlertRecord {
                alert_id: "live_reconcile_degraded".to_string(),
                severity: AlertSeverity::Critical,
                kind: "live_reconcile_degraded".to_string(),
                source: "ployd".to_string(),
                resource_type: "system".to_string(),
                resource_id: None,
                message: "live reconcile is backing off".to_string(),
                first_seen_at: now,
                last_seen_at: now,
            }],
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "alert_snapshot",
                "data": {
                    "alerts": [{
                        "alert_id": "live_reconcile_degraded",
                        "severity": "critical",
                        "kind": "live_reconcile_degraded",
                        "source": "ployd",
                        "resource_type": "system",
                        "resource_id": null,
                        "message": "live reconcile is backing off",
                        "first_seen_at": now,
                        "last_seen_at": now,
                    }]
                }
            })
        );
    }

    #[test]
    fn metrics_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::MetricsSnapshot(MetricsSnapshotEvent {
            metrics: SystemMetrics {
                deployments_total: 1,
                deployments_running: 1,
                deployments_degraded: 0,
                deployments_failed: 0,
                live_deployments: 1,
                paper_deployments: 0,
                claim_accounts_total: 1,
                claim_accounts_degraded: 0,
                pending_intents: 0,
                active_orders: 1,
                open_positions: 1,
                gross_exposure: Decimal::new(500, 2),
                reserved_order_exposure: Decimal::new(50, 2),
                total_gross_exposure: Decimal::new(550, 2),
                active_alert_count: 1,
                warning_alert_count: 0,
                critical_alert_count: 1,
            },
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "metrics_snapshot",
                "data": {
                    "metrics": {
                        "deployments_total": 1,
                        "deployments_running": 1,
                        "deployments_degraded": 0,
                        "deployments_failed": 0,
                        "live_deployments": 1,
                        "paper_deployments": 0,
                        "claim_accounts_total": 1,
                        "claim_accounts_degraded": 0,
                        "pending_intents": 0,
                        "active_orders": 1,
                        "open_positions": 1,
                        "gross_exposure": "5.00",
                        "reserved_order_exposure": "0.50",
                        "total_gross_exposure": "5.50",
                        "active_alert_count": 1,
                        "warning_alert_count": 0,
                        "critical_alert_count": 1,
                    }
                }
            })
        );
    }
}
