use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::postgres::PostgresStore;
use crate::adapters::PolymarketClient;
use crate::config::AppConfig;
use crate::domain::{OrderRequest, OrderSide, Side};
use crate::error::{PloyError, Result};
use crate::platform::{
    Domain, OrderIntent, RiskCheckResult, RiskConfig as PlatformRiskConfig, RiskDecision,
    RiskDecisionStatus, RiskGate, TradeIntent,
};
use crate::signing::Wallet;
use crate::strategy::event_edge::{discover_best_event_id_by_title, scan_event_edge_once};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::multi_outcome::fetch_multi_outcome_event;
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;
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

#[derive(Debug, Serialize, Deserialize)]
struct RpcIdempotencyRecord {
    method: String,
    params_hash: String,
    response: Value,
    created_at: String,
}

#[derive(Debug, Clone)]
struct IdempotencyContext {
    key: String,
    params_hash: String,
    record_path: PathBuf,
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

fn write_enabled() -> bool {
    matches!(
        std::env::var("PLOY_RPC_WRITE_ENABLED")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

fn require_write_enabled(id: Option<Value>) -> std::result::Result<(), Value> {
    if write_enabled() {
        return Ok(());
    }
    Err(jsonrpc_err(
        id,
        -32010,
        "write operations disabled (set PLOY_RPC_WRITE_ENABLED=true)",
        None,
    ))
}

fn parse_bool(v: &Value, key: &str) -> std::result::Result<bool, PloyError> {
    v.get(key)
        .and_then(|x| x.as_bool())
        .ok_or_else(|| PloyError::Validation(format!("missing/invalid boolean param: {key}")))
}

fn parse_str(v: &Value, key: &str) -> std::result::Result<String, PloyError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| PloyError::Validation(format!("missing/invalid string param: {key}")))
}

fn parse_u64(v: &Value, key: &str) -> std::result::Result<u64, PloyError> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| PloyError::Validation(format!("missing/invalid integer param: {key}")))
}

fn parse_decimal(v: &Value, key: &str) -> std::result::Result<Decimal, PloyError> {
    let Some(x) = v.get(key) else {
        return Err(PloyError::Validation(format!(
            "missing/invalid decimal param: {key}"
        )));
    };
    match x {
        Value::String(s) => Decimal::from_str(s)
            .map_err(|_| PloyError::Validation(format!("missing/invalid decimal param: {key}"))),
        Value::Number(n) => Decimal::from_str(&n.to_string())
            .map_err(|_| PloyError::Validation(format!("missing/invalid decimal param: {key}"))),
        _ => Err(PloyError::Validation(format!(
            "missing/invalid decimal param: {key}"
        ))),
    }
}

fn parse_optional_decimal(v: &Value, key: &str) -> std::result::Result<Option<Decimal>, PloyError> {
    if v.get(key).is_none() {
        return Ok(None);
    }
    parse_decimal(v, key).map(Some)
}

fn parse_optional_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn load_app_config(config_path: &Path) -> std::result::Result<AppConfig, PloyError> {
    AppConfig::load_from(config_path).map_err(PloyError::from)
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

fn parse_domain(value: Option<&str>) -> std::result::Result<Domain, PloyError> {
    match value
        .unwrap_or("crypto")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "crypto" => Ok(Domain::Crypto),
        "sports" => Ok(Domain::Sports),
        "politics" => Ok(Domain::Politics),
        "economics" => Ok(Domain::Economics),
        other => Err(PloyError::Validation(format!(
            "invalid domain '{}', expected crypto|sports|politics|economics",
            other
        ))),
    }
}

fn build_risk_config(config: &AppConfig) -> PlatformRiskConfig {
    let max_positions = config.risk.max_positions.max(1);
    PlatformRiskConfig {
        max_platform_exposure: config.risk.max_single_exposure_usd * Decimal::from(max_positions),
        max_consecutive_failures: config.risk.max_consecutive_failures,
        daily_loss_limit: config.risk.daily_loss_limit_usd,
        max_spread_bps: config.execution.max_spread_bps,
        critical_bypass_exposure: true,
        ..Default::default()
    }
}

fn risk_result_to_decision(result: &RiskCheckResult) -> RiskDecision {
    match result {
        RiskCheckResult::Passed => RiskDecision {
            status: RiskDecisionStatus::Allow,
            reason_code: None,
            message: None,
            suggested_max_size: None,
        },
        RiskCheckResult::Blocked(reason) => RiskDecision {
            status: RiskDecisionStatus::Deny,
            reason_code: Some("blocked".to_string()),
            message: Some(reason.to_string()),
            suggested_max_size: None,
        },
        RiskCheckResult::Adjusted(s) => RiskDecision {
            status: RiskDecisionStatus::Throttle,
            reason_code: Some("adjusted".to_string()),
            message: Some(s.reason.clone()),
            suggested_max_size: Some(s.max_shares),
        },
    }
}

