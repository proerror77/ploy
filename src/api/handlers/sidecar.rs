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
#[cfg(test)]
mod tests;
mod types;
mod write_side;

pub use grok_decision::sidecar_grok_decision;
use ingress::{
    apply_deployment_metadata, broadcast_sidecar_activity, clamp_external_priority,
    deployment_default_priority, ensure_agent_authorized, ensure_domain_allowed,
    map_coordinator_submit_error, parse_binary_side, parse_is_buy, parse_order_priority,
    parse_sidecar_domain, resolve_intent_deployment, resolve_request_account_scope,
    sidecar_orders_live_enabled, table_has_account_scope, validate_account_scope,
    validate_deployment_binding,
};
pub use read_side::{sidecar_get_positions, sidecar_get_risk};
pub use types::{
    SidecarCircuitBreakerEvent, SidecarIntentRequest, SidecarIntentResponse, SidecarOrderRequest,
    SidecarOrderResponse, SidecarPosition, SidecarRiskPosition, SidecarRiskState,
};
pub use write_side::{sidecar_submit_intent, sidecar_submit_order};
