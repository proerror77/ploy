use crate::domain::{OrderRequest, OrderSide};
use crate::platform::{OrderIntent, OrderPriority};

use super::traits::StrategyOrderIntent;

pub fn order_request_from_intent(intent: &StrategyOrderIntent) -> OrderRequest {
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

pub fn order_intent_from_strategy_intent(
    agent_id: &str,
    intent: &StrategyOrderIntent,
) -> OrderIntent {
    let mut order_intent = OrderIntent::new(
        agent_id.to_string(),
        intent.domain,
        intent.market_slug.clone(),
        intent.token_id.clone(),
        intent.side,
        intent.is_buy,
        intent.shares,
        intent.limit_price,
    )
    .with_client_order_id(intent.client_order_id.clone())
    .with_order_type(intent.order_type)
    .with_time_in_force(intent.time_in_force);
    order_intent.priority = map_strategy_priority(intent.priority);
    order_intent.metadata = intent.metadata.clone();
    order_intent.metadata.insert(
        "idempotency_key".to_string(),
        intent.client_order_id.clone(),
    );
    order_intent
        .metadata
        .insert("strategy_priority".to_string(), intent.priority.to_string());
    order_intent
}

fn map_strategy_priority(priority: u8) -> OrderPriority {
    match priority {
        90..=u8::MAX => OrderPriority::Critical,
        8..=89 => OrderPriority::High,
        5..=7 => OrderPriority::Normal,
        _ => OrderPriority::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::{order_intent_from_strategy_intent, order_request_from_intent};
    use crate::domain::{OrderSide, OrderType, TimeInForce};
    use crate::platform::{Domain, OrderPriority};
    use crate::strategy::traits::StrategyOrderIntent;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    #[test]
    fn order_request_from_intent_preserves_action_id() {
        let request = order_request_from_intent(&StrategyOrderIntent {
            client_order_id: "intent-123".to_string(),
            domain: Domain::Politics,
            market_slug: "election-market".to_string(),
            token_id: "token-1".to_string(),
            side: crate::domain::Side::Up,
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
        assert_eq!(request.market_side, crate::domain::Side::Up);
        assert_eq!(request.shares, 25);
        assert_eq!(request.limit_price, dec!(0.44));
        assert_eq!(request.order_type, OrderType::Market);
        assert_eq!(request.time_in_force, TimeInForce::IOC);
    }

    #[test]
    fn order_intent_from_strategy_intent_preserves_runtime_metadata() {
        let order_intent = order_intent_from_strategy_intent(
            "managed.runtime",
            &StrategyOrderIntent {
                client_order_id: "intent-123".to_string(),
                domain: Domain::Politics,
                market_slug: "election-market".to_string(),
                token_id: "token-1".to_string(),
                side: crate::domain::Side::Up,
                is_buy: true,
                shares: 25,
                limit_price: dec!(0.44),
                order_type: OrderType::Market,
                time_in_force: TimeInForce::IOC,
                priority: 7,
                metadata: HashMap::from([(
                    "deployment_id".to_string(),
                    "deploy.politics.test".to_string(),
                )]),
            },
        );

        assert_eq!(order_intent.agent_id, "managed.runtime");
        assert_eq!(order_intent.client_order_id, "intent-123");
        assert_eq!(order_intent.order_type, OrderType::Market);
        assert_eq!(order_intent.time_in_force, TimeInForce::IOC);
        assert_eq!(order_intent.priority, OrderPriority::Normal);
        assert_eq!(
            order_intent
                .metadata
                .get("idempotency_key")
                .map(String::as_str),
            Some("intent-123")
        );
        assert_eq!(
            order_intent
                .metadata
                .get("strategy_priority")
                .map(String::as_str),
            Some("7")
        );
        assert_eq!(order_intent.deployment_id(), Some("deploy.politics.test"));
    }
}
