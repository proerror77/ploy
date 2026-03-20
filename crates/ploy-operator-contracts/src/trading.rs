use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentPurpose {
    Entry,
    Exit,
    Reduce,
    Hedge,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeResponse {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionResponse {
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub entry_time: DateTime<Utc>,
    pub duration_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketData {
    pub token_id: String,
    pub token_name: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub last_price: f64,
    pub volume_24h: f64,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::{IntentPurpose, TradeResponse};
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn intent_purpose_serializes_as_snake_case() {
        let json = serde_json::to_string(&IntentPurpose::Entry).expect("serialize");
        assert_eq!(json, "\"entry\"");
    }

    #[test]
    fn trade_response_uses_existing_wire_fields() {
        let timestamp = Utc::now();
        let value = serde_json::to_value(TradeResponse {
            id: "cycle-1".to_string(),
            timestamp,
            token_id: "token-1".to_string(),
            token_name: "BTC".to_string(),
            side: "UP".to_string(),
            shares: 5,
            entry_price: 0.42,
            exit_price: Some(0.61),
            pnl: Some(0.95),
            status: "COMPLETED".to_string(),
            error_message: None,
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "id": "cycle-1",
                "timestamp": timestamp,
                "token_id": "token-1",
                "token_name": "BTC",
                "side": "UP",
                "shares": 5,
                "entry_price": 0.42,
                "exit_price": 0.61,
                "pnl": 0.95,
                "status": "COMPLETED",
                "error_message": null,
            })
        );
    }
}
