//! Sidecar REST endpoints — bridge between Claude Agent SDK and Rust trading core
//!
//! These endpoints are called by the TypeScript sidecar (ploy-sidecar) which uses
//! Claude Agent SDK + MCP tools for research, then routes order decisions through
//! Grok and the Coordinator.
//!
//! Endpoints:
//! - POST /api/sidecar/grok/decision — Unified Grok decision with full context
//! - POST /api/sidecar/intents      — Unified intent ingress (OpenClaw/RPC/scripts)
//! - POST /api/sidecar/orders       — Submit order through Coordinator
//! - GET  /api/sidecar/positions     — Current positions from DB
//! - GET  /api/sidecar/risk          — Risk state from Coordinator

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use tracing::{info, warn};
use uuid::Uuid;

use crate::api::{auth::ensure_sidecar_authorized, state::AppState};
use crate::domain::market::Side;
use crate::platform::{Domain, OrderIntent, OrderPriority};

mod grok_decision;
mod ingress;
mod read_side;
mod types;
mod write_side;

pub(crate) use ingress::table_has_account_scope;
pub(super) use types::{SidecarCircuitBreakerEvent, SidecarPosition, SidecarRiskPosition, SidecarRiskState};

// Re-export handler functions for routes
pub use grok_decision::sidecar_grok_decision;
pub use read_side::{sidecar_get_positions, sidecar_get_risk};
pub use write_side::{sidecar_submit_intent, sidecar_submit_order};
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;
    use crate::platform::{StrategyLifecycleStage, StrategyProductType, Timeframe};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn sample_deployment(lifecycle_stage: StrategyLifecycleStage) -> StrategyDeployment {
        StrategyDeployment {
            id: "deploy.test.crypto".to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v2.1.0".to_string(),
            domain: Domain::Crypto,
            market_selector: MarketSelector::Static {
                symbol: None,
                series_id: None,
                market_slug: Some("btc-price-series-15m".to_string()),
            },
            timeframe: Timeframe::M15,
            enabled: true,
            state: crate::platform::DeploymentState::Enabled,
            allocator_profile: "balanced".to_string(),
            risk_profile: "default".to_string(),
            priority: 80,
            cooldown_secs: 30,
            account_ids: Vec::new(),
            execution_mode: crate::platform::DeploymentExecutionMode::Any,
            lifecycle_stage,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: Some(Utc::now()),
            last_evaluation_score: Some(0.73),
        }
    }

    #[test]
    fn parse_domain_rejects_unknown_values() {
        assert!(parse_sidecar_domain(Some("crypto"), Domain::Sports).is_ok());
        assert!(parse_sidecar_domain(Some("sports"), Domain::Crypto).is_ok());
        assert!(parse_sidecar_domain(Some("custom:42"), Domain::Crypto).is_ok());
        assert!(parse_sidecar_domain(Some("bad-domain"), Domain::Crypto).is_err());
    }

    #[test]
    fn parse_side_rejects_unknown_values() {
        assert_eq!(parse_binary_side(Some("UP")).unwrap(), Side::Up);
        assert_eq!(parse_binary_side(Some("NO")).unwrap(), Side::Down);
        assert!(parse_binary_side(Some("LEFT")).is_err());
    }

    #[test]
    fn parse_order_side_rejects_unknown_values() {
        assert_eq!(parse_is_buy(Some("BUY"), None).unwrap(), true);
        assert_eq!(parse_is_buy(Some("SELL"), None).unwrap(), false);
        assert!(parse_is_buy(Some("HOLD"), None).is_err());
    }

    #[test]
    fn parse_order_side_rejects_conflicting_is_buy() {
        assert!(parse_is_buy(Some("BUY"), Some(false)).is_err());
        assert!(parse_is_buy(Some("SELL"), Some(true)).is_err());
        assert_eq!(parse_is_buy(Some("SELL"), Some(false)).unwrap(), false);
    }

    #[test]
    fn sidecar_auth_fails_closed_when_token_not_configured() {
        let _guard = ENV_LOCK.lock().unwrap();
        let keys = [
            "PLOY_SIDECAR_AUTH_TOKEN",
            "PLOY_API_SIDECAR_AUTH_TOKEN",
            "PLOY_SIDECAR_AUTH_REQUIRED",
            "PLOY_GATEWAY_ONLY",
            "PLOY_ENFORCE_GATEWAY_ONLY",
            "PLOY_ENFORCE_COORDINATOR_GATEWAY_ONLY",
        ];
        let prev: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();

        for k in keys {
            set_env(k, None);
        }

        let result = ensure_sidecar_authorized(&HeaderMap::new());
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(msg.contains("not configured"));

        for (k, v) in prev {
            set_env(&k, v.as_deref());
        }
    }

    #[test]
    fn sidecar_auth_accepts_valid_bearer_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_SIDECAR_AUTH_TOKEN";
        let prev = std::env::var(key).ok();
        set_env(key, Some("expected-token"));

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer expected-token"),
        );

        let result = ensure_sidecar_authorized(&headers);
        assert!(result.is_ok());

        set_env(key, prev.as_deref());
    }

    #[test]
    fn sidecar_auth_rejects_invalid_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_SIDECAR_AUTH_TOKEN";
        let prev = std::env::var(key).ok();
        set_env(key, Some("expected-token"));

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ploy-sidecar-token",
            axum::http::HeaderValue::from_static("wrong-token"),
        );

        let result = ensure_sidecar_authorized(&headers);
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(msg.contains("missing/invalid token"));

        set_env(key, prev.as_deref());
    }

    #[test]
    fn non_live_deployment_ingress_is_blocked_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS";
        let prev = std::env::var(key).ok();
        set_env(key, None);

        let deployment = sample_deployment(StrategyLifecycleStage::Paper);
        let err = ensure_deployment_accepts_live_ingress(&deployment)
            .expect_err("paper lifecycle should be blocked without override");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("lifecycle_stage=paper"));

        set_env(key, prev.as_deref());
    }

    #[test]
    fn non_live_deployment_ingress_can_be_enabled_for_migration() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = "PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS";
        let prev = std::env::var(key).ok();
        set_env(key, Some("true"));

        let deployment = sample_deployment(StrategyLifecycleStage::Backtest);
        assert!(ensure_deployment_accepts_live_ingress(&deployment).is_ok());

        set_env(key, prev.as_deref());
    }

    #[test]
    fn deployment_metadata_includes_strategy_contract_fields() {
        let deployment = sample_deployment(StrategyLifecycleStage::Live);
        let mut metadata = HashMap::new();
        apply_deployment_metadata(&mut metadata, &deployment);

        assert_eq!(
            metadata.get("strategy_version").map(String::as_str),
            Some("v2.1.0")
        );
        assert_eq!(
            metadata.get("lifecycle_stage").map(String::as_str),
            Some("live")
        );
        assert_eq!(
            metadata.get("product_type").map(String::as_str),
            Some("binary_option")
        );
    }
}
