use super::write_support::{parse_str, require_write_enabled};
use super::{build_pm_client, jsonrpc_err, jsonrpc_ok};
use crate::adapters::postgres::PostgresStore;
use crate::config::AppConfig;
use crate::strategy::event_edge::{discover_best_event_id_by_title, scan_event_edge_once};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use crate::strategy::multi_outcome::fetch_multi_outcome_event;
use crate::strategy::registry::{EventFilter, EventStatus, EventUpsertRequest};
use serde_json::{json, Value};

pub(super) async fn handle_event_method(
    request_id: &Option<Value>,
    method: &str,
    params: &Value,
    config: &AppConfig,
    rest_url: &str,
) -> Option<Value> {
    match method {
        "event_edge.scan" => Some(handle_event_edge_scan(request_id, params, rest_url).await),
        "multi_outcome.analyze" => {
            Some(handle_multi_outcome_analyze(request_id, params, rest_url).await)
        }
        "events.upsert" => Some(handle_events_upsert(request_id, params, config).await),
        "events.list" => Some(handle_events_list(request_id, params, config).await),
        "events.update_status" => {
            Some(handle_events_update_status(request_id, params, config).await)
        }
        _ => None,
    }
}

async fn handle_event_edge_scan(
    request_id: &Option<Value>,
    params: &Value,
    rest_url: &str,
) -> Value {
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
                return jsonrpc_err(
                    request_id.clone(),
                    -32001,
                    "event discovery failed",
                    Some(json!({"detail": e.to_string()})),
                );
            }
        },
        _ => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": "event_id or title required"})),
            );
        }
    };

    let arena = match fetch_arena_text_snapshot().await {
        Ok(a) => a,
        Err(e) => {
            return jsonrpc_err(
                request_id.clone(),
                -32001,
                "arena fetch failed",
                Some(json!({"detail": e.to_string()})),
            );
        }
    };

    match build_pm_client(rest_url, true).await {
        Ok(c) => match scan_event_edge_once(&c, &event_id, Some(arena)).await {
            Ok(r) => match serde_json::to_value(r) {
                Ok(value) => jsonrpc_ok(request_id.clone(), value),
                Err(e) => jsonrpc_err(
                    request_id.clone(),
                    -32001,
                    "event_edge.scan failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                request_id.clone(),
                -32001,
                "event_edge.scan failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            "pm client init failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

async fn handle_multi_outcome_analyze(
    request_id: &Option<Value>,
    params: &Value,
    rest_url: &str,
) -> Value {
    let event_id = match parse_str(params, "event_id") {
        Ok(v) => v,
        Err(e) => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": e.to_string()})),
            );
        }
    };

    match build_pm_client(rest_url, true).await {
        Ok(c) => match fetch_multi_outcome_event(&c, &event_id).await {
            Ok(monitor) => {
                let arbs = monitor.find_all_arbitrage();
                let summary = monitor.summary();
                jsonrpc_ok(
                    request_id.clone(),
                    json!({
                        "event_id": monitor.event_id,
                        "event_title": monitor.event_title,
                        "outcomes": summary,
                        "arbs": arbs
                    }),
                )
            }
            Err(e) => jsonrpc_err(
                request_id.clone(),
                -32001,
                "multi_outcome.analyze failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            "pm client init failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

async fn handle_events_upsert(
    request_id: &Option<Value>,
    params: &Value,
    config: &AppConfig,
) -> Value {
    if let Err(v) = require_write_enabled(request_id.clone()) {
        return v;
    }
    let title = match parse_str(params, "title") {
        Ok(v) => v,
        Err(e) => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": e.to_string()})),
            );
        }
    };
    let source = params
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
        .to_string();

    let req_body = EventUpsertRequest {
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

    match PostgresStore::new(&config.database.url, config.database.max_connections).await {
        Ok(store) => match store.upsert_event(&req_body).await {
            Ok(id) => jsonrpc_ok(request_id.clone(), json!({"id": id})),
            Err(e) => jsonrpc_err(
                request_id.clone(),
                -32001,
                "events.upsert failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            "db connect failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

async fn handle_events_list(
    request_id: &Option<Value>,
    params: &Value,
    config: &AppConfig,
) -> Value {
    let filter = EventFilter {
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

    match PostgresStore::new(&config.database.url, config.database.max_connections).await {
        Ok(store) => match store.list_events(&filter).await {
            Ok(events) => match serde_json::to_value(events) {
                Ok(value) => jsonrpc_ok(request_id.clone(), value),
                Err(e) => jsonrpc_err(
                    request_id.clone(),
                    -32001,
                    "events.list failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                request_id.clone(),
                -32001,
                "events.list failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            "db connect failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

async fn handle_events_update_status(
    request_id: &Option<Value>,
    params: &Value,
    config: &AppConfig,
) -> Value {
    if let Err(v) = require_write_enabled(request_id.clone()) {
        return v;
    }
    let id = match params.get("id").and_then(|v| v.as_i64()) {
        Some(v) => v as i32,
        None => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": "missing/invalid integer param: id"})),
            );
        }
    };
    let status_str = match parse_str(params, "status") {
        Ok(v) => v,
        Err(e) => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": e.to_string()})),
            );
        }
    };
    let new_status = match EventStatus::from_str(&status_str) {
        Some(s) => s,
        None => {
            return jsonrpc_err(
                request_id.clone(),
                -32602,
                "invalid params",
                Some(json!({"detail": format!("unknown status: {status_str}")})),
            );
        }
    };

    match PostgresStore::new(&config.database.url, config.database.max_connections).await {
        Ok(store) => match store.update_event_status(id, new_status).await {
            Ok(()) => jsonrpc_ok(
                request_id.clone(),
                json!({"ok": true, "id": id, "status": status_str}),
            ),
            Err(e) => jsonrpc_err(
                request_id.clone(),
                -32001,
                "events.update_status failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },
        Err(e) => jsonrpc_err(
            request_id.clone(),
            -32001,
            "db connect failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}
