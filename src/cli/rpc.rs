use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::signing::Wallet;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

mod event_methods;
mod intent_methods;
mod pm_read_methods;
mod write_support;

use event_methods::handle_event_method;
use intent_methods::handle_coordinator_intent_method;
use pm_read_methods::handle_pm_read_method;
use write_support::{
    finalize_write_response, hash_idempotency_params, idempotency_record_path, is_write_method,
    load_app_config, load_idempotency_record, parse_idempotency_key, parse_str,
    require_write_enabled, write_enabled, IdempotencyContext,
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
    } else if let Some(resp) =
        handle_coordinator_intent_method(&req.id, req.method.as_str(), &params).await
    {
        resp
    } else if let Some(resp) =
        handle_event_method(&req.id, req.method.as_str(), &params, &config, &rest_url).await
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
