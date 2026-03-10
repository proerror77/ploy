use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::postgres::PostgresStore;
use crate::adapters::PolymarketClient;
use crate::control_plane::TradeIntent;
use crate::domain::{OrderSide, Side};
use crate::error::Result;
use crate::signing::Wallet;
use crate::strategy::event_edge::{discover_best_event_id_by_title, scan_event_edge_once};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use crate::strategy::multi_outcome::fetch_multi_outcome_event;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

mod pm_read_methods;
mod write_support;

use pm_read_methods::handle_pm_read_method;
use write_support::{
    finalize_write_response, hash_idempotency_params, idempotency_record_path, is_write_method,
    load_app_config, load_idempotency_record, parse_decimal, parse_domain,
    parse_idempotency_key, parse_optional_decimal, parse_optional_str, parse_str, parse_u64,
    require_write_enabled, submit_intent_via_coordinator, write_enabled, IdempotencyContext,
};
// (keep logs minimal; stdout is reserved for JSON-RPC responses)

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

fn jsonrpc_ok(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn jsonrpc_err(id: Option<Value>, code: i32, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({
        "code": code,
        "message": message,
    });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err
    })
}

async fn build_pm_client(rest_url: &str, dry_run: bool) -> Result<PolymarketClient> {
    if dry_run {
        return PolymarketClient::new(rest_url, true);
    }

    let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
    let funder = std::env::var("POLYMARKET_FUNDER").ok();
    if let Some(funder_addr) = funder {
        PolymarketClient::new_authenticated_proxy(rest_url, wallet, &funder_addr, false).await
    } else {
        PolymarketClient::new_authenticated(rest_url, wallet, false).await
    }
}

