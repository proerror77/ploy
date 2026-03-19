use crate::domain::{OrderRequest, OrderSide};
use crate::strategy::traits::StrategyOrderIntent;

pub(crate) fn order_request_from_strategy_intent(intent: &StrategyOrderIntent) -> OrderRequest {
    let client_order_id = intent.client_order_id.clone();
    OrderRequest {
        client_order_id: client_order_id.clone(),
        idempotency_key: Some(client_order_id),
        token_id: intent.token_id.clone(),
        market_side: intent.side,
        order_side: if intent.is_buy {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        },
        shares: intent.shares,
        limit_price: intent.limit_price,
        order_type: intent.order_type,
        time_in_force: intent.time_in_force,
    }
}

#[cfg(test)]
mod tests {
    use super::order_request_from_strategy_intent;
    use crate::domain::{Domain, OrderSide, OrderType, Side, TimeInForce};
    use crate::strategy::traits::StrategyOrderIntent;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    #[test]
    fn order_request_from_strategy_intent_preserves_action_id() {
        let request = order_request_from_strategy_intent(&StrategyOrderIntent {
            client_order_id: "intent-123".to_string(),
            domain: Domain::Politics,
            market_slug: "election-market".to_string(),
            token_id: "token-1".to_string(),
            side: Side::Up,
            is_buy: true,
            shares: 25,
            limit_price: dec!(0.44),
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            priority: 7,
            metadata: HashMap::new(),
        });

        assert_eq!(request.client_order_id, "intent-123");
        assert_eq!(request.idempotency_key.as_deref(), Some("intent-123"));
        assert_eq!(request.order_side, OrderSide::Buy);
        assert_eq!(request.market_side, Side::Up);
        assert_eq!(request.shares, 25);
        assert_eq!(request.limit_price, dec!(0.44));
        assert_eq!(request.order_type, OrderType::Market);
        assert_eq!(request.time_in_force, TimeInForce::IOC);
    }
}
