use super::*;

fn invalid_params_response(request_id: Option<Value>, detail: impl ToString) -> Value {
    jsonrpc_err(
        request_id,
        -32602,
        "invalid params",
        Some(json!({"detail": detail.to_string()})),
    )
}

fn pm_client_init_failed(request_id: Option<Value>, err: impl ToString) -> Value {
    jsonrpc_err(
        request_id,
        -32001,
        "pm client init failed",
        Some(json!({"detail": err.to_string()})),
    )
}

fn pm_method_failed(request_id: Option<Value>, method: &str, err: impl ToString) -> Value {
    jsonrpc_err(
        request_id,
        -32001,
        method,
        Some(json!({"detail": err.to_string()})),
    )
}

fn serialize_ok<T: Serialize>(request_id: Option<Value>, value: T) -> Value {
    match serde_json::to_value(value) {
        Ok(v) => jsonrpc_ok(request_id, v),
        Err(e) => jsonrpc_err(
            request_id,
            -32001,
            "serialization failed",
            Some(json!({"detail": e.to_string()})),
        ),
    }
}

pub(super) async fn handle_pm_read_method(
    request_id: Option<Value>,
    method: &str,
    params: &Value,
    rest_url: &str,
    dry_run: bool,
) -> Option<Value> {
    Some(match method {
        "pm.resolve_event_id" => {
            let title = match parse_str(params, "title") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match discover_best_event_id_by_title(&title).await {
                Ok(event_id) => {
                    jsonrpc_ok(request_id, json!({ "title": title, "event_id": event_id }))
                }
                Err(e) => pm_method_failed(request_id, "pm.resolve_event_id failed", e),
            }
        }
        "pm.get_balance" => match build_pm_client(rest_url, dry_run).await {
            Ok(c) => match c.get_balance().await {
                Ok(r) => serialize_ok(request_id, r),
                Err(e) => pm_method_failed(request_id, "pm.get_balance failed", e),
            },
            Err(e) => pm_client_init_failed(request_id, e),
        },
        "pm.get_positions" => match build_pm_client(rest_url, dry_run).await {
            Ok(c) => match c.get_positions().await {
                Ok(r) => serialize_ok(request_id, r),
                Err(e) => pm_method_failed(request_id, "pm.get_positions failed", e),
            },
            Err(e) => pm_client_init_failed(request_id, e),
        },
        "pm.get_open_orders" => match build_pm_client(rest_url, dry_run).await {
            Ok(c) => match c.get_open_orders().await {
                Ok(r) => serialize_ok(request_id, r),
                Err(e) => pm_method_failed(request_id, "pm.get_open_orders failed", e),
            },
            Err(e) => pm_client_init_failed(request_id, e),
        },
        "pm.get_order" => {
            let order_id = match parse_str(params, "order_id") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match build_pm_client(rest_url, dry_run).await {
                Ok(c) => match c.get_order(&order_id).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.get_order failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.search_markets" => {
            let query = match parse_str(params, "query") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match build_pm_client(rest_url, true).await {
                Ok(c) => match c.search_markets(&query).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.search_markets failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.get_event_details" => {
            let event_id = match parse_str(params, "event_id") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match build_pm_client(rest_url, true).await {
                Ok(c) => match c.get_event_details(&event_id).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.get_event_details failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.get_market" => {
            let condition_id = match parse_str(params, "condition_id") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match build_pm_client(rest_url, true).await {
                Ok(c) => match c.get_market(&condition_id).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.get_market failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.get_order_book" => {
            let token_id = match parse_str(params, "token_id") {
                Ok(v) => v,
                Err(e) => return Some(invalid_params_response(request_id, e)),
            };
            match build_pm_client(rest_url, true).await {
                Ok(c) => match c.get_order_book(&token_id).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.get_order_book failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.get_trades" => {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            match build_pm_client(rest_url, true).await {
                Ok(c) => match c.get_trades(limit).await {
                    Ok(r) => serialize_ok(request_id, r),
                    Err(e) => pm_method_failed(request_id, "pm.get_trades failed", e),
                },
                Err(e) => pm_client_init_failed(request_id, e),
            }
        }
        "pm.get_account_summary" => match build_pm_client(rest_url, dry_run).await {
            Ok(c) => match c.get_account_summary().await {
                Ok(r) => serialize_ok(request_id, r),
                Err(e) => pm_method_failed(request_id, "pm.get_account_summary failed", e),
            },
            Err(e) => pm_client_init_failed(request_id, e),
        },
        _ => return None,
    })
}