pub async fn run_rpc(config_path: &str) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut body = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut body).await?;

    if body.trim().is_empty() {
        println!(
            "{}",
            jsonrpc_err(None, -32600, "empty request body", None).to_string()
        );
        return Ok(());
    }

    let req: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            println!(
                "{}",
                jsonrpc_err(
                    None,
                    -32700,
                    "parse error",
                    Some(json!({ "detail": e.to_string() }))
                )
                .to_string()
            );
            return Ok(());
        }
    };

    if req.jsonrpc.as_deref().unwrap_or("2.0") != "2.0" {
        println!(
            "{}",
            jsonrpc_err(req.id, -32600, "invalid jsonrpc version", None).to_string()
        );
        return Ok(());
    }

    let params = req.params.unwrap_or_else(|| json!({}));
    let config_path = PathBuf::from(config_path);
    let config = match load_app_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            println!(
                "{}",
                jsonrpc_err(
                    req.id,
                    -32000,
                    "config load failed",
                    Some(json!({ "detail": e.to_string() }))
                )
                .to_string()
            );
            return Ok(());
        }
    };

    let rest_url = config.market.rest_url.clone();
    let dry_run = config.dry_run.enabled;
    let allow_write = write_enabled();
    let method_name = req.method.clone();
    let request_id = req.id.clone();
    let idempotency_ctx = if is_write_method(&method_name) {
        let key = match parse_idempotency_key(&params) {
            Ok(v) => v,
            Err(e) => {
                println!(
                    "{}",
                    jsonrpc_err(
                        request_id,
                        -32602,
                        "invalid params",
                        Some(json!({"detail": e.to_string()})),
                    )
                );
                return Ok(());
            }
        };
        let params_hash = match hash_idempotency_params(&params) {
            Ok(v) => v,
            Err(e) => {
                println!(
                    "{}",
                    jsonrpc_err(
                        req.id.clone(),
                        -32001,
                        "idempotency hash failed",
                        Some(json!({"detail": e.to_string()})),
                    )
                );
                return Ok(());
            }
        };
        let record_path = idempotency_record_path(&method_name, &key);

        match load_idempotency_record(&record_path) {
            Ok(Some(existing)) => {
                if existing.params_hash != params_hash {
                    println!(
                        "{}",
                        jsonrpc_err(
                            req.id.clone(),
                            -32011,
                            "idempotency key conflict (params mismatch)",
                            Some(json!({"key": key})),
                        )
                    );
                    return Ok(());
                }

                let mut replay = existing.response.clone();
                if let Some(obj) = replay.as_object_mut() {
                    obj.insert("id".to_string(), req.id.clone().unwrap_or(Value::Null));
                }
                println!("{}", replay);
                return Ok(());
            }
            Ok(None) => Some(IdempotencyContext {
                key,
                params_hash,
                record_path,
            }),
            Err(e) => {
                println!(
                    "{}",
                    jsonrpc_err(
                        req.id.clone(),
                        -32001,
                        "idempotency check failed",
                        Some(json!({"detail": e.to_string()})),
                    )
                );
                return Ok(());
            }
        }
    } else {
        None
    };

    let resp = if let Some(resp) = handle_pm_read_method(
        req.id.clone(),
        req.method.as_str(),
        &params,
        &rest_url,
        dry_run,
    )
    .await
    {
        resp
    } else {
        match req.method.as_str() {
            "system.ping" => jsonrpc_ok(req.id, json!({"ok": true})),

            "system.describe" => jsonrpc_ok(
                req.id,
                json!({
                    "ok": true,
                    "rest_url": rest_url,
                    "dry_run": dry_run,
                    "write_enabled": allow_write,
                    "methods": [
                        "system.ping",
                        "system.describe",
                        "pm.resolve_event_id",
                        "pm.get_balance",
                        "pm.get_positions",
                        "pm.get_open_orders",
                        "pm.get_order",
                        "pm.cancel_order",
                        "pm.search_markets",
                        "pm.get_event_details",
                        "pm.get_market",
                        "pm.get_order_book",
                        "pm.get_trades",
                        "pm.get_account_summary",
                        "pm.submit_limit",
                        "gateway.submit_intent",
                        "event_edge.scan",
                        "multi_outcome.analyze",
                        "events.upsert",
                        "events.list",
                        "events.update_status"
                    ]
                }),
            ),

            "pm.cancel_order" => {
                if let Err(v) = require_write_enabled(req.id.clone()) {
                    println!("{}", v.to_string());
                    return Ok(());
                }
                let order_id = match parse_str(&params, "order_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                match build_pm_client(&rest_url, dry_run).await {
                    Ok(c) => match c.cancel_order(&order_id).await {
                        Ok(ok) => jsonrpc_ok(req.id, json!({ "ok": ok })),
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "pm.cancel_order failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm client init failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "pm.submit_limit" => {
                if let Err(v) = require_write_enabled(req.id.clone()) {
                    println!("{}", v.to_string());
                    return Ok(());
                }
                // params:
                // - token_id: string
                // - market_side: "UP" | "DOWN" (optional, default "UP" to match EventEdge YES token convention)
                // - order_side: "BUY" | "SELL"
                // - shares: integer
                // - limit_price: number|string (0..1)
                let token_id = match parse_str(&params, "token_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let order_side = match parse_str(&params, "order_side") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let shares = match parse_u64(&params, "shares") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let limit_price = match parse_decimal(&params, "limit_price") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let market_side = params
                    .get("market_side")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UP");
                let market_side = match market_side {
                    "UP" => Side::Up,
                    "DOWN" => Side::Down,
                    _ => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "market_side must be UP|DOWN"}))
                            )
                        );
                        return Ok(());
                    }
                };
                let order_side = match order_side.as_str() {
                    "BUY" => OrderSide::Buy,
                    "SELL" => OrderSide::Sell,
                    _ => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "order_side must be BUY|SELL"}))
                            )
                        );
                        return Ok(());
                    }
                };

                let idempotency_key = match parse_str(&params, "idempotency_key") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };

                let domain = match parse_domain(params.get("domain").and_then(|v| v.as_str())) {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let agent_id = parse_optional_str(&params, "agent_id")
                    .unwrap_or_else(|| "openclaw_rpc".to_string());
                let deployment_id = match parse_str(&params, "deployment_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let market_slug =
                    parse_optional_str(&params, "market_slug").unwrap_or_else(|| token_id.clone());

                let mut metadata: HashMap<String, String> = HashMap::new();
                metadata.insert("source".to_string(), "rpc.pm.submit_limit".to_string());
                metadata.insert("deployment_id".to_string(), deployment_id.clone());
                if let Some(v) = parse_optional_str(&params, "symbol") {
                    metadata.insert("symbol".to_string(), v);
                }
                if let Some(v) = parse_optional_str(&params, "horizon") {
                    metadata.insert("horizon".to_string(), v);
                }
                if let Some(v) = parse_optional_str(&params, "series_id") {
                    metadata.insert("series_id".to_string(), v.clone());
                    metadata.entry("event_series_id".to_string()).or_insert(v);
                }

                let confidence = match parse_optional_decimal(&params, "confidence") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let edge = match parse_optional_decimal(&params, "edge") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let reason = parse_optional_str(&params, "reason");

                let trade_intent = TradeIntent {
                    intent_id: Uuid::new_v4(),
                    deployment_id: deployment_id.clone(),
                    agent_id: agent_id.clone(),
                    domain,
                    market_slug: market_slug.clone(),
                    token_id: token_id.clone(),
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

                let ingress_payload = json!({
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
                });

                match submit_intent_via_coordinator(&ingress_payload).await {
                    Ok(submission) => jsonrpc_ok(
                        req.id,
                        json!({
                            "submission": submission,
                            "intent_id": trade_intent.intent_id,
                            "deployment_id": trade_intent.deployment_id,
                        }),
                    ),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.submit_limit failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "gateway.submit_intent" => {
                if let Err(v) = require_write_enabled(req.id.clone()) {
                    println!("{}", v.to_string());
                    return Ok(());
                }

                let deployment_id = match parse_str(&params, "deployment_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let agent_id = parse_optional_str(&params, "agent_id")
                    .unwrap_or_else(|| "openclaw_rpc".into());
                let domain = match parse_domain(params.get("domain").and_then(|v| v.as_str())) {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let market_slug = match parse_str(&params, "market_slug") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let token_id = match parse_str(&params, "token_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let side = match parse_str(&params, "side")
                    .unwrap_or_else(|_| "UP".to_string())
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "UP" | "YES" => Side::Up,
                    "DOWN" | "NO" => Side::Down,
                    _ => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "side must be UP|DOWN|YES|NO"}))
                            )
                        );
                        return Ok(());
                    }
                };
                let order_side = match parse_str(&params, "order_side")
                    .unwrap_or_else(|_| "BUY".to_string())
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "BUY" => OrderSide::Buy,
                    "SELL" => OrderSide::Sell,
                    _ => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "order_side must be BUY|SELL"}))
                            )
                        );
                        return Ok(());
                    }
                };
                let size = match parse_u64(&params, "size") {
                    Ok(v) => v,
                    Err(_) => match parse_u64(&params, "shares") {
                        Ok(v) => v,
                        Err(e) => {
                            println!(
                                "{}",
                                jsonrpc_err(
                                    req.id,
                                    -32602,
                                    "invalid params",
                                    Some(json!({"detail": e.to_string()}))
                                )
                            );
                            return Ok(());
                        }
                    },
                };
                let price_limit = match parse_decimal(&params, "price_limit") {
                    Ok(v) => v,
                    Err(_) => match parse_decimal(&params, "limit_price") {
                        Ok(v) => v,
                        Err(e) => {
                            println!(
                                "{}",
                                jsonrpc_err(
                                    req.id,
                                    -32602,
                                    "invalid params",
                                    Some(json!({"detail": e.to_string()}))
                                )
                            );
                            return Ok(());
                        }
                    },
                };
                let idempotency_key = match parse_str(&params, "idempotency_key") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };

                let confidence = match parse_optional_decimal(&params, "confidence") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let edge = match parse_optional_decimal(&params, "edge") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let reason = parse_optional_str(&params, "reason");

                let mut metadata: HashMap<String, String> = HashMap::new();
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

                let trade_intent = TradeIntent {
                    intent_id: Uuid::new_v4(),
                    deployment_id: deployment_id.clone(),
                    agent_id: agent_id.clone(),
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

                let ingress_payload = json!({
                    "intent_id": trade_intent.intent_id,
                    "deployment_id": trade_intent.deployment_id,
                    "agent_id": trade_intent.agent_id,
                    "domain": trade_intent.domain.to_string(),
                    "market_slug": trade_intent.market_slug,
                    "token_id": trade_intent.token_id,
                    "side": match trade_intent.side { Side::Up => "UP", Side::Down => "DOWN" },
                    "order_side": if trade_intent.is_buy { "BUY" } else { "SELL" },
                    "size": trade_intent.size,
                    "price_limit": trade_intent.price_limit.to_f64().unwrap_or(0.0),
                    "idempotency_key": idempotency_key,
                    "reason": trade_intent.reason,
                    "confidence": trade_intent.confidence.and_then(|v| v.to_f64()),
                    "edge": trade_intent.edge.and_then(|v| v.to_f64()),
                    "metadata": trade_intent.metadata,
                });

                match submit_intent_via_coordinator(&ingress_payload).await {
                    Ok(submission) => jsonrpc_ok(
                        req.id,
                        json!({
                            "submission": submission,
                            "intent_id": trade_intent.intent_id,
                            "deployment_id": trade_intent.deployment_id,
                        }),
                    ),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "gateway.submit_intent failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "event_edge.scan" => {
                let event_id_opt = params
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let title_opt = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let event_id = match (event_id_opt, title_opt) {
                    (Some(id), _) => id,
                    (None, Some(t)) => match discover_best_event_id_by_title(&t).await {
                        Ok(id) => id,
                        Err(e) => {
                            println!(
                                "{}",
                                jsonrpc_err(
                                    req.id,
                                    -32001,
                                    "event discovery failed",
                                    Some(json!({"detail": e.to_string()}))
                                )
                            );
                            return Ok(());
                        }
                    },
                    _ => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "event_id or title required"}))
                            )
                        );
                        return Ok(());
                    }
                };

                let arena = match fetch_arena_text_snapshot().await {
                    Ok(a) => a,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32001,
                                "arena fetch failed",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };

                match build_pm_client(&rest_url, true).await {
                    Ok(c) => match scan_event_edge_once(&c, &event_id, Some(arena)).await {
                        Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "event_edge.scan failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm client init failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "multi_outcome.analyze" => {
                let event_id = match parse_str(&params, "event_id") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };

                match build_pm_client(&rest_url, true).await {
                    Ok(c) => match fetch_multi_outcome_event(&c, &event_id).await {
                        Ok(monitor) => {
                            let arbs = monitor.find_all_arbitrage();
                            let summary = monitor.summary();
                            jsonrpc_ok(
                                req.id,
                                json!({
                                    "event_id": monitor.event_id,
                                    "event_title": monitor.event_title,
                                    "outcomes": summary,
                                    "arbs": arbs
                                }),
                            )
                        }
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "multi_outcome.analyze failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm client init failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            // ==================== Event Registry ====================
            "events.upsert" => {
                if let Err(v) = require_write_enabled(req.id.clone()) {
                    println!("{}", v.to_string());
                    return Ok(());
                }
                let title = match parse_str(&params, "title") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let source = params
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual")
                    .to_string();

                let req_body = crate::strategy::registry::EventUpsertRequest {
                    title,
                    source,
                    event_id: params
                        .get("event_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    slug: params
                        .get("slug")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    domain: params
                        .get("domain")
                        .and_then(|v| v.as_str())
                        .unwrap_or("politics")
                        .to_string(),
                    strategy_hint: params
                        .get("strategy_hint")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    status: params
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    confidence: params.get("confidence").and_then(|v| v.as_f64()),
                    settlement_rule: params
                        .get("settlement_rule")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    end_time: params
                        .get("end_time")
                        .and_then(|v| v.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&chrono::Utc)),
                    market_slug: params
                        .get("market_slug")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    condition_id: params
                        .get("condition_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    token_ids: params.get("token_ids").cloned(),
                    outcome_prices: params.get("outcome_prices").cloned(),
                    metadata: params.get("metadata").cloned(),
                };

                match PostgresStore::new(&config.database.url, config.database.max_connections)
                    .await
                {
                    Ok(store) => match store.upsert_event(&req_body).await {
                        Ok(id) => jsonrpc_ok(req.id, json!({"id": id})),
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "events.upsert failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "db connect failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "events.list" => {
                let filter = crate::strategy::registry::EventFilter {
                    status: params
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    domain: params
                        .get("domain")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    strategy_hint: params
                        .get("strategy_hint")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    source: params
                        .get("source")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    limit: params.get("limit").and_then(|v| v.as_i64()),
                };

                match PostgresStore::new(&config.database.url, config.database.max_connections)
                    .await
                {
                    Ok(store) => match store.list_events(&filter).await {
                        Ok(events) => jsonrpc_ok(req.id, serde_json::to_value(events)?),
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "events.list failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "db connect failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            "events.update_status" => {
                if let Err(v) = require_write_enabled(req.id.clone()) {
                    println!("{}", v.to_string());
                    return Ok(());
                }
                let id = match params.get("id").and_then(|v| v.as_i64()) {
                    Some(v) => v as i32,
                    None => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": "missing/invalid integer param: id"}))
                            )
                        );
                        return Ok(());
                    }
                };
                let status_str = match parse_str(&params, "status") {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": e.to_string()}))
                            )
                        );
                        return Ok(());
                    }
                };
                let new_status = match crate::strategy::registry::EventStatus::from_str(&status_str)
                {
                    Some(s) => s,
                    None => {
                        println!(
                            "{}",
                            jsonrpc_err(
                                req.id,
                                -32602,
                                "invalid params",
                                Some(json!({"detail": format!("unknown status: {status_str}")}))
                            )
                        );
                        return Ok(());
                    }
                };

                match PostgresStore::new(&config.database.url, config.database.max_connections)
                    .await
                {
                    Ok(store) => match store.update_event_status(id, new_status).await {
                        Ok(()) => {
                            jsonrpc_ok(req.id, json!({"ok": true, "id": id, "status": status_str}))
                        }
                        Err(e) => jsonrpc_err(
                            req.id,
                            -32001,
                            "events.update_status failed",
                            Some(json!({"detail": e.to_string()})),
                        ),
                    },
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "db connect failed",
                        Some(json!({"detail": e.to_string()})),
                    ),
                }
            }

            _ => jsonrpc_err(
                req.id,
                -32601,
                "method not found",
                Some(json!({"method": req.method})),
            ),
        }
    };

    if let Err(e) = finalize_write_response(&method_name, idempotency_ctx.as_ref(), &params, &resp)
    {
        if idempotency_ctx.is_some() && resp.get("error").is_none() {
            eprintln!("rpc idempotency persistence failed: {}", e);
        } else {
            eprintln!("rpc write audit log failed: {}", e);
        }
    }

    // Keep output single-line JSON for robust remote parsing.
    println!("{}", resp.to_string());
    Ok(())
}
