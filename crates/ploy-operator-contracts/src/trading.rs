use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
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
pub struct TradingIntentSnapshot {
    pub intent_id: String,
    pub market_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub purpose: IntentPurpose,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperIntentRequest {
    pub market_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub purpose: IntentPurpose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperIntentResponse {
    pub deployment_id: String,
    pub intent_id: String,
    pub order_id: String,
    pub state: String,
    pub venue_order_id: Option<String>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderControlResponse {
    pub deployment_id: String,
    pub order_id: String,
    pub state: String,
    pub venue_order_id: Option<String>,
    pub rejection_reason: Option<String>,
    pub filled_qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderSnapshot {
    pub order_id: String,
    pub intent_id: String,
    pub token_id: String,
    pub requested_qty: Decimal,
    pub limit_price: Option<Decimal>,
    pub venue_order_id: Option<String>,
    pub state: String,
    pub filled_qty: Decimal,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillSnapshot {
    pub fill_id: String,
    pub order_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fee: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionSnapshotResponse {
    pub token_id: String,
    pub net_qty: Decimal,
    pub avg_entry_price: Decimal,
    pub realized_pnl: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PnlSnapshotResponse {
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_fees: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RiskSnapshotResponse {
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub gross_exposure: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradingStateSnapshot {
    pub deployment_id: String,
    pub runtime_mode: String,
    pub intents: Vec<TradingIntentSnapshot>,
    pub orders: Vec<OrderSnapshot>,
    pub fills: Vec<FillSnapshot>,
    pub positions: Vec<PositionSnapshotResponse>,
    pub pnl: PnlSnapshotResponse,
    pub risk: RiskSnapshotResponse,
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
    use super::{
        IntentPurpose, OrderControlResponse, OrderSnapshot, PaperIntentRequest,
        PaperIntentResponse, PnlSnapshotResponse, RiskSnapshotResponse, TradeResponse,
        TradingIntentSnapshot, TradingStateSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
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

    #[test]
    fn order_control_response_uses_stable_wire_keys() {
        let value = serde_json::to_value(OrderControlResponse {
            deployment_id: "example.live".to_string(),
            order_id: "order-1".to_string(),
            state: "canceled".to_string(),
            venue_order_id: Some("venue-1".to_string()),
            rejection_reason: None,
            filled_qty: Decimal::ZERO,
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "deployment_id": "example.live",
                "order_id": "order-1",
                "state": "canceled",
                "venue_order_id": "venue-1",
                "rejection_reason": null,
                "filled_qty": "0",
            })
        );
    }

    #[test]
    fn trading_state_snapshot_uses_stable_wire_keys() {
        let timestamp = Utc::now();
        let value = serde_json::to_value(TradingStateSnapshot {
            deployment_id: "example.paper".to_string(),
            runtime_mode: "paper".to_string(),
            intents: vec![TradingIntentSnapshot {
                intent_id: "intent-1".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: "buy".to_string(),
                quantity: rust_decimal::Decimal::ONE,
                limit_price: None,
                purpose: IntentPurpose::Entry,
                created_at: timestamp,
            }],
            orders: vec![OrderSnapshot {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                token_id: "token-1".to_string(),
                requested_qty: rust_decimal::Decimal::ONE,
                limit_price: None,
                venue_order_id: Some("venue-1".to_string()),
                state: "filled".to_string(),
                filled_qty: rust_decimal::Decimal::ONE,
                rejection_reason: None,
            }],
            fills: vec![],
            positions: vec![],
            pnl: PnlSnapshotResponse::default(),
            risk: RiskSnapshotResponse::default(),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "deployment_id": "example.paper",
                "runtime_mode": "paper",
                "intents": [{
                    "intent_id": "intent-1",
                    "market_id": "market-1",
                    "token_id": "token-1",
                    "side": "buy",
                    "quantity": "1",
                    "limit_price": null,
                    "purpose": "entry",
                    "created_at": timestamp,
                }],
                "orders": [{
                    "order_id": "order-1",
                    "intent_id": "intent-1",
                    "token_id": "token-1",
                    "requested_qty": "1",
                    "limit_price": null,
                    "venue_order_id": "venue-1",
                    "state": "filled",
                    "filled_qty": "1",
                    "rejection_reason": null,
                }],
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
            })
        );
    }

    #[test]
    fn paper_intent_contract_uses_stable_wire_keys() {
        let request = serde_json::to_value(PaperIntentRequest {
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: IntentPurpose::Entry,
        })
        .expect("request json");
        assert_eq!(
            request,
            json!({
                "market_id": "market-1",
                "token_id": "token-1",
                "side": "buy",
                "quantity": "1",
                "limit_price": "1",
                "purpose": "entry",
            })
        );

        let response = serde_json::to_value(PaperIntentResponse {
            deployment_id: "example.paper".to_string(),
            intent_id: "intent-1".to_string(),
            order_id: "order-intent-1".to_string(),
            state: "acknowledged".to_string(),
            venue_order_id: Some("venue-order-1".to_string()),
            rejection_reason: None,
        })
        .expect("response json");
        assert_eq!(
            response,
            json!({
                "deployment_id": "example.paper",
                "intent_id": "intent-1",
                "order_id": "order-intent-1",
                "state": "acknowledged",
                "venue_order_id": "venue-order-1",
                "rejection_reason": null,
            })
        );
    }
}
