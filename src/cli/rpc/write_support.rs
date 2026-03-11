use super::jsonrpc_err;
use crate::config::AppConfig;
use crate::domain::Domain;
use crate::error::{PloyError, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RpcIdempotencyRecord {
    pub(super) method: String,
    pub(super) params_hash: String,
    pub(super) response: Value,
    pub(super) created_at: String,
}

#[derive(Debug, Clone)]
pub(super) struct IdempotencyContext {
    pub(super) key: String,
    pub(super) params_hash: String,
    pub(super) record_path: PathBuf,
}

pub(super) fn write_enabled() -> bool {
    matches!(
        std::env::var("PLOY_RPC_WRITE_ENABLED")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

pub(super) fn require_write_enabled(id: Option<Value>) -> std::result::Result<(), Value> {
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

pub(super) fn parse_str(v: &Value, key: &str) -> std::result::Result<String, PloyError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| PloyError::Validation(format!("missing/invalid string param: {key}")))
}

pub(super) fn parse_u64(v: &Value, key: &str) -> std::result::Result<u64, PloyError> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| PloyError::Validation(format!("missing/invalid integer param: {key}")))
}

pub(super) fn parse_decimal(v: &Value, key: &str) -> std::result::Result<Decimal, PloyError> {
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

pub(super) fn parse_optional_decimal(
    v: &Value,
    key: &str,
) -> std::result::Result<Option<Decimal>, PloyError> {
    if v.get(key).is_none() {
        return Ok(None);
    }
    parse_decimal(v, key).map(Some)
}

pub(super) fn parse_optional_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

pub(super) fn load_app_config(config_path: &Path) -> std::result::Result<AppConfig, PloyError> {
    AppConfig::load_from(config_path).map_err(PloyError::from)
}

pub(super) fn parse_domain(value: Option<&str>) -> std::result::Result<Domain, PloyError> {
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

fn coordinator_intent_ingress_url() -> String {
    std::env::var("PLOY_RPC_COORDINATOR_INTENT_URL")
        .or_else(|_| std::env::var("PLOY_COORDINATOR_INTENT_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8081/api/sidecar/intents".to_string())
}

fn coordinator_intent_ingress_token() -> Option<String> {
    std::env::var("PLOY_RPC_SIDECAR_AUTH_TOKEN")
        .or_else(|_| std::env::var("PLOY_SIDECAR_AUTH_TOKEN"))
        .or_else(|_| std::env::var("PLOY_API_SIDECAR_AUTH_TOKEN"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(super) async fn submit_intent_via_coordinator(payload: &Value) -> Result<Value> {
    let url = coordinator_intent_ingress_url();
    let client = coordinator_ingress_http_client()?;

    let mut request = client.post(&url).json(payload);
    if let Some(token) = coordinator_intent_ingress_token() {
        request = request.header("x-ploy-sidecar-token", token);
    }

    let response = request.send().await.map_err(|e| {
        PloyError::Internal(format!(
            "failed to reach coordinator intent ingress {}: {}",
            url, e
        ))
    })?;
    let status = response.status();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| "<empty>".to_string());

    if !status.is_success() {
        return Err(PloyError::Internal(format!(
            "coordinator intent ingress rejected request (status={}): {}",
            status, text
        )));
    }

    serde_json::from_str(&text)
        .or_else(|_| Ok(json!({ "raw": text })))
        .map_err(|e: serde_json::Error| PloyError::Internal(format!("invalid ingress JSON: {}", e)))
}

fn coordinator_ingress_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(16)
            .tcp_nodelay(true)
            .build()
            .map_err(|e| format!("failed to build http client: {}", e))
    });

    client
        .as_ref()
        .map_err(|msg| PloyError::Internal(msg.clone()))
}

pub(super) fn is_write_method(method: &str) -> bool {
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

pub(super) fn idempotency_record_path(method: &str, key: &str) -> PathBuf {
    let mut path = rpc_state_dir();
    path.push("idempotency");
    path.push(method.replace('.', "_"));
    path.push(format!("{}.json", sanitize_idempotency_key(key)));
    path
}

pub(super) fn hash_idempotency_params(params: &Value) -> std::result::Result<String, PloyError> {
    let mut normalized = params.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("idempotency_key");
    }
    let bytes = serde_json::to_vec(&normalized)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn load_idempotency_record(
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

pub(super) fn finalize_write_response(
    method: &str,
    ctx: Option<&IdempotencyContext>,
    params: &Value,
    response: &Value,
) -> std::result::Result<(), PloyError> {
    if let Some(ctx) = ctx {
        if response.get("error").is_none() {
            let record = RpcIdempotencyRecord {
                method: method.to_string(),
                params_hash: ctx.params_hash.clone(),
                response: response.clone(),
                created_at: Utc::now().to_rfc3339(),
            };
            save_idempotency_record(&ctx.record_path, &record)?;
        }

        append_write_audit_log(method, Some(&ctx.key), params, response)?;
    } else if is_write_method(method) {
        append_write_audit_log(method, None, params, response)?;
    }

    Ok(())
}

pub(super) fn parse_idempotency_key(params: &Value) -> std::result::Result<String, PloyError> {
    let key = parse_str(params, "idempotency_key")?;
    if key.trim().is_empty() {
        return Err(PloyError::Validation(
            "missing/invalid string param: idempotency_key".to_string(),
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::{hash_idempotency_params, sanitize_idempotency_key};
    use serde_json::json;

    #[test]
    fn sanitize_idempotency_key_normalizes_unsafe_chars() {
        assert_eq!(
            sanitize_idempotency_key("alpha/beta gamma?delta"),
            "alpha_beta_gamma_delta"
        );
    }

    #[test]
    fn hash_idempotency_params_ignores_idempotency_key() {
        let left = hash_idempotency_params(&json!({
            "deployment_id": "dep-1",
            "size": 10,
            "idempotency_key": "first"
        }))
        .unwrap();
        let right = hash_idempotency_params(&json!({
            "deployment_id": "dep-1",
            "size": 10,
            "idempotency_key": "second"
        }))
        .unwrap();

        assert_eq!(left, right);
    }
}