async fn execute_order_via_gateway(
    config: &AppConfig,
    request: &OrderRequest,
) -> Result<crate::strategy::executor::ExecutionResult> {
    let client = build_pm_client(&config.market.rest_url, config.dry_run.enabled).await?;
    let mut executor = OrderExecutor::new(client, config.execution.clone());

    if let Ok(store) =
        PostgresStore::new(&config.database.url, config.database.max_connections).await
    {
        let idem = Arc::new(IdempotencyManager::new_with_account(
            store,
            config.account.id.clone(),
        ));
        executor = executor.with_idempotency(idem);
    }

    executor.execute(request).await
}

fn is_write_method(method: &str) -> bool {
    matches!(
        method,
        "pm.submit_limit"
            | "gateway.submit_intent"
            | "pm.cancel_order"
            | "events.upsert"
            | "events.update_status"
    )
}

fn rpc_state_dir() -> PathBuf {
    std::env::var("PLOY_RPC_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/rpc"))
}

fn sanitize_idempotency_key(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn idempotency_record_path(method: &str, key: &str) -> PathBuf {
    let mut path = rpc_state_dir();
    path.push("idempotency");
    path.push(method.replace('.', "_"));
    path.push(format!("{}.json", sanitize_idempotency_key(key)));
    path
}

fn hash_idempotency_params(params: &Value) -> std::result::Result<String, PloyError> {
    let mut normalized = params.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("idempotency_key");
    }
    let bytes = serde_json::to_vec(&normalized)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn load_idempotency_record(
    path: &Path,
) -> std::result::Result<Option<RpcIdempotencyRecord>, PloyError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    let record = serde_json::from_str::<RpcIdempotencyRecord>(&text)?;
    Ok(Some(record))
}

fn save_idempotency_record(
    path: &Path,
    record: &RpcIdempotencyRecord,
) -> std::result::Result<(), PloyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(record)?)?;
    Ok(())
}

fn append_write_audit_log(
    method: &str,
    idempotency_key: Option<&str>,
    params: &Value,
    response: &Value,
) -> std::result::Result<(), PloyError> {
    let mut path = rpc_state_dir();
    path.push("audit");
    fs::create_dir_all(&path)?;
    path.push(format!("{}.jsonl", Utc::now().format("%Y-%m-%d")));

    let mut params_for_log = params.clone();
    if let Some(obj) = params_for_log.as_object_mut() {
        for secret_key in ["private_key", "api_secret", "passphrase"] {
            if obj.contains_key(secret_key) {
                obj.insert(
                    secret_key.to_string(),
                    Value::String("***redacted***".to_string()),
                );
            }
        }
    }

    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "method": method,
        "idempotency_key": idempotency_key,
        "params": params_for_log,
        "response": response
    });

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

