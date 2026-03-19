use axum::{extract::State, http::HeaderMap, http::StatusCode, Json};
use serde::Deserialize;
use sqlx::{postgres::Postgres, QueryBuilder, Row};
use std::collections::{BTreeSet, HashMap};

use crate::account::{AccountBudgetSnapshot, AccountRegistryEntry, AccountService};
use crate::api::{
    auth::ensure_admin_authorized,
    state::{AppState, SystemRunStatus},
    types::*,
};
use crate::domain::Domain;

#[derive(Debug, Deserialize)]
pub struct DomainControlRequest {
    pub domain: Option<String>,
}

fn parse_domain_control_request(
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Option<Domain>, (StatusCode, String)> {
    let Some(Json(r)) = req else {
        return Ok(None);
    };
    let Some(raw) = r.domain.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    Domain::parse_optional(Some(raw), Domain::Crypto)
        .map(Some)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "invalid domain '{}', expected crypto|sports|politics|economics|custom:<id>",
                    raw
                ),
            )
        })
}

fn domain_label(domain: Domain) -> String {
    match domain {
        Domain::Crypto => "crypto".to_string(),
        Domain::Sports => "sports".to_string(),
        Domain::Politics => "politics".to_string(),
        Domain::Economics => "economics".to_string(),
        Domain::Custom(id) => format!("custom:{}", id),
    }
}

fn summarize_deployment_states<'a>(
    deployments: impl IntoIterator<Item = &'a crate::platform::StrategyDeployment>,
) -> DeploymentStateSummary {
    let mut summary = DeploymentStateSummary {
        enabled: 0,
        draining: 0,
        disabled: 0,
        archived: 0,
    };

    for deployment in deployments {
        match deployment.effective_state() {
            DeploymentState::Enabled => summary.enabled += 1,
            DeploymentState::Draining => summary.draining += 1,
            DeploymentState::Disabled => summary.disabled += 1,
            DeploymentState::Archived => summary.archived += 1,
        }
    }

    summary
}

