use crate::deployments::DeploymentSummary;
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
}

pub type WsMessage = OperatorEvent;

#[cfg(test)]
mod tests {
    use super::{
        DeploymentSnapshotEvent, OperatorEvent, StatusUpdate, SystemSnapshotEvent,
        TradingSnapshotEvent,
    };
    use crate::{
        DeploymentSummary, DesiredState, ObservedState, SystemStatus, TradingStateSnapshot,
    };
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
                        "websocket_connected": false,
                        "database_connected": false,
                        "error_count_1h": 0,
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
                        },
                    }]
                }
            })
        );
    }
}
