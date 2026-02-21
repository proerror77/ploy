use axum::{extract::State, Json};
use serde::Serialize;

use crate::api::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityEndpoint {
    pub path: String,
    pub method: String,
    pub description: String,
    pub auth: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityFlags {
    pub coordinator_present: bool,
    pub coordinator_only_live_execution: bool,
    pub governance_api_available: bool,
    pub deployment_gate_required: bool,
    pub strategy_lifecycle_gate_required: bool,
    pub sidecar_auth_required: bool,
    pub sidecar_orders_live_enabled: bool,
    pub openclaw_mode: bool,
    pub internal_agents_hard_disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityResponse {
    pub architecture_mode: String,
    pub canonical_live_ingress: String,
    pub flags: CapabilityFlags,
    pub endpoints: Vec<CapabilityEndpoint>,
}

fn parse_boolish(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn env_bool_default_true(key: &str) -> bool {
    match std::env::var(key) {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => true,
    }
}

fn env_bool_default_false(key: &str) -> bool {
    match std::env::var(key) {
        Ok(raw) => parse_boolish(&raw),
        Err(_) => false,
    }
}

fn capability_endpoints(governance_available: bool) -> Vec<CapabilityEndpoint> {
    let mut endpoints = vec![
        CapabilityEndpoint {
            path: "/api/capabilities".to_string(),
            method: "GET".to_string(),
            description: "Machine-readable runtime and control-plane capabilities".to_string(),
            auth: "none".to_string(),
        },
        CapabilityEndpoint {
            path: "/api/sidecar/intents".to_string(),
            method: "POST".to_string(),
            description: "Canonical live ingress for agent/order intents".to_string(),
            auth: "x-ploy-sidecar-token".to_string(),
        },
        CapabilityEndpoint {
            path: "/api/sidecar/risk".to_string(),
            method: "GET".to_string(),
            description: "Runtime risk snapshot for external schedulers".to_string(),
            auth: "x-ploy-sidecar-token".to_string(),
        },
        CapabilityEndpoint {
            path: "/api/deployments".to_string(),
            method: "GET/PUT".to_string(),
            description: "Deployment matrix CRUD for control-plane".to_string(),
            auth: "x-ploy-admin-token".to_string(),
        },
        CapabilityEndpoint {
            path: "/api/strategies/control|/api/strategies/control/:id".to_string(),
            method: "GET/PUT".to_string(),
            description: "Strategy control projection and targeted lifecycle/version mutation"
                .to_string(),
            auth: "x-ploy-admin-token".to_string(),
        },
        CapabilityEndpoint {
            path: "/api/system/pause|resume|halt".to_string(),
            method: "POST".to_string(),
            description: "Global/domain runtime control commands".to_string(),
            auth: "x-ploy-admin-token".to_string(),
        },
    ];

    if governance_available {
        endpoints.push(CapabilityEndpoint {
            path: "/api/governance/status|policy|policy/history".to_string(),
            method: "GET/PUT".to_string(),
            description: "Account-level governance policy and ledger snapshots".to_string(),
            auth: "x-ploy-admin-token".to_string(),
        });
    }

    endpoints
}

/// GET /api/capabilities
pub async fn get_capabilities(State(state): State<AppState>) -> Json<CapabilityResponse> {
    let coordinator_present = state.coordinator.is_some();
    let governance_available = coordinator_present;

    let deployment_gate_required = env_bool_default_true("PLOY_DEPLOYMENT_GATE_REQUIRED");
    let strategy_lifecycle_gate_required =
        !env_bool_default_false("PLOY_ALLOW_NON_LIVE_DEPLOYMENT_INGRESS");
    let sidecar_auth_required = env_bool_default_true("PLOY_SIDECAR_AUTH_REQUIRED");
    let sidecar_orders_live_enabled = env_bool_default_false("PLOY_SIDECAR_ORDERS_LIVE_ENABLED");
    let openclaw_mode = std::env::var("PLOY_AGENT_FRAMEWORK_MODE")
        .map(|v| v.trim().eq_ignore_ascii_case("openclaw"))
        .unwrap_or(false);
    let internal_agents_hard_disabled =
        env_bool_default_false("PLOY_AGENT_FRAMEWORK_HARD_DISABLE_INTERNAL_AGENTS")
            || env_bool_default_false("PLOY_OPENCLAW_ONLY");

    let flags = CapabilityFlags {
        coordinator_present,
        coordinator_only_live_execution: true,
        governance_api_available: governance_available,
        deployment_gate_required,
        strategy_lifecycle_gate_required,
        sidecar_auth_required,
        sidecar_orders_live_enabled,
        openclaw_mode,
        internal_agents_hard_disabled,
    };

    let architecture_mode = if openclaw_mode {
        "openclaw_control_plane".to_string()
    } else {
        "internal_control_plane".to_string()
    };

    Json(CapabilityResponse {
        architecture_mode,
        canonical_live_ingress: "/api/sidecar/intents".to_string(),
        flags,
        endpoints: capability_endpoints(governance_available),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_endpoints_includes_governance_only_when_available() {
        let with_governance = capability_endpoints(true);
        assert!(with_governance
            .iter()
            .any(|ep| ep.path.contains("/api/governance/status")));

        let without_governance = capability_endpoints(false);
        assert!(!without_governance
            .iter()
            .any(|ep| ep.path.contains("/api/governance/status")));
    }
}