fn summarize_available_plugins(registry: &PluginRegistry) -> Vec<PluginCapabilitySummary> {
    let mut plugins = registry
        .definitions()
        .iter()
        .map(|definition| PluginCapabilitySummary {
            plugin_id: definition.plugin_id.clone(),
            kind: definition.kind.to_string(),
            version: definition.version.clone(),
            domain: domain_label(definition.domain),
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
    plugins
}

fn summarize_account_budget(budget: &AccountBudgetSnapshot) -> AccountBudgetSummary {
    AccountBudgetSummary {
        available_notional_usd: budget.available_notional_usd.normalize().to_string(),
        reserved_notional_usd: budget.reserved_notional_usd.normalize().to_string(),
    }
}

fn build_platform_capabilities_response(
    account_id: &str,
    runtime_mode: &str,
    dry_run: bool,
    coordinator_running: bool,
    mut active_domains: BTreeSet<String>,
    deployments: &[crate::platform::StrategyDeployment],
    registry: &PluginRegistry,
) -> PlatformCapabilities {
    let mut by_domain: HashMap<String, usize> = HashMap::new();
    let mut enabled = 0usize;
    let mut scoped_total = 0usize;
    let mut scoped_enabled = 0usize;

    for deployment in deployments {
        let effective_state = deployment.effective_state();
        let runtime_active = matches!(
            effective_state,
            DeploymentState::Enabled | DeploymentState::Draining
        );
        let in_scope =
            deployment.matches_account(account_id) && deployment.matches_execution_mode(dry_run);

        if runtime_active {
            enabled += 1;
        }
        if in_scope {
            scoped_total += 1;
            if runtime_active {
                scoped_enabled += 1;
                active_domains.insert(domain_label(deployment.domain));
            }
        }
        *by_domain.entry(domain_label(deployment.domain)).or_insert(0) += 1;
    }

    let mut supported_domains = vec![
        "crypto".to_string(),
        "sports".to_string(),
        "politics".to_string(),
        "economics".to_string(),
    ];
    if by_domain.keys().any(|k| k.starts_with("custom:")) {
        supported_domains.push("custom".to_string());
    }

    let deployment_states = summarize_deployment_states(deployments.iter());
    let scoped_deployment_states = summarize_deployment_states(
        deployments
            .iter()
            .filter(|deployment| {
                deployment.matches_account(account_id) && deployment.matches_execution_mode(dry_run)
            }),
    );

    PlatformCapabilities {
        account_id: account_id.to_string(),
        runtime_mode: runtime_mode.to_string(),
        execution_plane: "coordinator".to_string(),
        dry_run,
        coordinator_running,
        supported_domains,
        active_domains: active_domains.into_iter().collect(),
        total_deployments: deployments.len(),
        enabled_deployments: enabled,
        scoped_total_deployments: scoped_total,
        scoped_enabled_deployments: scoped_enabled,
        deployment_states,
        scoped_deployment_states,
        deployments_by_domain: by_domain,
        available_plugins: summarize_available_plugins(registry),
        system_controls: vec![
            "pause_all".to_string(),
            "resume_all".to_string(),
            "halt_all".to_string(),
            "pause_domain".to_string(),
            "resume_domain".to_string(),
            "halt_domain".to_string(),
            "deployment_gate".to_string(),
        ],
    }
}

fn build_accounts_overview(
    runtime_account: &str,
    dry_run: bool,
    registry_rows: Vec<AccountRegistryEntry>,
    deployments: Vec<crate::platform::StrategyDeployment>,
    budget: AccountBudgetSnapshot,
) -> AccountsOverview {
    let service = AccountService::new(registry_rows, deployments.clone(), budget.clone());
    let accounts = service
        .accounts_overview(runtime_account)
        .into_iter()
        .map(|account| AccountRuntimeSummary {
            deployment_states: summarize_deployment_states(
                deployments
                    .iter()
                    .filter(|deployment| deployment.matches_account(account.account_id.as_str())),
            ),
            account_id: account.account_id,
            wallet_address: account.wallet_address,
            label: account.label,
            runtime_active: account.runtime_active,
            deployment_total: account.deployment_total,
            deployment_enabled: account.deployment_enabled,
        })
        .collect();

    AccountsOverview {
        runtime_account_id: runtime_account.trim().to_string(),
        dry_run,
        runtime_budget: summarize_account_budget(&budget),
        accounts,
    }
}

/// GET /health -- lightweight liveness/readiness probe
pub async fn health_handler(
    State(state): State<AppState>,
) -> std::result::Result<Json<HealthResponse>, (StatusCode, Json<HealthResponse>)> {
    let db_status = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(state.store.pool())
        .await
    {
        Ok(_) => "connected".to_string(),
        Err(_) => "disconnected".to_string(),
    };

    let ok = db_status == "connected";
    let resp = HealthResponse {
        status: if ok {
            "ok".to_string()
        } else {
            "degraded".to_string()
        },
        db: db_status,
        uptime_secs: state.uptime_seconds(),
    };

    if ok {
        Ok(Json(resp))
    } else {
        Err((StatusCode::SERVICE_UNAVAILABLE, Json(resp)))
    }
}

/// GET /api/system/status
pub async fn get_system_status(
    State(state): State<AppState>,
) -> std::result::Result<Json<SystemStatus>, (StatusCode, String)> {
    let status_state = state.system_status.read().await;

    // Get error count from last hour
    let error_count = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)::BIGINT as count
        FROM security_audit_log
        WHERE timestamp > NOW() - INTERVAL '1 hour'
          AND severity IN ('HIGH', 'CRITICAL')
        "#,
    )
    .fetch_one(state.store.pool())
    .await
    .unwrap_or(0);

    Ok(Json(SystemStatus {
        status: status_state.status.as_str().to_string(),
        uptime_seconds: state.uptime_seconds(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        strategy: "coordinator".to_string(),
        last_trade_time: status_state.last_trade_time,
        websocket_connected: status_state.websocket_connected,
        database_connected: status_state.database_connected,
        error_count_1h: error_count,
    }))
}

/// GET /api/system/capabilities
///
/// Execution-plane capabilities for architecture/runtime introspection.
pub async fn get_platform_capabilities(
    State(state): State<AppState>,
) -> std::result::Result<Json<PlatformCapabilities>, (StatusCode, String)> {
    let coordinator_running = state.coordinator.is_some();

    let mut active_domains: BTreeSet<String> = BTreeSet::new();
    if let Some(coordinator) = state.coordinator.as_ref() {
        let global = coordinator.read_state().await;
        for snapshot in global.agents.values() {
            active_domains.insert(domain_label(snapshot.domain));
        }
    }

    let deployments = state.deployments.read().await;
    let registry = PluginRegistry::builtin_runtime_registry()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(build_platform_capabilities_response(
        state.account_id.as_str(),
        state.runtime_mode.as_str(),
        state.dry_run,
        coordinator_running,
        active_domains,
        &deployments.values().cloned().collect::<Vec<_>>(),
        &registry,
    )))
}

