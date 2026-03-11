use super::write_support::{
    parse_decimal, parse_domain, parse_optional_decimal, parse_optional_str, parse_str, parse_u64,
    require_write_enabled, submit_intent_via_coordinator,
};
use super::{jsonrpc_err, jsonrpc_ok};
use crate::control_plane::TradeIntent;
use crate::domain::{OrderSide, Side};
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

pub(super) async fn handle_coordinator_intent_method(
    request_id: &Option<Value>,
    method: &str,
    params: &Value,
) -> Option<Value> {
    match method {
        "pm.submit_limit" => Some(handle_pm_submit_limit(request_id, params).await),
        "gateway.submit_intent" => Some(handle_gateway_submit_intent(request_id, params).await),
        _ => None,
    }
}

async fn handle_pm_submit_limit(request_id: &Option<Value>, params: &Value) -> Value {
    if let Err(v) = require_write_enabled(request_id.clone()) {
        return v;
    }

    let token_id = match parse_str(params, "token_id") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let order_side = match parse_str(params, "order_side") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let shares = match parse_u64(params, "shares") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let limit_price = match parse_decimal(params, "limit_price") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let market_side = match parse_pm_market_side(params) {
        Ok(v) => v,
        Err(detail) => return invalid_params(request_id, detail),
    };
    let order_side = match parse_required_order_side(&order_side) {
        Ok(v) => v,
        Err(detail) => return invalid_params(request_id, detail),
    };
    let idempotency_key = match parse_str(params, "idempotency_key") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let domain = match parse_domain(params.get("domain").and_then(|v| v.as_str())) {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let agent_id =
        parse_optional_str(params, "agent_id").unwrap_or_else(|| "openclaw_rpc".to_string());
    let deployment_id = match parse_str(params, "deployment_id") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let market_slug = parse_optional_str(params, "market_slug").unwrap_or_else(|| token_id.clone());

    let metadata = build_pm_submit_limit_metadata(params, &deployment_id);
    let confidence = match parse_optional_decimal(params, "confidence") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let edge = match parse_optional_decimal(params, "edge") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let reason = parse_optional_str(params, "reason");

    let trade_intent = TradeIntent {
        intent_id: Uuid::new_v4(),
        deployment_id: deployment_id.clone(),
        agent_id,
        domain,
        market_slug,
        token_id,
        side: market_side,
        is_buy: matches!(order_side, OrderSide::Buy),
        size: shares,
        price_limit: limit_price,
        confidence,
        edge,
        event_time: None,
        reason,
        priority: None,
        metadata,
    };

    submit_trade_intent(
        request_id,
        "pm.submit_limit failed",
        &trade_intent,
        order_side,
        &idempotency_key,
    )
    .await
}

async fn handle_gateway_submit_intent(request_id: &Option<Value>, params: &Value) -> Value {
    if let Err(v) = require_write_enabled(request_id.clone()) {
        return v;
    }

    let deployment_id = match parse_str(params, "deployment_id") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let agent_id =
        parse_optional_str(params, "agent_id").unwrap_or_else(|| "openclaw_rpc".to_string());
    let domain = match parse_domain(params.get("domain").and_then(|v| v.as_str())) {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let market_slug = match parse_str(params, "market_slug") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let token_id = match parse_str(params, "token_id") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let side = match parse_gateway_side(params) {
        Ok(v) => v,
        Err(detail) => return invalid_params(request_id, detail),
    };
    let order_side = match parse_gateway_order_side(params) {
        Ok(v) => v,
        Err(detail) => return invalid_params(request_id, detail),
    };
    let size = match parse_gateway_size(params) {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let price_limit = match parse_gateway_price_limit(params) {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let idempotency_key = match parse_str(params, "idempotency_key") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let confidence = match parse_optional_decimal(params, "confidence") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let edge = match parse_optional_decimal(params, "edge") {
        Ok(v) => v,
        Err(e) => return invalid_params(request_id, e.to_string()),
    };
    let reason = parse_optional_str(params, "reason");
    let metadata = build_gateway_metadata(params);

    let trade_intent = TradeIntent {
        intent_id: Uuid::new_v4(),
        deployment_id,
        agent_id,
        domain,
        market_slug,
        token_id,
        side,
        is_buy: matches!(order_side, OrderSide::Buy),
        size,
        price_limit,
        confidence,
        edge,
        event_time: None,
        reason,
        priority: None,
        metadata,
    };

    submit_trade_intent(
        request_id,
        "gateway.submit_intent failed",
        &trade_intent,
        order_side,
        &idempotency_key,
    )
    .await
}

async fn submit_trade_intent(
    request_id: &Option<Value>,
    failure_message: &str,
    trade_intent: &TradeIntent,
    order_side: OrderSide,
    idempotency_key: &str,
) -> Value {
    let ingress_payload = build_ingress_payload(trade_intent, order_side, idempotency_key);
    match submit_intent_via_coordinator(&ingress_payload).await {
        Ok(submission) => jsonrpc_ok(
            request_id.clone(),
            json!({
                "submission": submission,
                "intent_id": trade_intent.intent_id,
                "deployment_id": trade_intent.deployment_id,
            }),
        ),
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            failure_message,
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

fn build_ingress_payload(
    trade_intent: &TradeIntent,
    order_side: OrderSide,
    idempotency_key: &str,
) -> Value {
    json!({
        "intent_id": trade_intent.intent_id,
        "deployment_id": trade_intent.deployment_id,
        "agent_id": trade_intent.agent_id,
        "domain": trade_intent.domain.to_string(),
        "market_slug": trade_intent.market_slug,
        "token_id": trade_intent.token_id,
        "side": match trade_intent.side { Side::Up => "UP", Side::Down => "DOWN" },
        "order_side": match order_side { OrderSide::Buy => "BUY", OrderSide::Sell => "SELL" },
        "size": trade_intent.size,
        "price_limit": trade_intent.price_limit.to_f64().unwrap_or(0.0),
        "idempotency_key": idempotency_key,
        "reason": trade_intent.reason,
        "confidence": trade_intent.confidence.and_then(|v| v.to_f64()),
        "edge": trade_intent.edge.and_then(|v| v.to_f64()),
        "metadata": trade_intent.metadata,
    })
}

fn build_pm_submit_limit_metadata(params: &Value, deployment_id: &str) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "rpc.pm.submit_limit".to_string());
    metadata.insert("deployment_id".to_string(), deployment_id.to_string());
    if let Some(v) = parse_optional_str(params, "symbol") {
        metadata.insert("symbol".to_string(), v);
    }
    if let Some(v) = parse_optional_str(params, "horizon") {
        metadata.insert("horizon".to_string(), v);
    }
    if let Some(v) = parse_optional_str(params, "series_id") {
        metadata.insert("series_id".to_string(), v.clone());
        metadata.entry("event_series_id".to_string()).or_insert(v);
    }
    metadata
}

fn build_gateway_metadata(params: &Value) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    if let Some(meta_obj) = params.get("metadata").and_then(|v| v.as_object()) {
        for (k, v) in meta_obj {
            if let Some(s) = v.as_str() {
                metadata.insert(k.clone(), s.to_string());
            } else {
                metadata.insert(k.clone(), v.to_string());
            }
        }
    }
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "rpc.gateway.submit_intent".to_string());
    metadata
}

fn parse_pm_market_side(params: &Value) -> std::result::Result<Side, &'static str> {
    match params
        .get("market_side")
        .and_then(|v| v.as_str())
        .unwrap_or("UP")
    {
        "UP" => Ok(Side::Up),
        "DOWN" => Ok(Side::Down),
        _ => Err("market_side must be UP|DOWN"),
    }
}

fn parse_gateway_side(params: &Value) -> std::result::Result<Side, &'static str> {
    match parse_str(params, "side")
        .unwrap_or_else(|_| "UP".to_string())
        .to_ascii_uppercase()
        .as_str()
    {
        "UP" | "YES" => Ok(Side::Up),
        "DOWN" | "NO" => Ok(Side::Down),
        _ => Err("side must be UP|DOWN|YES|NO"),
    }
}

