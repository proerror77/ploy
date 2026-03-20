use crate::trading::{MarketData, PositionResponse, TradeResponse};
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
}

pub type WsMessage = OperatorEvent;

#[cfg(test)]
mod tests {
    use super::{OperatorEvent, StatusUpdate};
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
}