/// GET /api/system/accounts
///
/// Account registry and deployment coverage overview.
pub async fn get_system_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<AccountsOverview>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;

    let runtime_account = state.account_id.trim().to_string();

    let mut registry_rows: Vec<AccountRegistryEntry> = Vec::new();
    if let Ok(rows) = sqlx::query(
        r#"
        SELECT account_id, wallet_address, label
        FROM accounts
        ORDER BY account_id ASC
        "#,
    )
    .fetch_all(state.store.pool())
    .await
    {
        for row in rows {
            let account_id: String = row.try_get("account_id").unwrap_or_default();
            let account_id = account_id.trim().to_string();
            if account_id.is_empty() {
                continue;
            }
            registry_rows.push(AccountRegistryEntry {
                account_id,
                wallet_address: row.try_get("wallet_address").ok(),
                label: row.try_get("label").ok(),
            });
        }
    }

    let deployments = state.deployments.read().await;
    let budget = AccountBudgetSnapshot::default();

    Ok(Json(build_accounts_overview(
        runtime_account.as_str(),
        state.dry_run,
        registry_rows,
        deployments.values().cloned().collect(),
        budget,
    )))
}

/// POST /api/system/start
pub async fn start_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    coordinator
        .resume_all()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Running;
    }

    // Broadcast status update
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "running".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_START', 'LOW', 'System started via API', '{}')
        "#,
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已启动".to_string(),
    }))
}

/// POST /api/system/stop
pub async fn stop_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    coordinator
        .pause_all()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Stopped;
    }

    // Broadcast status update
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_STOP', 'LOW', 'System stopped via API', '{}')
        "#,
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已停止".to_string(),
    }))
}

/// POST /api/system/restart
pub async fn restart_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };

    coordinator
        .pause_all()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Stopped;
    }
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    // Wait a moment
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    coordinator
        .resume_all()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Running;
    }
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "running".to_string(),
    }));

    // Log to audit
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('SYSTEM_RESTART', 'MEDIUM', 'System restarted via API', '{}')
        "#,
    )
    .execute(state.store.pool())
    .await;

    Ok(Json(SystemControlResponse {
        success: true,
        message: "系统已重启".to_string(),
    }))
}

/// POST /api/system/pause
pub async fn pause_system(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let domain = parse_domain_control_request(req)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    if let Some(domain) = domain {
        coordinator
            .pause_domain(domain)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        coordinator
            .pause_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let mut status_state = state.system_status.write().await;
    status_state.status = SystemRunStatus::Stopped;
    drop(status_state);
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已暂停".to_string(),
    }))
}

/// POST /api/system/resume
pub async fn resume_system(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let domain = parse_domain_control_request(req)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    if let Some(domain) = domain {
        coordinator
            .resume_domain(domain)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        coordinator
            .resume_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let mut status_state = state.system_status.write().await;
    status_state.status = SystemRunStatus::Running;
    drop(status_state);
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "running".to_string(),
    }));

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已恢复".to_string(),
    }))
}

/// POST /api/system/halt
///
/// Force-close all positions and mark the system as stopped.
pub async fn halt_system(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Option<Json<DomainControlRequest>>,
) -> std::result::Result<Json<SystemControlResponse>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let domain = parse_domain_control_request(req)?;
    let Some(coordinator) = state.coordinator.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "coordinator unavailable in this runtime".to_string(),
        ));
    };
    if let Some(domain) = domain {
        coordinator
            .force_close_domain(domain)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        coordinator
            .force_close_all()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Update system status and broadcast
    {
        let mut status_state = state.system_status.write().await;
        status_state.status = SystemRunStatus::Stopped;
    }
    state.broadcast(WsMessage::Status(StatusUpdate {
        status: "stopped".to_string(),
    }));

    Ok(Json(SystemControlResponse {
        success: true,
        message: "已紧急停止并强制平仓".to_string(),
    }))
}