fn parse_required_order_side(raw: &str) -> std::result::Result<OrderSide, &'static str> {
    match raw {
        "BUY" => Ok(OrderSide::Buy),
        "SELL" => Ok(OrderSide::Sell),
        _ => Err("order_side must be BUY|SELL"),
    }
}

fn parse_gateway_order_side(params: &Value) -> std::result::Result<OrderSide, &'static str> {
    match parse_str(params, "order_side")
        .unwrap_or_else(|_| "BUY".to_string())
        .to_ascii_uppercase()
        .as_str()
    {
        "BUY" => Ok(OrderSide::Buy),
        "SELL" => Ok(OrderSide::Sell),
        _ => Err("order_side must be BUY|SELL"),
    }
}

fn parse_gateway_size(params: &Value) -> std::result::Result<u64, crate::error::PloyError> {
    parse_u64(params, "size").or_else(|_| parse_u64(params, "shares"))
}

fn parse_gateway_price_limit(
    params: &Value,
) -> std::result::Result<rust_decimal::Decimal, crate::error::PloyError> {
    parse_decimal(params, "price_limit").or_else(|_| parse_decimal(params, "limit_price"))
}

fn invalid_params(request_id: &Option<Value>, detail: impl Into<String>) -> Value {
    jsonrpc_err(
        request_id.clone(),
        -32602,
        "invalid params",
        Some(json!({"detail": detail.into()})),
    )
}

#[cfg(test)]
mod tests {
    use super::{build_gateway_metadata, build_pm_submit_limit_metadata};
    use serde_json::json;

    #[test]
    fn pm_submit_limit_metadata_includes_deployment_and_series_alias() {
        let metadata = build_pm_submit_limit_metadata(
            &json!({
                "token_id": "tok-1",
                "symbol": "BTC",
                "horizon": "5m",
                "series_id": "BTC-5M"
            }),
            "dep-1",
        );

        assert_eq!(
            metadata.get("deployment_id").map(String::as_str),
            Some("dep-1")
        );
        assert_eq!(metadata.get("symbol").map(String::as_str), Some("BTC"));
        assert_eq!(
            metadata.get("event_series_id").map(String::as_str),
            Some("BTC-5M")
        );
    }

    #[test]
    fn gateway_metadata_defaults_source_and_stringifies_non_strings() {
        let metadata = build_gateway_metadata(&json!({
            "metadata": {
                "source": "custom",
                "priority": 3
            }
        }));

        assert_eq!(metadata.get("source").map(String::as_str), Some("custom"));
        assert_eq!(metadata.get("priority").map(String::as_str), Some("3"));
    }
}