fn parse_idempotency_key(params: &Value) -> std::result::Result<String, PloyError> {
    let key = parse_str(params, "idempotency_key")?;
    if key.trim().is_empty() {
        return Err(PloyError::Validation(
            "missing/invalid string param: idempotency_key".to_string(),
        ));
    }
    Ok(key)
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

    let resp = match req.method.as_str() {
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

        "pm.resolve_event_id" => {
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
            match discover_best_event_id_by_title(&title).await {
                Ok(event_id) => jsonrpc_ok(req.id, json!({ "title": title, "event_id": event_id })),
                Err(e) => jsonrpc_err(
                    req.id,
                    -32001,
                    "pm.resolve_event_id failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            }
        }

        "pm.get_balance" => match build_pm_client(&rest_url, dry_run).await {
            Ok(c) => match c.get_balance().await {
                Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                Err(e) => jsonrpc_err(
                    req.id,
                    -32001,
                    "pm.get_balance failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                req.id,
                -32001,
                "pm client init failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },

        "pm.get_positions" => match build_pm_client(&rest_url, dry_run).await {
            Ok(c) => match c.get_positions().await {
                Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                Err(e) => jsonrpc_err(
                    req.id,
                    -32001,
                    "pm.get_positions failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                req.id,
                -32001,
                "pm client init failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },

        "pm.get_open_orders" => match build_pm_client(&rest_url, dry_run).await {
            Ok(c) => match c.get_open_orders().await {
                Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                Err(e) => jsonrpc_err(
                    req.id,
                    -32001,
                    "pm.get_open_orders failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                req.id,
                -32001,
                "pm client init failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },

        "pm.get_order" => {
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
                Ok(c) => match c.get_order(&order_id).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.get_order failed",
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

        "pm.search_markets" => {
            let query = match parse_str(&params, "query") {
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
                Ok(c) => match c.search_markets(&query).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.search_markets failed",
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

        "pm.get_event_details" => {
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
                Ok(c) => match c.get_event_details(&event_id).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.get_event_details failed",
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

        "pm.get_market" => {
            let condition_id = match parse_str(&params, "condition_id") {
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
                Ok(c) => match c.get_market(&condition_id).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.get_market failed",
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

        "pm.get_order_book" => {
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
            match build_pm_client(&rest_url, true).await {
                Ok(c) => match c.get_order_book(&token_id).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.get_order_book failed",
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

        "pm.get_trades" => {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            match build_pm_client(&rest_url, true).await {
                Ok(c) => match c.get_trades(limit).await {
                    Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                    Err(e) => jsonrpc_err(
                        req.id,
                        -32001,
                        "pm.get_trades failed",
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

        "pm.get_account_summary" => match build_pm_client(&rest_url, dry_run).await {
            Ok(c) => match c.get_account_summary().await {
                Ok(r) => jsonrpc_ok(req.id, serde_json::to_value(r)?),
                Err(e) => jsonrpc_err(
                    req.id,
                    -32001,
                    "pm.get_account_summary failed",
                    Some(json!({"detail": e.to_string()})),
                ),
            },
            Err(e) => jsonrpc_err(
                req.id,
                -32001,
                "pm client init failed",
                Some(json!({"detail": e.to_string()})),
            ),
        },

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
            let deployment_id = parse_optional_str(&params, "deployment_id")
                .unwrap_or_else(|| "openclaw_rpc.default".to_string());
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
            let order_intent: OrderIntent = trade_intent.clone().into_order_intent();
            let risk_gate = RiskGate::new(build_risk_config(&config));
            let risk_result = risk_gate.check_order(&order_intent).await;
            let risk_decision = risk_result_to_decision(&risk_result);

            if !risk_result.is_passed() {
                println!(
                    "{}",
                    jsonrpc_err(
                        req.id,
                        -32012,
                        "risk check failed",
                        Some(json!({
                            "risk_decision": risk_decision,
                            "deployment_id": deployment_id,
                            "agent_id": agent_id,
                        })),
                    )
                );
                return Ok(());
            }

            let req_order = match order_side {
                OrderSide::Buy => {
                    OrderRequest::buy_limit(token_id, market_side, shares, limit_price)
                }
                OrderSide::Sell => {
                    OrderRequest::sell_limit(token_id, market_side, shares, limit_price)
                }
            };
            let mut req_order = req_order;
            req_order.client_order_id = format!("intent:{}", trade_intent.intent_id);
            req_order.idempotency_key = Some(idempotency_key);

            match execute_order_via_gateway(&config, &req_order).await {
                Ok(exec) => jsonrpc_ok(
                    req.id,
                    json!({
                        "execution": exec,
                        "risk_decision": risk_decision,
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
            let agent_id =
                parse_optional_str(&params, "agent_id").unwrap_or_else(|| "openclaw_rpc".into());
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

            let order_intent = trade_intent.clone().into_order_intent();
            let risk_gate = RiskGate::new(build_risk_config(&config));
            let risk_result = risk_gate.check_order(&order_intent).await;
            let risk_decision = risk_result_to_decision(&risk_result);
            if !risk_result.is_passed() {
                println!(
                    "{}",
                    jsonrpc_err(
                        req.id,
                        -32012,
                        "risk check failed",
                        Some(json!({ "risk_decision": risk_decision })),
                    )
                );
                return Ok(());
            }

            let mut request = if trade_intent.is_buy {
                OrderRequest::buy_limit(
                    trade_intent.token_id.clone(),
                    trade_intent.side,
                    trade_intent.size,
                    trade_intent.price_limit,
                )
            } else {
                OrderRequest::sell_limit(
                    trade_intent.token_id.clone(),
                    trade_intent.side,
                    trade_intent.size,
                    trade_intent.price_limit,
                )
            };
            request.client_order_id = format!("intent:{}", trade_intent.intent_id);
            request.idempotency_key = Some(idempotency_key);

            match execute_order_via_gateway(&config, &request).await {
                Ok(exec) => jsonrpc_ok(
                    req.id,
                    json!({
                        "execution": exec,
                        "risk_decision": risk_decision,
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

            match PostgresStore::new(&config.database.url, config.database.max_connections).await {
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

            match PostgresStore::new(&config.database.url, config.database.max_connections).await {
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
            let new_status = match crate::strategy::registry::EventStatus::from_str(&status_str) {
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

            match PostgresStore::new(&config.database.url, config.database.max_connections).await {
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
    };

    if let Some(ctx) = idempotency_ctx {
        if resp.get("error").is_none() {
            let record = RpcIdempotencyRecord {
                method: method_name.clone(),
                params_hash: ctx.params_hash,
                response: resp.clone(),
                created_at: Utc::now().to_rfc3339(),
            };
            if let Err(e) = save_idempotency_record(&ctx.record_path, &record) {
                eprintln!("rpc idempotency persistence failed: {}", e);
            }
        }

        if let Err(e) = append_write_audit_log(&method_name, Some(&ctx.key), &params, &resp) {
            eprintln!("rpc write audit log failed: {}", e);
        }
    } else if is_write_method(&method_name) {
        if let Err(e) = append_write_audit_log(&method_name, None, &params, &resp) {
            eprintln!("rpc write audit log failed: {}", e);
        }
    }

    // Keep output single-line JSON for robust remote parsing.
    println!("{}", resp.to_string());
    Ok(())
}