/// GET /api/config
pub async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Json<StrategyConfig>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let config = state.config.read().await;

    Ok(Json(StrategyConfig {
        symbols: config.symbols.clone(),
        min_move: config.min_move,
        max_entry: config.max_entry,
        shares: config.shares,
        predictive: config.predictive,
        exit_edge_floor: config.exit_edge_floor,
        exit_price_band: config.exit_price_band,
        time_decay_exit_secs: config.time_decay_exit_secs,
        liquidity_exit_spread_bps: config.liquidity_exit_spread_bps,
    }))
}

/// PUT /api/config
pub async fn update_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(new_config): Json<StrategyConfig>,
) -> std::result::Result<Json<serde_json::Value>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let mut config = state.config.write().await;

    // Update config
    config.symbols = new_config.symbols;
    config.min_move = new_config.min_move;
    config.max_entry = new_config.max_entry;
    config.shares = new_config.shares;
    config.predictive = new_config.predictive;
    config.exit_edge_floor = new_config.exit_edge_floor;
    config.exit_price_band = new_config.exit_price_band;
    config.time_decay_exit_secs = new_config.time_decay_exit_secs;
    config.liquidity_exit_spread_bps = new_config.liquidity_exit_spread_bps;

    // Log to audit
    let _ = sqlx::query(
        r#"
        INSERT INTO security_audit_log (event_type, severity, details, metadata)
        VALUES ('CONFIG_UPDATE', 'MEDIUM', 'Strategy config updated via API', $1)
        "#,
    )
    .bind(serde_json::to_value(&*config).unwrap_or_default())
    .execute(state.store.pool())
    .await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// GET /api/security/events
