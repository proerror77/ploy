use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::postgres::PostgresStore;
use crate::adapters::PolymarketClient;
use crate::error::Result;
use crate::signing::Wallet;
use crate::strategy::event_edge::{discover_best_event_id_by_title, scan_event_edge_once};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use crate::strategy::multi_outcome::fetch_multi_outcome_event;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

mod intent_methods;
mod pm_read_methods;
mod write_support;

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
