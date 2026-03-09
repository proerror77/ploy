//! Platform Bootstrap — wires up Coordinator + Agents from config
//!
//! Entry point for `ploy platform start`. Creates shared infrastructure,
//! registers agents based on config flags, and runs the coordinator loop.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, trace, warn};

use crate::adapters::polymarket_clob::POLYGON_CHAIN_ID;
use crate::adapters::{BinanceWebSocket, PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::config::AppConfig;
use crate::control_plane::StrategyDeployment;
use crate::coordinator::{Coordinator, CoordinatorHandle, GlobalState};
use crate::domain::Side;
use crate::error::Result;
use crate::exchange::{build_exchange_client, parse_exchange_kind, ExchangeKind};
use crate::platform::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence, spawn_polymarket_trade_persistence_from_collector_targets,
    BinanceDataPlaneHandle, DataPlaneConfig, Domain, PlatformDataPlane,
};
use crate::signing::Wallet;
use crate::strategy::executor::OrderExecutor;
use crate::strategy::idempotency::IdempotencyManager;
use crate::strategy::momentum::EventMatcher;
use crate::strategy::DataFeed;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::strategy_runtime::run_managed_strategy_runtime;

mod bootstrap_config;
mod coordinator_bootstrap;
mod crypto_runtime_support;
mod managed_crypto;
mod openclaw_config;
mod runtime_config;
mod runtime_orchestration;
mod runtime_spawns;
mod schema;
mod sports_runtime_support;
mod startup_context;
mod status;
mod strategy_deployments;
mod support;
#[cfg(test)]
mod tests;

pub use self::bootstrap_config::PlatformBootstrapConfig;
use self::coordinator_bootstrap::initialize_coordinator_runtime;
use self::crypto_runtime_support::initialize_crypto_runtime_support;
pub use self::openclaw_config::{AllocatorConfig, OpenClawConfig, RegimeConfig, StraddleConfig};
use self::runtime_orchestration::run_platform_runtime;
use self::runtime_spawns::{spawn_managed_strategy_runtime_task, spawn_openclaw_governance_agent};
use self::schema::{
    ensure_accounts_table, ensure_binance_lob_ticks_table, ensure_binance_price_ticks_table,
    ensure_clob_quote_ticks_table, ensure_schema_repairs, upsert_account_from_config,
};
pub(crate) use self::schema::{
    ensure_agent_order_executions_table, ensure_clob_orderbook_snapshots_table,
    ensure_coordinator_governance_policies_table,
    ensure_coordinator_governance_policy_history_table, ensure_pm_market_metadata_table,
    ensure_pm_token_settlements_table, ensure_risk_runtime_state_table,
    ensure_strategy_observability_tables,
};
use self::sports_runtime_support::prepare_sports_runtime_support;
use self::startup_context::initialize_startup_context;
pub use self::status::print_platform_status;
#[cfg(all(test, feature = "rl"))]
use self::strategy_deployments::build_crypto_rl_policy_runtime_config;
#[cfg(test)]
use self::strategy_deployments::{
    build_crypto_lob_ml_runtime_config, build_event_edge_runtime_config,
    build_momentum_runtime_config, build_nba_comeback_runtime_config,
    build_split_arb_runtime_config,
};
use self::strategy_deployments::{
    collect_managed_strategy_runtime_plans, collect_runtime_crypto_strategy_targets,
    ManagedRuntimeBootstrapStep, ManagedRuntimeDataPlaneKind,
};
use self::support::{env_bool, env_i64, env_u64, env_usize, lob_levels_json};

const CLOB_PERSIST_MIN_INTERVAL_SECS: i64 = 2;
const BINANCE_PERSIST_MIN_INTERVAL_SECS: i64 = 1;
const PM_COLLECTOR_REFRESH_SECS: u64 = 300;

/// Optional control commands to apply immediately after platform startup.
#[derive(Debug, Clone, Default)]
pub struct PlatformStartControl {
    pub pause: Option<String>,
    pub resume: Option<String>,
}

/// Start the multi-agent platform
///
/// Creates shared infrastructure, registers configured agents,
/// and runs the coordinator loop until shutdown.
pub async fn start_platform(
    config: PlatformBootstrapConfig,
    app_config: &AppConfig,
    control: PlatformStartControl,
) -> Result<()> {
    let startup = initialize_startup_context(&config, app_config).await?;
    let exchange_client = startup.exchange_client;
    let pm_client = startup.pm_client;
    let account_id = startup.account_id;
    let runtime_crypto_targets = startup.runtime_crypto_targets;
    let allowed_domains = startup.allowed_domains;
    let shared_pool = startup.shared_pool;

    let coordinator_bootstrap = initialize_coordinator_runtime(
        &config,
        &app_config,
        exchange_client.clone(),
        &account_id,
        allowed_domains.clone(),
        shared_pool.as_ref(),
    )
    .await?;
    let coordinator = coordinator_bootstrap.coordinator;
    let handle = coordinator_bootstrap.handle;
    let _api_handle = coordinator_bootstrap.api_handle;
    run_platform_runtime(
        &config,
        app_config,
        &control,
        coordinator,
        handle,
        pm_client,
        shared_pool,
        account_id,
        &runtime_crypto_targets,
    )
    .await
}