pub async fn get_security_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<SecurityEventQuery>,
) -> std::result::Result<Json<Vec<SecurityEvent>>, (StatusCode, String)> {
    ensure_admin_authorized(&headers)?;
    let limit = query.limit.unwrap_or(100).min(500);

    let mut qb = QueryBuilder::<Postgres>::new(
        r#"
        SELECT
            id,
            timestamp,
            event_type,
            severity,
            details,
            metadata
        FROM security_audit_log
        WHERE 1=1
        "#,
    );

    if let Some(ref severity) = query.severity {
        qb.push(" AND severity = ").push_bind(severity);
    }
    if let Some(start_time) = query.start_time {
        qb.push(" AND timestamp >= ").push_bind(start_time);
    }
    qb.push(" ORDER BY timestamp DESC LIMIT ").push_bind(limit);

    let rows = qb
        .build()
        .fetch_all(state.store.pool())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut security_events = Vec::with_capacity(rows.len());
    for row in rows {
        let id = if let Ok(v) = row.try_get::<uuid::Uuid, _>("id") {
            v.to_string()
        } else if let Ok(v) = row.try_get::<i64, _>("id") {
            v.to_string()
        } else if let Ok(v) = row.try_get::<String, _>("id") {
            v
        } else {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "unsupported security_audit_log.id type".to_string(),
            ));
        };
        let timestamp = row
            .try_get("timestamp")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let event_type = row
            .try_get("event_type")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let severity = row
            .try_get("severity")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let details = row
            .try_get("details")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let metadata = row.try_get("metadata").ok();
        security_events.push(SecurityEvent {
            id,
            timestamp,
            event_type,
            severity,
            details,
            metadata,
        });
    }

    Ok(Json(security_events))
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use crate::account::{AccountBudgetSnapshot, AccountRegistryEntry};
    use crate::plugins::{DeploymentState, PluginRegistry};
    use crate::platform::{
        DeploymentExecutionMode, Domain, MarketSelector, StrategyDeployment,
        StrategyLifecycleStage, StrategyProductType, Timeframe,
    };

    use super::{build_accounts_overview, build_platform_capabilities_response};

    fn sample_deployment(
        id: &str,
        enabled: bool,
        state: DeploymentState,
        domain: Domain,
        account_ids: Vec<&str>,
    ) -> StrategyDeployment {
        StrategyDeployment {
            id: id.to_string(),
            strategy: "momentum".to_string(),
            strategy_version: "v1".to_string(),
            domain,
            market_selector: MarketSelector::Static {
                symbol: Some("BTCUSDT".to_string()),
                series_id: None,
                market_slug: None,
            },
            timeframe: Timeframe::M5,
            enabled,
            state,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 0,
            cooldown_secs: 60,
            account_ids: account_ids.into_iter().map(str::to_string).collect(),
            execution_mode: DeploymentExecutionMode::Any,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn platform_capabilities_expose_plugin_visibility_and_deployment_states() {
        let registry = PluginRegistry::builtin_runtime_registry().expect("builtin plugin registry");
        let deployments = vec![
            sample_deployment(
                "deploy.crypto.momentum.1",
                true,
                DeploymentState::Enabled,
                Domain::Crypto,
                vec!["tango"],
            ),
            sample_deployment(
                "deploy.crypto.momentum.2",
                true,
                DeploymentState::Draining,
                Domain::Crypto,
                vec!["tango"],
            ),
            sample_deployment(
                "deploy.sports.nba.1",
                false,
                DeploymentState::Disabled,
                Domain::Sports,
                vec!["other"],
            ),
        ];

        let capabilities = build_platform_capabilities_response(
            "tango",
            "platform",
            false,
            true,
            ["crypto".to_string()].into_iter().collect(),
            &deployments,
            &registry,
        );

        assert_eq!(capabilities.deployment_states.enabled, 1);
        assert_eq!(capabilities.deployment_states.draining, 1);
        assert_eq!(capabilities.deployment_states.disabled, 1);
        assert_eq!(capabilities.scoped_deployment_states.draining, 1);
        assert_eq!(capabilities.scoped_deployment_states.disabled, 0);
        assert!(capabilities
            .available_plugins
            .iter()
            .any(|plugin| plugin.plugin_id == "crypto.momentum.v1"));
    }

    #[test]
    fn account_overview_includes_state_counts_and_runtime_budget_snapshot() {
        let accounts = build_accounts_overview(
            "tango",
            false,
            vec![
                AccountRegistryEntry {
                    account_id: "tango".to_string(),
                    wallet_address: Some("0xabc".to_string()),
                    label: Some("Main".to_string()),
                },
                AccountRegistryEntry {
                    account_id: "other".to_string(),
                    wallet_address: Some("0xdef".to_string()),
                    label: Some("Alt".to_string()),
                },
            ],
            vec![
                sample_deployment(
                    "deploy.crypto.enabled",
                    true,
                    DeploymentState::Enabled,
                    Domain::Crypto,
                    vec!["tango"],
                ),
                sample_deployment(
                    "deploy.crypto.draining",
                    true,
                    DeploymentState::Draining,
                    Domain::Crypto,
                    vec!["tango"],
                ),
                sample_deployment(
                    "deploy.crypto.disabled",
                    false,
                    DeploymentState::Disabled,
                    Domain::Crypto,
                    vec!["tango"],
                ),
            ],
            AccountBudgetSnapshot {
                available_notional_usd: Decimal::new(900, 0),
                reserved_notional_usd: Decimal::new(100, 0),
            },
        );

        assert_eq!(accounts.runtime_budget.available_notional_usd, "900");
        assert_eq!(accounts.runtime_budget.reserved_notional_usd, "100");

        let runtime = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == "tango")
            .expect("runtime account row");
        assert!(runtime.runtime_active);
        assert_eq!(runtime.deployment_states.enabled, 1);
        assert_eq!(runtime.deployment_states.draining, 1);
        assert_eq!(runtime.deployment_states.disabled, 1);
    }

    #[test]
    fn draining_deployments_are_reported_as_active_not_disabled() {
        let accounts = build_accounts_overview(
            "tango",
            false,
            vec![AccountRegistryEntry {
                account_id: "tango".to_string(),
                wallet_address: None,
                label: Some("Main".to_string()),
            }],
            vec![sample_deployment(
                "deploy.crypto.draining",
                true,
                DeploymentState::Draining,
                Domain::Crypto,
                vec!["tango"],
            )],
            AccountBudgetSnapshot::default(),
        );

        let runtime = accounts
            .accounts
            .iter()
            .find(|account| account.account_id == "tango")
            .expect("runtime account row");
        assert_eq!(runtime.deployment_enabled, 1);
        assert_eq!(runtime.deployment_states.draining, 1);
        assert_eq!(runtime.deployment_states.disabled, 0);
    }
}
